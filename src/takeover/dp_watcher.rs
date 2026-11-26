use super::state::{current_mode, CREATE_NO_WINDOW, WM_DP_RETURNED};
use crate::adb::DeviceController;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub(super) fn is_external_source_focus(dumpsys_out: &str) -> bool {
    dumpsys_out.to_ascii_lowercase().contains("externalsourceactivity")
}

/// Optimized DP Watcher: reuses a single persistent `adb shell` stream instead of spawning
/// ~5500 short-lived `adb.exe` processes per hour during long multi-screen sessions!
pub(super) fn spawn_dp_return_watcher(
    dev: Arc<DeviceController>,
    adb_path: PathBuf,
    addr: String,
    host_hwnd: usize,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let mut saw_non_ext = false;
        let mut consecutive_ext = 0;

        let mut shell_child = Command::new(&adb_path)
            .args(["-s", &addr, "shell"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok();

        let mut stdin_opt = shell_child.as_mut().and_then(|c| c.stdin.take());
        let mut reader_opt = shell_child
            .as_mut()
            .and_then(|c| c.stdout.take())
            .map(BufReader::new);

        while !stop.load(Ordering::Relaxed) && current_mode().is_session_active() {
            let mut out = String::new();
            let mut used_persistent = false;

            if let (Some(ref mut sin), Some(ref mut rdr)) = (&mut stdin_opt, &mut reader_opt) {
                let probe = b"dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'; echo __MK_END__\n";
                if sin.write_all(probe).is_ok() && sin.flush().is_ok() {
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match rdr.read_line(&mut line) {
                            Ok(0) | Err(_) => {
                                break;
                            }
                            Ok(_) => {
                                if line.trim() == "__MK_END__" {
                                    used_persistent = true;
                                    break;
                                }
                                out.push_str(&line);
                            }
                        }
                    }
                }
            }

            if !used_persistent {
                // Clean up dead pipe and fall back once to single adb call
                stdin_opt = None;
                reader_opt = None;
                if let Some(mut ch) = shell_child.take() {
                    let _ = ch.kill();
                    let _ = ch.wait();
                }
                let (_, fallback_out, _) = dev.shell(
                    "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'",
                    2500,
                );
                out = fallback_out;
            }

            let is_ext = is_external_source_focus(&out);
            if !is_ext && !out.trim().is_empty() {
                saw_non_ext = true;
                consecutive_ext = 0;
            } else if is_ext {
                consecutive_ext += 1;
                if saw_non_ext || consecutive_ext >= 2 {
                    unsafe {
                        PostMessageW(host_hwnd as HWND, WM_DP_RETURNED, 0, 0);
                    }
                    break;
                }
            }

            let sleep_ms = if saw_non_ext { 1100 } else { 450 };
            // Sleep in small slices so stop() exits promptly
            let mut slept = 0;
            while slept < sleep_ms && !stop.load(Ordering::Relaxed) {
                let step = (sleep_ms - slept).min(150);
                std::thread::sleep(Duration::from_millis(step));
                slept += step;
            }
        }

        drop(stdin_opt);
        drop(reader_opt);
        if let Some(mut ch) = shell_child {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::is_external_source_focus;

    #[test]
    fn external_source_focus_detection() {
        let dp_out = "  mCurrentFocus=Window{2872f13 u0 com.xiaomi.mitv.tvplayer/com.xiaomi.mitv.tvplayer.ExternalSourceActivity}\n";
        assert!(is_external_source_focus(dp_out));
        let tv_out = "  mCurrentFocus=Window{1e2106e u0 app.orchard.tv/app.orchard.tv.MainActivity}\n";
        assert!(!is_external_source_focus(tv_out));
        assert!(!is_external_source_focus(""));
    }
}
