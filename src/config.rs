use crate::keymap::{KeyChord, KeyMap, ALL_ACTIONS};
use std::fs;
use std::path::{Path, PathBuf};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ,
};

const RUN_REG_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_REG_NAME: &str = "MiTVSwitch";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalSource {
    Unknown,
    TvSystem,
    Dp,
    Hdmi1,
    Hdmi2,
}

impl SignalSource {
    pub fn label(self) -> &'static str {
        match self {
            SignalSource::TvSystem => "电视系统",
            SignalSource::Dp => "电脑 DP",
            SignalSource::Hdmi1 => "HDMI 1",
            SignalSource::Hdmi2 => "HDMI 2",
            SignalSource::Unknown => "未知",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceStatus {
    pub connected: bool,
    pub active_source: SignalSource,
    pub backlight: Option<i32>,
    pub volume: Option<i32>,
}

impl Default for DeviceStatus {
    fn default() -> Self {
        Self {
            connected: false,
            active_source: SignalSource::Unknown,
            backlight: None,
            volume: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub path: PathBuf,
    pub tv_address: String,
    pub auto_takeover_on_tv: bool,
    pub wheel_as_dpad: bool,
    pub dp_input: i32,
    pub hdmi1_input: i32,
    pub hdmi2_input: i32,
    pub keymap: KeyMap,
}

impl AppConfig {
    pub fn load(path: PathBuf) -> Self {
        let mut cfg = Self {
            path,
            tv_address: "192.168.1.200:5555".to_string(),
            auto_takeover_on_tv: true,
            wheel_as_dpad: true,
            dp_input: 29,
            hdmi1_input: 23,
            hdmi2_input: 24,
            keymap: KeyMap::default(),
        };
        if !cfg.path.exists() {
            cfg.save();
            return cfg;
        }
        if let Ok(text) = fs::read_to_string(&cfg.path) {
            for raw in text.lines() {
                let line = raw.trim().trim_start_matches('\u{FEFF}');
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim();
                    let v = v.trim();
                    if k.eq_ignore_ascii_case("TvAddress") && !v.is_empty() {
                        cfg.tv_address = v.to_string();
                    } else if k.eq_ignore_ascii_case("AutoTakeoverOnTv") {
                        if let Ok(b) = v.to_ascii_lowercase().parse::<bool>() {
                            cfg.auto_takeover_on_tv = b;
                        }
                    } else if k.eq_ignore_ascii_case("WheelAsDpad") {
                        if let Ok(b) = v.to_ascii_lowercase().parse::<bool>() {
                            cfg.wheel_as_dpad = b;
                        }
                    } else if k.eq_ignore_ascii_case("DpInput") {
                        if let Ok(n) = v.parse::<i32>() {
                            cfg.dp_input = n;
                        }
                    } else if k.eq_ignore_ascii_case("Hdmi1Input") {
                        if let Ok(n) = v.parse::<i32>() {
                            cfg.hdmi1_input = n;
                        }
                    } else if k.eq_ignore_ascii_case("Hdmi2Input") {
                        if let Ok(n) = v.parse::<i32>() {
                            cfg.hdmi2_input = n;
                        }
                    } else {
                        for act in ALL_ACTIONS {
                            if k.eq_ignore_ascii_case(act.ini_key()) {
                                let parsed = KeyChord::parse(v);
                                if parsed.is_some() || !act.is_required() {
                                    cfg.keymap.set(act, parsed);
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
        cfg
    }

    pub fn save(&self) {
        let mut content = format!(
            "# G Pro 27U 融合切换工具配置 (Rust 原生版 / 键盘固定 SDK 模式)\r\n\
             TvAddress={}\r\n\
             AutoTakeoverOnTv={}\r\n\
             WheelAsDpad={}\r\n\
             DpInput={}\r\n\
             Hdmi1Input={}\r\n\
             Hdmi2Input={}\r\n\
             # 自定义遥控快捷键（留空表示禁用，仅接管电视期间生效）\r\n",
            self.tv_address,
            if self.auto_takeover_on_tv { "True" } else { "False" },
            if self.wheel_as_dpad { "True" } else { "False" },
            self.dp_input,
            self.hdmi1_input,
            self.hdmi2_input
        );
        for act in ALL_ACTIONS {
            let val = self.keymap.get(act).map(|c| c.format()).unwrap_or_default();
            content.push_str(&format!("{}={}\r\n", act.ini_key(), val));
        }
        let _ = fs::write(&self.path, content);
    }

    pub fn normalize_address(raw: &str) -> String {
        let s = raw.trim();
        if s.is_empty() {
            return String::new();
        }
        if s.contains(':') {
            s.to_string()
        } else {
            format!("{}:5555", s)
        }
    }
}

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn resolve_asset_dir(base: &Path) -> PathBuf {
    let direct = base.join("assets");
    if direct.exists() {
        return direct;
    }
    // When running from target/release during dev
    if let Some(p2) = base.parent().and_then(|p| p.parent()) {
        let candidate = p2.join("assets");
        if candidate.exists() {
            return candidate;
        }
    }
    direct
}

pub fn resolve_scrcpy_dir(base: &Path) -> PathBuf {
    let direct = base.join("scrcpy");
    if direct.exists() {
        return direct;
    }
    if let Some(p2) = base.parent().and_then(|p| p.parent()) {
        let candidate = p2.join("scrcpy");
        if candidate.exists() {
            return candidate;
        }
    }
    direct
}

pub fn is_autostart_enabled() -> bool {
    unsafe {
        let path_w = to_wide(RUN_REG_PATH);
        let name_w = to_wide(RUN_REG_NAME);
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return false;
        }
        let mut size: u32 = 0;
        let rc = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        RegCloseKey(hkey);
        rc == 0 && size > 2
    }
}

pub fn set_autostart(enable: bool) {
    unsafe {
        let path_w = to_wide(RUN_REG_PATH);
        let name_w = to_wide(RUN_REG_NAME);
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) != 0 {
            return;
        }
        if enable {
            if let Ok(exe) = std::env::current_exe() {
                let val = format!("\"{}\"", exe.display());
                let val_w = to_wide(&val);
                let byte_len = (val_w.len() * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    name_w.as_ptr(),
                    0,
                    REG_SZ,
                    val_w.as_ptr() as *const u8,
                    byte_len,
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, name_w.as_ptr());
        }
        RegCloseKey(hkey);
    }
}
