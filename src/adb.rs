use crate::config::{AppConfig, DeviceStatus, SignalSource};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const DAEMON_IDLE_SECS: u64 = 25;

struct PersistentShell {
    child: Child,
    stdin: ChildStdin,
}

struct DaemonProc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl DaemonProc {
    fn send_cmd(&mut self, cmd: &str) -> Option<String> {
        if self.stdin.write_all(cmd.as_bytes()).is_err()
            || self.stdin.write_all(b"\n").is_err()
            || self.stdin.flush().is_err()
        {
            return None;
        }
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(n) if n > 0 => Some(line.trim().to_string()),
            _ => None,
        }
    }

    fn shutdown(mut self) {
        let _ = self.stdin.write_all(b"Q\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        let deadline = Instant::now() + Duration::from_millis(350);
        loop {
            if !matches!(self.child.try_wait(), Ok(None)) {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

#[derive(Default)]
struct SliderQueue {
    backlight: Option<i32>,
    volume: Option<i32>,
    sync_backlight: Option<i32>,
    warm: bool,
    step_bl_delta: i32,
    stop_daemon: bool,
}

pub struct DeviceController {
    adb_path: PathBuf,
    assets_dir: PathBuf,
    cfg: Arc<Mutex<AppConfig>>,
    shell: Mutex<Option<PersistentShell>>,
    daemon: Mutex<Option<DaemonProc>>,
    jars_verified: Mutex<bool>,
    keep_warm: AtomicBool,
    cached_bl: AtomicI32,
    cached_vol: AtomicI32,
    queue: Mutex<SliderQueue>,
    cv: Condvar,
    worker_started: AtomicBool,
}

impl DeviceController {
    pub fn new(adb_path: PathBuf, assets_dir: PathBuf, cfg: Arc<Mutex<AppConfig>>) -> Self {
        Self {
            adb_path,
            assets_dir,
            cfg,
            shell: Mutex::new(None),
            daemon: Mutex::new(None),
            jars_verified: Mutex::new(false),
            keep_warm: AtomicBool::new(false),
            cached_bl: AtomicI32::new(-1),
            cached_vol: AtomicI32::new(-1),
            queue: Mutex::new(SliderQueue::default()),
            cv: Condvar::new(),
            worker_started: AtomicBool::new(false),
        }
    }

    fn ensure_worker(self: &Arc<Self>) {
        if self.worker_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let dev = Arc::clone(self);
        std::thread::spawn(move || dev.slider_worker_loop());
    }

    pub fn reset_jar_cache(&self) {
        if let Ok(mut g) = self.jars_verified.lock() {
            *g = false;
        }
        self.stop_daemon_now();
    }

    pub fn set_keep_warm(self: &Arc<Self>, warm: bool) {
        self.keep_warm.store(warm, Ordering::Release);
        if warm {
            self.ensure_worker();
            if let Ok(mut q) = self.queue.lock() {
                q.warm = true;
            }
            self.cv.notify_one();
        } else {
            self.cv.notify_one();
        }
    }

    pub fn stop_daemon_now(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.stop_daemon = true;
            q.warm = false;
        }
        self.cv.notify_one();
        if let Ok(mut g) = self.daemon.lock() {
            if let Some(d) = g.take() {
                d.shutdown();
            }
        }
    }

    pub fn queue_backlight(self: &Arc<Self>, val: i32, commit_sync: bool) {
        let v = val.clamp(0, 100);
        self.cached_bl.store(v, Ordering::Relaxed);
        self.ensure_worker();
        if let Ok(mut q) = self.queue.lock() {
            q.backlight = Some(v);
            if commit_sync {
                q.sync_backlight = Some(v);
            }
        }
        self.cv.notify_one();
    }

    pub fn queue_volume(self: &Arc<Self>, val: i32) {
        let v = val.clamp(0, 100);
        self.cached_vol.store(v, Ordering::Relaxed);
        self.ensure_worker();
        if let Ok(mut q) = self.queue.lock() {
            q.volume = Some(v);
        }
        self.cv.notify_one();
    }

    pub fn queue_step_backlight(self: &Arc<Self>, delta: i32) {
        self.ensure_worker();
        if let Ok(mut q) = self.queue.lock() {
            q.step_bl_delta += delta;
        }
        self.cv.notify_one();
    }

    fn tv_address(&self) -> String {
        self.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default()
    }

    fn tv_monitor_hint(&self) -> String {
        self.cfg
            .lock()
            .map(|c| c.tv_monitor_hint.clone())
            .unwrap_or_else(|_| "Mi Monitor".to_string())
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
                    std::thread::sleep(Duration::from_millis(12));
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
        let (_, st, _) = self.run_adb(&["-s", addr, "get-state"], 1500);
        if st.trim() == "device" {
            return true;
        }
        let (_, out, err) = self.run_adb(&["connect", addr], 3500);
        let combined = format!("{} {}", out, err).to_ascii_lowercase();
        if combined.contains("connected to") || combined.contains("already connected") {
            let (_, st2, _) = self.run_adb(&["-s", addr, "get-state"], 2500);
            return st2.trim() == "device";
        }
        false
    }

    pub fn ensure_connected(&self) -> bool {
        let addr = self.tv_address();
        self.test_connect(&addr)
    }

    /// Drops zombie persistent ADB pipes (e.g. after PC sleep/resume or Wi-Fi drop)
    /// so subsequent commands reconnect cleanly without hanging on dead sockets.
    pub fn reset_stale_connections(&self) {
        self.stop_persistent_shell();
        self.reset_jar_cache();
    }

    /// Forces a fresh wireless ADB session: if `get-state` is not `device`, runs `adb disconnect`
    /// first to clear Windows half-open/offline TCP sockets before reconnecting.
    pub fn reconnect_force(&self) -> bool {
        let addr = self.tv_address();
        if addr.is_empty() {
            return false;
        }
        let (_, st, _) = self.run_adb(&["-s", &addr, "get-state"], 1500);
        if st.trim() == "device" {
            let (rc, echo_out, _) = self.run_adb(&["-s", &addr, "shell", "echo ok"], 2000);
            if rc == 0 && echo_out.trim() == "ok" {
                return true;
            }
        }
        let _ = self.run_adb(&["disconnect", &addr], 1500);
        let (_, out, err) = self.run_adb(&["connect", &addr], 3500);
        let combined = format!("{} {}", out, err).to_ascii_lowercase();
        if combined.contains("connected to") || combined.contains("already connected") {
            let (_, st2, _) = self.run_adb(&["-s", &addr, "get-state"], 2000);
            return st2.trim() == "device";
        }
        false
    }

    /// Lightweight probe of whether the TV is currently displaying its built-in Android TV UI
    /// (`Some(true)`), an external source like DP/HDMI (`Some(false)`), or unreachable (`None`).
    pub fn is_tv_on_tv_system(&self) -> Option<bool> {
        let (rc, out, _) = self.shell(
            "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'",
            2500,
        );
        if rc != 0 || out.trim().is_empty() {
            return None;
        }
        Some(!out.to_ascii_lowercase().contains("externalsourceactivity"))
    }

    pub fn ensure_jars_deployed(&self) {
        if self.jars_verified.lock().map(|g| *g).unwrap_or(false) {
            return;
        }
        let local_daemon_len = std::fs::metadata(self.assets_dir.join("MiTVDaemon.jar"))
            .map(|m| m.len())
            .unwrap_or(0);
        let local_edge_len = std::fs::metadata(self.assets_dir.join("MiTVEdge.jar"))
            .map(|m| m.len())
            .unwrap_or(0);
        let check_cmd = format!(
            "test -f /data/local/tmp/MiTVDaemon.jar && test -f /data/local/tmp/MtkDirectTool.jar && test -f /data/local/tmp/VolDirectTool.jar && test -f /data/local/tmp/MiTVEdge.jar && [ \"$(wc -c < /data/local/tmp/MiTVDaemon.jar)\" -eq {} ] && [ \"$(wc -c < /data/local/tmp/MiTVEdge.jar)\" -eq {} ] && echo OK",
            local_daemon_len, local_edge_len
        );
        let (_, out, _) = self.shell(&check_cmd, 2500);
        if out.trim() == "OK" {
            if let Ok(mut g) = self.jars_verified.lock() {
                *g = true;
            }
            return;
        }
        let addr = self.tv_address();
        for name in [
            "MiTVDaemon.jar",
            "MtkDirectTool.jar",
            "VolDirectTool.jar",
            "MiTVEdge.jar",
        ] {
            let p = self.assets_dir.join(name);
            if p.exists() {
                if let Some(s) = p.to_str() {
                    let remote = format!("/data/local/tmp/{}", name);
                    let _ = self.run_adb(&["-s", &addr, "push", s, &remote], 6000);
                }
            }
        }
        let _ = self.shell(
            "chmod 644 /data/local/tmp/MiTVDaemon.jar /data/local/tmp/MtkDirectTool.jar /data/local/tmp/VolDirectTool.jar /data/local/tmp/MiTVEdge.jar 2>/dev/null",
            2500,
        );
        if let Ok(mut g) = self.jars_verified.lock() {
            *g = true;
        }
    }

    fn start_daemon_locked(&self, guard: &mut Option<DaemonProc>) -> bool {
        if let Some(ref mut d) = *guard {
            if matches!(d.child.try_wait(), Ok(None)) {
                return true;
            }
        }
        *guard = None;
        if !self.adb_path.exists() {
            return false;
        }
        self.ensure_jars_deployed();
        let addr = self.tv_address();
        let mut child = match Command::new(&self.adb_path)
            .args([
                "-s",
                &addr,
                "shell",
                "su 1000 -c 'CLASSPATH=/data/local/tmp/MiTVDaemon.jar /system/bin/app_process /data/local/tmp MiTVDaemon'",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            return false;
        };
        let Some(stdout_raw) = child.stdout.take() else {
            let _ = child.kill();
            return false;
        };
        let mut stdout = BufReader::new(stdout_raw);
        let mut ready_line = String::new();
        if stdout.read_line(&mut ready_line).unwrap_or(0) > 0 && ready_line.trim() == "READY" {
            *guard = Some(DaemonProc { child, stdin, stdout });
            true
        } else {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }

    fn daemon_cmd(&self, cmd: &str) -> Option<String> {
        let mut guard = self.daemon.lock().ok()?;
        if !self.start_daemon_locked(&mut guard) {
            return None;
        }
        if let Some(ref mut d) = *guard {
            if let Some(resp) = d.send_cmd(cmd) {
                return Some(resp);
            }
        }
        // Retry once after restarting dead pipe
        if let Some(d) = guard.take() {
            d.shutdown();
        }
        if self.start_daemon_locked(&mut guard) {
            if let Some(ref mut d) = *guard {
                return d.send_cmd(cmd);
            }
        }
        None
    }

    fn slider_worker_loop(&self) {
        let mut last_activity = Instant::now();
        let mut pending_bl_sync: Option<(i32, Instant)> = None;
        let mut last_bl_used_mtk = false;

        loop {
            let task = {
                let mut q = match self.queue.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let has_work = q.backlight.is_some()
                    || q.volume.is_some()
                    || q.sync_backlight.is_some()
                    || q.step_bl_delta != 0
                    || q.warm
                    || q.stop_daemon;

                if !has_work {
                    let daemon_alive = self
                        .daemon
                        .lock()
                        .map(|g| g.is_some())
                        .unwrap_or(false);

                    let wait_dur = if let Some((_, at)) = pending_bl_sync {
                        let now = Instant::now();
                        if now >= at {
                            Duration::from_millis(1)
                        } else {
                            at - now
                        }
                    } else if daemon_alive && !self.keep_warm.load(Ordering::Acquire) {
                        let elapsed = last_activity.elapsed();
                        let idle = Duration::from_secs(DAEMON_IDLE_SECS);
                        if elapsed >= idle {
                            Duration::from_millis(1)
                        } else {
                            idle - elapsed
                        }
                    } else {
                        Duration::from_secs(60)
                    };

                    let (guard_after, _) = match self.cv.wait_timeout(q, wait_dur) {
                        Ok(r) => r,
                        Err(_) => return,
                    };
                    q = guard_after;
                }

                std::mem::take(&mut *q)
            };

            if task.stop_daemon {
                if let Ok(mut g) = self.daemon.lock() {
                    if let Some(d) = g.take() {
                        d.shutdown();
                    }
                }
                pending_bl_sync = None;
                continue;
            }

            if task.warm {
                if let Ok(mut g) = self.daemon.lock() {
                    if g.is_none() {
                        let _ = self.start_daemon_locked(&mut g);
                    }
                }
                last_activity = Instant::now();
            }

            if task.step_bl_delta != 0 {
                let mut cur = self.cached_bl.load(Ordering::Relaxed);
                if !(0..=100).contains(&cur) {
                    if let Some(resp) = self.daemon_cmd("G") {
                        let mut parts = resp.split_whitespace();
                        if parts.next() == Some("G") {
                            if let Some(Ok(bl)) = parts.next().map(|s| s.parse::<i32>()) {
                                if (0..=100).contains(&bl) {
                                    cur = bl;
                                    self.cached_bl.store(bl, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
                if !(0..=100).contains(&cur) {
                    let hint = self.tv_monitor_hint();
                    if let Some(bl) = crate::ddc::get_brightness(&hint) {
                        cur = bl;
                        self.cached_bl.store(bl, Ordering::Relaxed);
                    }
                }
                if !(0..=100).contains(&cur) {
                    cur = 50;
                }
                let next = (cur + task.step_bl_delta).clamp(0, 100);
                self.cached_bl.store(next, Ordering::Relaxed);
                last_bl_used_mtk = self.write_backlight_fast(next);
                if last_bl_used_mtk {
                    pending_bl_sync = Some((next, Instant::now() + Duration::from_millis(450)));
                } else {
                    pending_bl_sync = None;
                }
                last_activity = Instant::now();
            }

            if let Some(v) = task.backlight {
                last_bl_used_mtk = self.write_backlight_fast(v);
                last_activity = Instant::now();
            }

            if let Some(v) = task.volume {
                self.write_volume_fast(v);
                last_activity = Instant::now();
            }

            if let Some(v) = task.sync_backlight {
                pending_bl_sync = None;
                if last_bl_used_mtk {
                    self.sync_backlight_settings(v);
                }
                last_activity = Instant::now();
            } else if let Some((v, at)) = pending_bl_sync {
                if Instant::now() >= at {
                    pending_bl_sync = None;
                    self.sync_backlight_settings(v);
                    last_activity = Instant::now();
                }
            }

            // Idle auto-shutdown when flyout is hidden & not in TV takeover
            if !self.keep_warm.load(Ordering::Acquire)
                && pending_bl_sync.is_none()
                && last_activity.elapsed() >= Duration::from_secs(DAEMON_IDLE_SECS)
            {
                if let Ok(mut g) = self.daemon.lock() {
                    if let Some(d) = g.take() {
                        d.shutdown();
                    }
                }
            }
        }
    }

    /// Writes hardware backlight (0..=100). Returns `true` if written via TV-side MTK register
    /// (requiring delayed system PQ settings sync), `false` if handled rootlessly via DDC/CI or failed.
    fn write_backlight_fast(&self, v: i32) -> bool {
        if let Some(resp) = self.daemon_cmd(&format!("B {}", v)) {
            if resp.starts_with("OK") {
                return true;
            }
        }
        // Rootless DDC/CI path (instant hardware brightness on DP/HDMI without Root!)
        let hint = self.tv_monitor_hint();
        if crate::ddc::set_brightness(&hint, v) {
            return false;
        }
        // Fallback if MiTVDaemon & DDC/CI both unavailable (rooted MTK direct tool)
        if self.ensure_connected() {
            let cmd = format!(
                "su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool set g_disp__disp_back_light {} 3' >/dev/null 2>&1",
                v
            );
            let (rc, _, _) = self.shell(&cmd, 3000);
            return rc == 0;
        }
        false
    }

    fn write_volume_fast(&self, v: i32) {
        if let Some(resp) = self.daemon_cmd(&format!("V {}", v)) {
            if resp.starts_with("OK") {
                return;
            }
        }
        // Fast rootless Binder fallback (~68ms, works without Root!)
        let cmd = format!("cmd media_session volume --stream 3 --set {} --show >/dev/null 2>&1", v);
        let (code, _, _) = self.shell(&cmd, 2000);
        if code != 0 && self.ensure_connected() {
            let _ = self.shell(&cmd, 2500);
        }
    }

    fn sync_backlight_settings(&self, v: i32) {
        let cmd = format!(
            "am broadcast -a com.xiaomi.mitv.action.PIC_MODE_CHANGED --ei picmode 7 >/dev/null 2>&1; \
             settings put global picture_backlight {0}; settings put global xiaomi_picture_backlight {0}",
            v
        );
        let _ = self.shell(&cmd, 3000);
    }

    pub fn query_status(&self) -> DeviceStatus {
        let mut st = DeviceStatus::default();
        if !self.ensure_connected() {
            return st;
        }
        st.connected = true;
        self.ensure_jars_deployed();

        // If daemon is already warm, read BL & VOL in ~15ms!
        let mut got_fast = false;
        if let Ok(mut g) = self.daemon.lock() {
            if let Some(ref mut d) = *g {
                if let Some(resp) = d.send_cmd("G") {
                    let mut parts = resp.split_whitespace();
                    if parts.next() == Some("G") {
                        if let Some(Ok(bl)) = parts.next().map(|s| s.parse::<i32>()) {
                            if (0..=100).contains(&bl) {
                                st.backlight = Some(bl);
                                self.cached_bl.store(bl, Ordering::Relaxed);
                            }
                        }
                        if let Some(Ok(vol)) = parts.next().map(|s| s.parse::<i32>()) {
                            if (0..=100).contains(&vol) {
                                st.volume = Some(vol);
                                self.cached_vol.store(vol, Ordering::Relaxed);
                            }
                        }
                        got_fast = st.backlight.is_some() && st.volume.is_some();
                    }
                }
            }
        }

        let cmd = if got_fast {
            "echo SRC=$(settings get global mitv.tvplayer.hdmi.last.source); \
             echo FOC=$(dumpsys window | grep -E 'mCurrentFocus|mFocusedApp' | head -n 2 | tr '\\n' ' ')"
        } else {
            "echo SRC=$(settings get global mitv.tvplayer.hdmi.last.source); \
             echo FOC=$(dumpsys window | grep -E 'mCurrentFocus|mFocusedApp' | head -n 2 | tr '\\n' ' '); \
             echo BL=$(su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool get g_disp__disp_back_light' 2>/dev/null | grep -oE '[0-9]+$' | tail -n 1); \
             echo VOL=$(cmd media_session volume --stream 3 --get 2>/dev/null | grep -oE 'volume is [0-9]+' | grep -oE '[0-9]+')"
        };

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
                        self.cached_bl.store(n, Ordering::Relaxed);
                    }
                }
            } else if let Some(v) = line.strip_prefix("VOL=") {
                if let Ok(n) = v.trim().parse::<i32>() {
                    if (0..=100).contains(&n) {
                        st.volume = Some(n);
                        self.cached_vol.store(n, Ordering::Relaxed);
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

        // Rootless DDC/CI backlight readout fallback when TV is unrooted and displaying PC input
        if st.backlight.is_none() && st.active_source != SignalSource::TvSystem {
            let hint = self.tv_monitor_hint();
            if let Some(bl) = crate::ddc::get_brightness(&hint) {
                st.backlight = Some(bl);
                self.cached_bl.store(bl, Ordering::Relaxed);
            }
        }

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

    pub fn start_persistent_shell_async(self: &Arc<Self>) {
        let dev = Arc::clone(self);
        std::thread::spawn(move || dev.start_persistent_shell());
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

    pub fn stop_persistent_shell_async(self: &Arc<Self>) {
        let dev = Arc::clone(self);
        std::thread::spawn(move || dev.stop_persistent_shell());
    }

    pub fn send_shell_cmd_fast(self: &Arc<Self>, cmd: String) {
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
                    let line = format!("{}\n", cmd);
                    if ps.stdin.write_all(line.as_bytes()).is_ok() && ps.stdin.flush().is_ok() {
                        sent = true;
                    } else {
                        let _ = ps.child.kill();
                        *guard = None;
                    }
                }
            }
            if !sent {
                let _ = dev.shell(&cmd, 2500);
            }
        });
    }

    pub fn send_tv_key_fast(self: &Arc<Self>, keycode: i32) {
        self.send_shell_cmd_fast(format!("cmd input keyevent {}", keycode));
    }

    pub fn trigger_voice_assistant(self: &Arc<Self>) {
        self.send_shell_cmd_fast(
            "am start-service -n com.xiaomi.voicecontrol/.VoiceControlService -a com.xiaomi.mitv.voice.ONBOARD_MIC_START >/dev/null 2>&1"
                .to_string(),
        );
    }
}
