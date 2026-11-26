use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 512 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

#[repr(C)]
#[derive(Clone, Copy)]
struct SystemTimeWin {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(lp_system_time: *mut SystemTimeWin);
}

fn format_timestamp(st: SystemTimeWin) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.year, st.month, st.day, st.hour, st.minute, st.second, st.milliseconds
    )
}

fn now_timestamp() -> String {
    let mut st = SystemTimeWin {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    unsafe {
        GetLocalTime(&mut st);
    }
    format_timestamp(st)
}

pub fn init(log_path: PathBuf, enabled: bool) {
    if let Ok(mut g) = LOG_PATH.lock() {
        *g = Some(log_path);
    }
    ENABLED.store(enabled, Ordering::Release);
    if enabled {
        log_line("INIT", "MiKVM diagnostic logging enabled");
    }
}

#[inline]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn rotate_if_oversized(path: &Path, max_bytes: u64) {
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len >= max_bytes {
        let mut old_name = path.as_os_str().to_os_string();
        old_name.push(".old");
        let old_path = PathBuf::from(old_name);
        let _ = fs::remove_file(&old_path);
        let _ = fs::rename(path, &old_path);
    }
}

fn append_line_to(path: &Path, tag: &str, msg: &str, max_bytes: u64) {
    rotate_if_oversized(path, max_bytes);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let ts = now_timestamp();
        let _ = writeln!(f, "[{}] [{}] {}", ts, tag, msg);
    }
}

pub fn log_line(tag: &str, msg: &str) {
    if !is_enabled() {
        return;
    }
    if let Ok(g) = LOG_PATH.lock() {
        if let Some(ref path) = *g {
            append_line_to(path, tag, msg, MAX_LOG_BYTES);
        }
    }
}

#[macro_export]
macro_rules! diag_log {
    ($tag:expr, $($arg:tt)*) => {
        if $crate::diag::is_enabled() {
            $crate::diag::log_line($tag, &format!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_log(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("mikvm_diag_{}_{}_{}.log", name, std::process::id(), nanos))
    }

    #[test]
    fn timestamp_formatting() {
        let st = SystemTimeWin {
            year: 2026,
            month: 11,
            day_of_week: 5,
            day: 27,
            hour: 0,
            minute: 5,
            second: 9,
            milliseconds: 42,
        };
        assert_eq!(format_timestamp(st), "2026-11-27 00:05:09.042");
    }

    #[test]
    fn append_and_rotate_log_file() {
        let path = temp_log("rot");
        let old_path = PathBuf::from(format!("{}.old", path.display()));
        append_line_to(&path, "TEST", "first line", 64);
        let text1 = fs::read_to_string(&path).expect("read first");
        assert!(text1.contains("[TEST] first line"));

        // Exceed 64-byte rotation cap so next write rotates previous log into .old
        append_line_to(&path, "TEST", "second line that pushes size past rotation limit", 64);
        append_line_to(&path, "TEST", "third line after rotate", 64);
        let text_new = fs::read_to_string(&path).expect("read rotated");
        assert!(text_new.contains("third line after rotate"));
        assert!(old_path.exists());

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&old_path);
    }

    #[test]
    fn disabled_mode_creates_no_file() {
        let path = temp_log("off");
        ENABLED.store(false, Ordering::Release);
        if let Ok(mut g) = LOG_PATH.lock() {
            *g = Some(path.clone());
        }
        log_line("TEST", "should never be written");
        assert!(!path.exists());
    }
}
