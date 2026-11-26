use crate::display::EdgeSide;
use crate::keymap::{KeyChord, KeyMap, ALL_ACTIONS};
use std::fs;
use std::path::{Path, PathBuf};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ,
};

const RUN_REG_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_REG_NAME: &str = "MiKVM";
const LEGACY_RUN_REG_NAME: &str = "MiTVSwitch";
const PERSONALIZE_REG_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

fn read_personalize_dword(name: &str, default_val: u32) -> u32 {
    unsafe {
        let path_w = to_wide(PERSONALIZE_REG_PATH);
        let name_w = to_wide(name);
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return default_val;
        }
        let mut val: u32 = default_val;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let rc = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut val as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        if rc == 0 { val } else { default_val }
    }
}

pub fn is_apps_dark_mode() -> bool {
    read_personalize_dword("AppsUseLightTheme", 1) == 0
}

pub fn is_system_dark_mode() -> bool {
    read_personalize_dword("SystemUsesLightTheme", 1) == 0
}

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
    pub edge_switch: bool,
    pub edge_resistance_pc: i32,
    pub edge_resistance_tv: i32,
    pub clipboard_sync: bool,
    pub auto_internal_on_tv: bool,
    pub tv_monitor_hint: String,
    pub tv_edge_side: EdgeSide,
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
            edge_switch: true,
            edge_resistance_pc: 120,
            edge_resistance_tv: 180,
            clipboard_sync: true,
            auto_internal_on_tv: false,
            tv_monitor_hint: "Mi Monitor".to_string(),
            tv_edge_side: EdgeSide::Left,
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
                        // Legacy key kept for backward compatibility; wheel always maps to DPAD up/down
                    } else if k.eq_ignore_ascii_case("EdgeSwitch") {
                        if let Ok(b) = v.to_ascii_lowercase().parse::<bool>() {
                            cfg.edge_switch = b;
                        }
                    } else if k.eq_ignore_ascii_case("EdgeResistancePc") {
                        if let Ok(n) = v.parse::<i32>() {
                            cfg.edge_resistance_pc = n.clamp(0, 300);
                        }
                    } else if k.eq_ignore_ascii_case("EdgeResistanceTv") {
                        if let Ok(n) = v.parse::<i32>() {
                            cfg.edge_resistance_tv = n.clamp(0, 300);
                        }
                    } else if k.eq_ignore_ascii_case("ClipboardSync") {
                        if let Ok(b) = v.to_ascii_lowercase().parse::<bool>() {
                            cfg.clipboard_sync = b;
                        }
                    } else if k.eq_ignore_ascii_case("AutoInternalOnTv") {
                        if let Ok(b) = v.to_ascii_lowercase().parse::<bool>() {
                            cfg.auto_internal_on_tv = b;
                        }
                    } else if k.eq_ignore_ascii_case("TvMonitorHint") {
                        cfg.tv_monitor_hint = v.to_string();
                    } else if k.eq_ignore_ascii_case("TvEdgeSide") {
                        if let Some(side) = EdgeSide::parse(v) {
                            cfg.tv_edge_side = side;
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
            "# MiKVM 配置文件 (Redmi G Pro 27U / MiTV 软件 KVM 与切源工具)\r\n\
             TvAddress={}\r\n\
             AutoTakeoverOnTv={}\r\n\
             EdgeSwitch={}\r\n\
             EdgeResistancePc={}\r\n\
             EdgeResistanceTv={}\r\n\
             ClipboardSync={}\r\n\
             AutoInternalOnTv={}\r\n\
             TvMonitorHint={}\r\n\
             TvEdgeSide={}\r\n\
             DpInput={}\r\n\
             Hdmi1Input={}\r\n\
             Hdmi2Input={}\r\n\
             # 自定义遥控快捷键（留空表示禁用）\r\n",
            self.tv_address,
            if self.auto_takeover_on_tv { "True" } else { "False" },
            if self.edge_switch { "True" } else { "False" },
            self.edge_resistance_pc.clamp(0, 300),
            self.edge_resistance_tv.clamp(0, 300),
            if self.clipboard_sync { "True" } else { "False" },
            if self.auto_internal_on_tv { "True" } else { "False" },
            self.tv_monitor_hint,
            self.tv_edge_side.as_str(),
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
    if direct.join("scrcpy.exe").exists() || direct.join("adb.exe").exists() {
        return direct;
    }
    if let Some(p2) = base.parent().and_then(|p| p.parent()) {
        let candidate = p2.join("scrcpy");
        if candidate.join("scrcpy.exe").exists() || candidate.join("adb.exe").exists() {
            return candidate;
        }
    }
    // Fallback: auto-discover scrcpy-server / adb.exe from system PATH (e.g. winget / scoop)
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if (dir.join("scrcpy-server").exists() || dir.join("scrcpy.exe").exists())
                && dir.join("adb.exe").exists()
            {
                return dir;
            }
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
        let _ = RegDeleteValueW(hkey, to_wide(LEGACY_RUN_REG_NAME).as_ptr());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeyAction;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_ini(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("mikvm_test_{}_{}_{}.ini", tag, std::process::id(), nanos))
    }

    #[test]
    fn normalize_address_adds_default_port() {
        assert_eq!(AppConfig::normalize_address("192.168.1.200"), "192.168.1.200:5555");
        assert_eq!(AppConfig::normalize_address("  192.168.1.200:5566  "), "192.168.1.200:5566");
        assert_eq!(AppConfig::normalize_address("   "), "");
    }

    #[test]
    fn wide_string_roundtrip() {
        let s = "MiKVM 电视待命 (焦点在电脑)";
        let w = to_wide(s);
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), s);
    }

    #[test]
    fn config_bom_clamp_and_roundtrip() {
        let path = temp_ini("bom");
        let content = "\u{FEFF}TvAddress=10.0.0.8:5555\r\n\
                       AutoTakeoverOnTv=False\r\n\
                       WheelAsDpad=False\r\n\
                       EdgeSwitch=True\r\n\
                       EdgeResistancePc=9999\r\n\
                       EdgeResistanceTv=-50\r\n\
                       ClipboardSync=False\r\n\
                       AutoInternalOnTv=True\r\n\
                       TvEdgeSide=Right\r\n\
                       Key.ReturnDp=\r\n\
                       Key.Voice=\r\n\
                       Key.ToggleFocus=Ctrl+Shift+S\r\n";
        fs::write(&path, content).expect("write temp ini");

        let cfg = AppConfig::load(path.clone());
        assert_eq!(cfg.tv_address, "10.0.0.8:5555");
        assert!(!cfg.auto_takeover_on_tv);
        assert!(cfg.edge_switch);
        assert_eq!(cfg.edge_resistance_pc, 300);
        assert_eq!(cfg.edge_resistance_tv, 0);
        assert!(!cfg.clipboard_sync);
        assert!(cfg.auto_internal_on_tv);
        assert_eq!(cfg.tv_edge_side, EdgeSide::Right);
        // Required action ignores empty value; optional action clears cleanly
        assert!(cfg.keymap.get(KeyAction::ReturnDp).is_some());
        assert!(cfg.keymap.get(KeyAction::Voice).is_none());
        assert_eq!(
            cfg.keymap.get(KeyAction::ToggleFocus).map(|c| c.format()).as_deref(),
            Some("Ctrl+Shift+S")
        );

        cfg.save();
        let reloaded = AppConfig::load(path.clone());
        assert_eq!(reloaded.tv_address, cfg.tv_address);
        assert_eq!(reloaded.edge_resistance_pc, 300);
        assert_eq!(reloaded.edge_resistance_tv, 0);
        assert_eq!(reloaded.tv_edge_side, EdgeSide::Right);
        assert!(reloaded.keymap.get(KeyAction::Voice).is_none());

        let _ = fs::remove_file(&path);
    }
}
