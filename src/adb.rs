use crate::config::{AppConfig, DeviceStatus, SignalSource};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x08000000;

struct PersistentShell {
    child: Child,
    stdin: ChildStdin,
}

pub struct DeviceController {
    adb_path: PathBuf,
    assets_dir: PathBuf,
    cfg: Arc<Mutex<AppConfig>>,
    shell: Mutex<Option<PersistentShell>>,
    jars_verified: Mutex<bool>,
}

impl DeviceController {
    pub fn new(adb_path: PathBuf, assets_dir: PathBuf, cfg: Arc<Mutex<AppConfig>>) -> Self {
        Self {
            adb_path,
            assets_dir,
            cfg,
            shell: Mutex::new(None),
            jars_verified: Mutex::new(false),
        }
    }

    pub fn reset_jar_cache(&self) {
        if let Ok(mut g) = self.jars_verified.lock() {
            *g = false;
        }
    }

    fn tv_address(&self) -> String {
        self.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default()
    }

    pub fn run_adb(&self, args: &[&str], timeout_ms: u64) -> (i32, String, String) {
        if !self.adb_path.exists() {
            return (-1, String::new(), "adb.exe not found".to_string());
        }
        let mut child = match Command::new(&self.adb_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return (-1, String::new(), e.to_string()),
        };

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(ref mut s) = stdout {
                let _ = std::io::Read::read_to_end(s, &mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        });
        let err_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(ref mut s) = stderr {
                let _ = std::io::Read::read_to_end(s, &mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        });

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(-1),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break -1;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(_) => break -1,
            }
        };

        let out_str = out_handle.join().unwrap_or_default();
        let err_str = err_handle.join().unwrap_or_default();
        (code, out_str, err_str)
    }

    pub fn shell(&self, cmd: &str, timeout_ms: u64) -> (i32, String, String) {
        let addr = self.tv_address();
        self.run_adb(&["-s", &addr, "shell", cmd], timeout_ms)
    }

    pub fn test_connect(&self, addr: &str) -> bool {
        let (_, out, err) = self.run_adb(&["connect", addr], 3500);
        let combined = format!("{} {}", out, err).to_ascii_lowercase();
        if combined.contains("connected to") || combined.contains("already connected") {
            let (_, st, _) = self.run_adb(&["-s", addr, "get-state"], 2500);
            return st.trim() == "device";
        }
        false
    }

    pub fn ensure_connected(&self) -> bool {
        let addr = self.tv_address();
        self.test_connect(&addr)
    }

    pub fn ensure_jars_deployed(&self) {
        if self.jars_verified.lock().map(|g| *g).unwrap_or(false) {
            return;
        }
        let (_, out, _) = self.shell(
            "test -f /data/local/tmp/MtkDirectTool.jar && test -f /data/local/tmp/VolDirectTool.jar && echo OK",
            2500,
        );
        if out.trim() == "OK" {
            if let Ok(mut g) = self.jars_verified.lock() {
                *g = true;
            }
            return;
        }
        let addr = self.tv_address();
        let mtk = self.assets_dir.join("MtkDirectTool.jar");
        let vol = self.assets_dir.join("VolDirectTool.jar");
        if mtk.exists() {
            if let Some(s) = mtk.to_str() {
                let _ = self.run_adb(&["-s", &addr, "push", s, "/data/local/tmp/MtkDirectTool.jar"], 6000);
            }
        }
        if vol.exists() {
            if let Some(s) = vol.to_str() {
                let _ = self.run_adb(&["-s", &addr, "push", s, "/data/local/tmp/VolDirectTool.jar"], 6000);
            }
        }
        let _ = self.shell(
            "chmod 644 /data/local/tmp/MtkDirectTool.jar /data/local/tmp/VolDirectTool.jar 2>/dev/null",
            2500,
        );
        if let Ok(mut g) = self.jars_verified.lock() {
            *g = true;
        }
    }

    pub fn query_status(&self) -> DeviceStatus {
        let mut st = DeviceStatus::default();
        if !self.ensure_connected() {
            return st;
        }
        st.connected = true;
        self.ensure_jars_deployed();

        let cmd = "echo SRC=$(settings get global mitv.tvplayer.hdmi.last.source); \
                   echo FOC=$(dumpsys window | grep -E 'mCurrentFocus|mFocusedApp' | head -n 2 | tr '\\n' ' '); \
                   echo BL=$(su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool get g_disp__disp_back_light' 2>/dev/null | grep -oE '[0-9]+$' | tail -n 1); \
                   echo VOL=$(cmd media_session volume --stream 3 --get 2>/dev/null | grep -oE 'volume is [0-9]+' | grep -oE '[0-9]+')";

        let (_, out, _) = self.shell(cmd, 4500);
        let mut is_ext = false;
        let mut last_src: i32 = -1;

        for raw in out.lines() {
            let line = raw.trim();
            if let Some(v) = line.strip_prefix("SRC=") {
                if let Ok(n) = v.trim().parse::<i32>() {
                    last_src = n;
                }
            } else if let Some(v) = line.strip_prefix("FOC=") {
                is_ext = v.to_ascii_lowercase().contains("externalsourceactivity");
            } else if let Some(v) = line.strip_prefix("BL=") {
                if let Ok(n) = v.trim().parse::<i32>() {
                    if (0..=100).contains(&n) {
                        st.backlight = Some(n);
                    }
                }
            } else if let Some(v) = line.strip_prefix("VOL=") {
                if let Ok(n) = v.trim().parse::<i32>() {
                    if (0..=100).contains(&n) {
                        st.volume = Some(n);
                    }
                }
            }
        }

        let (dp, h1, h2) = self
            .cfg
            .lock()
            .map(|c| (c.dp_input, c.hdmi1_input, c.hdmi2_input))
            .unwrap_or((29, 23, 24));

        st.active_source = if !is_ext {
            SignalSource::TvSystem
        } else if last_src == dp {
            SignalSource::Dp
        } else if last_src == h1 {
            SignalSource::Hdmi1
        } else if last_src == h2 {
            SignalSource::Hdmi2
        } else {
            SignalSource::Dp
        };

        st
    }

    pub fn switch_to_tv_desktop(&self) -> bool {
        if !self.ensure_connected() {
            return false;
        }
        let _ = self.shell("cmd input keyevent 3", 3500);
        true
    }

    pub fn switch_to_external(&self, input_id: i32) -> bool {
        if !self.ensure_connected() {
            return false;
        }
        let cmd = format!(
            "am start -a com.xiaomi.mitv.tvplayer.EXTSRC_PLAY -n com.xiaomi.mitv.tvplayer/.ExternalSourceActivity --ei input {} -f 0x10000000",
            input_id
        );
        let _ = self.shell(&cmd, 3500);
        true
    }

    pub fn set_backlight(&self, val: i32) -> bool {
        let v = val.clamp(0, 100);
        if !self.ensure_connected() {
            return false;
        }
        self.ensure_jars_deployed();
        let cmd = format!(
            "su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool set g_disp__disp_back_light {0} 3' >/dev/null 2>&1; \
             am broadcast -a com.xiaomi.mitv.action.PIC_MODE_CHANGED --ei picmode 7 >/dev/null 2>&1; \
             settings put global picture_backlight {0}; settings put global xiaomi_picture_backlight {0}",
            v
        );
        let (code, _, _) = self.shell(&cmd, 4000);
        code == 0
    }

    pub fn set_volume(&self, val: i32) -> bool {
        let v = val.clamp(0, 100);
        if !self.ensure_connected() {
            return false;
        }
        self.ensure_jars_deployed();
        let cmd = format!(
            "su 1000 -c 'CLASSPATH=/data/local/tmp/VolDirectTool.jar /system/bin/app_process /data/local/tmp VolDirectTool set 3 {}' >/dev/null 2>&1",
            v
        );
        let (code, _, _) = self.shell(&cmd, 3500);
        code == 0
    }

    pub fn start_persistent_shell(&self) {
        let mut guard = match self.shell.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(ref mut ps) = *guard {
            if let Ok(None) = ps.child.try_wait() {
                return;
            }
        }
        *guard = None;
        let addr = self.tv_address();
        if let Ok(mut child) = Command::new(&self.adb_path)
            .args(["-s", &addr, "shell"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            if let Some(stdin) = child.stdin.take() {
                *guard = Some(PersistentShell { child, stdin });
            }
        }
    }

    pub fn stop_persistent_shell(&self) {
        if let Ok(mut guard) = self.shell.lock() {
            if let Some(mut ps) = guard.take() {
                drop(ps.stdin);
                let _ = ps.child.kill();
                let _ = ps.child.wait();
            }
        }
    }

    pub fn send_tv_key_fast(self: &Arc<Self>, keycode: i32) {
        let dev = Arc::clone(self);
        std::thread::spawn(move || {
            let mut sent = false;
            if let Ok(mut guard) = dev.shell.lock() {
                let alive = match guard.as_mut() {
                    Some(ps) => matches!(ps.child.try_wait(), Ok(None)),
                    None => false,
                };
                if !alive {
                    *guard = None;
                    let addr = dev.tv_address();
                    if let Ok(mut child) = Command::new(&dev.adb_path)
                        .args(["-s", &addr, "shell"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn()
                    {
                        if let Some(stdin) = child.stdin.take() {
                            *guard = Some(PersistentShell { child, stdin });
                        }
                    }
                }
                if let Some(ps) = guard.as_mut() {
                    let line = format!("cmd input keyevent {}\n", keycode);
                    if ps.stdin.write_all(line.as_bytes()).is_ok() && ps.stdin.flush().is_ok() {
                        sent = true;
                    } else {
                        let _ = ps.child.kill();
                        *guard = None;
                    }
                }
            }
            if !sent {
                let _ = dev.shell(&format!("cmd input keyevent {}", keycode), 2000);
            }
        });
    }
}
