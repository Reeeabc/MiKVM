use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

pub const MOD_CTRL: u8 = 1 << 0;
pub const MOD_ALT: u8 = 1 << 1;
pub const MOD_SHIFT: u8 = 1 << 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyChord {
    pub mods: u8,
    pub vk: u16,
}

impl KeyChord {
    pub const fn new(mods: u8, vk: u16) -> Self {
        Self { mods, vk }
    }

    pub fn format(self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if (self.mods & MOD_CTRL) != 0 {
            parts.push("Ctrl".to_string());
        }
        if (self.mods & MOD_ALT) != 0 {
            parts.push("Alt".to_string());
        }
        if (self.mods & MOD_SHIFT) != 0 {
            parts.push("Shift".to_string());
        }
        parts.push(vk_to_name(self.vk));
        parts.join("+")
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("none") || s == "-" {
            return None;
        }
        let mut mods: u8 = 0;
        let mut vk: Option<u16> = None;
        for token in s.split('+') {
            let t = token.trim();
            if t.is_empty() {
                continue;
            }
            if t.eq_ignore_ascii_case("ctrl") || t.eq_ignore_ascii_case("control") {
                mods |= MOD_CTRL;
            } else if t.eq_ignore_ascii_case("alt") || t.eq_ignore_ascii_case("menu") {
                mods |= MOD_ALT;
            } else if t.eq_ignore_ascii_case("shift") {
                mods |= MOD_SHIFT;
            } else {
                vk = name_to_vk(t);
            }
        }
        let vk = vk?;
        let chord = KeyChord { mods, vk };
        if validate_chord(chord).is_ok() {
            Some(chord)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum KeyAction {
    ReturnDp = 0,
    Back = 1,
    Home = 2,
    Menu = 3,
    Voice = 4,
    VolUp = 5,
    VolDown = 6,
    Mute = 7,
    BrightnessUp = 8,
    BrightnessDown = 9,
    Power = 10,
}

pub const ACTION_COUNT: usize = 11;

pub const ALL_ACTIONS: [KeyAction; ACTION_COUNT] = [
    KeyAction::ReturnDp,
    KeyAction::Back,
    KeyAction::Home,
    KeyAction::Menu,
    KeyAction::Voice,
    KeyAction::VolUp,
    KeyAction::VolDown,
    KeyAction::Mute,
    KeyAction::BrightnessUp,
    KeyAction::BrightnessDown,
    KeyAction::Power,
];

impl KeyAction {
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn ini_key(self) -> &'static str {
        match self {
            KeyAction::ReturnDp => "Key.ReturnDp",
            KeyAction::Back => "Key.Back",
            KeyAction::Home => "Key.Home",
            KeyAction::Menu => "Key.Menu",
            KeyAction::Voice => "Key.Voice",
            KeyAction::VolUp => "Key.VolUp",
            KeyAction::VolDown => "Key.VolDown",
            KeyAction::Mute => "Key.Mute",
            KeyAction::BrightnessUp => "Key.BrightnessUp",
            KeyAction::BrightnessDown => "Key.BrightnessDown",
            KeyAction::Power => "Key.Power",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            KeyAction::ReturnDp => "切回电脑 DP",
            KeyAction::Back => "返回 (Back)",
            KeyAction::Home => "主页 (Home)",
            KeyAction::Menu => "菜单 (≡)",
            KeyAction::Voice => "语音助手 (小爱)",
            KeyAction::VolUp => "音量 + (支持连发)",
            KeyAction::VolDown => "音量 - (支持连发)",
            KeyAction::Mute => "静音",
            KeyAction::BrightnessUp => "亮度 + (支持连发)",
            KeyAction::BrightnessDown => "亮度 - (支持连发)",
            KeyAction::Power => "待机息屏",
        }
    }

    pub const fn is_repeatable(self) -> bool {
        matches!(
            self,
            KeyAction::VolUp
                | KeyAction::VolDown
                | KeyAction::BrightnessUp
                | KeyAction::BrightnessDown
        )
    }

    pub const fn is_required(self) -> bool {
        matches!(self, KeyAction::ReturnDp)
    }

    pub const fn android_keycode(self) -> Option<i32> {
        match self {
            KeyAction::Back => Some(4),
            KeyAction::Home => Some(3),
            KeyAction::Menu => Some(82),
            KeyAction::VolUp => Some(24),
            KeyAction::VolDown => Some(25),
            KeyAction::Mute => Some(164),
            KeyAction::Power => Some(26),
            KeyAction::ReturnDp
            | KeyAction::Voice
            | KeyAction::BrightnessUp
            | KeyAction::BrightnessDown => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMap {
    bindings: [Option<KeyChord>; ACTION_COUNT],
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::preset_1()
    }
}

impl KeyMap {
    pub fn preset_1() -> Self {
        let mut b = [None; ACTION_COUNT];
        b[KeyAction::ReturnDp.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'D' as u16));
        b[KeyAction::Back.index()] = Some(KeyChord::new(0, VK_ESCAPE));
        b[KeyAction::Home.index()] = Some(KeyChord::new(0, VK_HOME));
        b[KeyAction::Menu.index()] = Some(KeyChord::new(0, VK_F12));
        b[KeyAction::Voice.index()] = Some(KeyChord::new(0, VK_F11));
        b[KeyAction::VolUp.index()] = Some(KeyChord::new(0, VK_PRIOR));
        b[KeyAction::VolDown.index()] = Some(KeyChord::new(0, VK_NEXT));
        b[KeyAction::Mute.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'M' as u16));
        b[KeyAction::BrightnessUp.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, VK_UP));
        b[KeyAction::BrightnessDown.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, VK_DOWN));
        b[KeyAction::Power.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'P' as u16));
        Self { bindings: b }
    }

    pub fn preset_2() -> Self {
        let mut b = [None; ACTION_COUNT];
        b[KeyAction::ReturnDp.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'D' as u16));
        b[KeyAction::Back.index()] = Some(KeyChord::new(0, VK_ESCAPE));
        b[KeyAction::Home.index()] = Some(KeyChord::new(MOD_ALT, b'H' as u16));
        b[KeyAction::Menu.index()] = Some(KeyChord::new(MOD_ALT, b'M' as u16));
        b[KeyAction::Voice.index()] = Some(KeyChord::new(MOD_ALT, b'V' as u16));
        b[KeyAction::VolUp.index()] = Some(KeyChord::new(MOD_ALT, VK_UP));
        b[KeyAction::VolDown.index()] = Some(KeyChord::new(MOD_ALT, VK_DOWN));
        b[KeyAction::Mute.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'M' as u16));
        b[KeyAction::BrightnessUp.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, VK_UP));
        b[KeyAction::BrightnessDown.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, VK_DOWN));
        b[KeyAction::Power.index()] = Some(KeyChord::new(MOD_CTRL | MOD_ALT, b'P' as u16));
        Self { bindings: b }
    }

    pub fn get(&self, action: KeyAction) -> Option<KeyChord> {
        self.bindings[action.index()]
    }

    pub fn set(&mut self, action: KeyAction, chord: Option<KeyChord>) {
        self.bindings[action.index()] = chord;
    }

    pub fn find_conflict(&self, exclude: KeyAction, chord: KeyChord) -> Option<KeyAction> {
        for act in ALL_ACTIONS {
            if act != exclude && self.bindings[act.index()] == Some(chord) {
                return Some(act);
            }
        }
        None
    }

    pub fn match_event(&self, mods: u8, vk: u16) -> Option<KeyAction> {
        let target = KeyChord { mods, vk };
        for act in ALL_ACTIONS {
            if self.bindings[act.index()] == Some(target) {
                return Some(act);
            }
        }
        None
    }

    pub fn format_action(&self, action: KeyAction) -> String {
        match self.get(action) {
            Some(c) => c.format(),
            None => "未绑定".to_string(),
        }
    }

    pub fn return_dp_label(&self) -> String {
        self.get(KeyAction::ReturnDp)
            .map(|c| c.format())
            .unwrap_or_else(|| "Ctrl+Alt+D".to_string())
    }

    pub fn summary_text(&self) -> String {
        let f = |a: KeyAction| self.format_action(a);
        format!(
            "当前遥控快捷键（仅接管电视期间生效，回电脑自动注销）：\r\n\
             • 返回 / 主页 / 菜单 / 语音：{} / {} / {} / {}\r\n\
             • 音量 + / - / 静音：{} / {} / {}\r\n\
             • 亮度 + / -：{} / {}\r\n\
             • 待机息屏 / 切回电脑 DP：{} / {}",
            f(KeyAction::Back),
            f(KeyAction::Home),
            f(KeyAction::Menu),
            f(KeyAction::Voice),
            f(KeyAction::VolUp),
            f(KeyAction::VolDown),
            f(KeyAction::Mute),
            f(KeyAction::BrightnessUp),
            f(KeyAction::BrightnessDown),
            f(KeyAction::Power),
            f(KeyAction::ReturnDp),
        )
    }
}

pub fn is_modifier_vk(vk: u16) -> bool {
    matches!(
        vk,
        VK_CONTROL
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_MENU
            | VK_LMENU
            | VK_RMENU
            | VK_SHIFT
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LWIN
            | VK_RWIN
    )
}

pub fn validate_chord(chord: KeyChord) -> Result<(), &'static str> {
    if is_modifier_vk(chord.vk) {
        return Err("不能只绑定修饰键，请配合其他按键");
    }
    let has_ctrl_or_alt = (chord.mods & (MOD_CTRL | MOD_ALT)) != 0;
    if !has_ctrl_or_alt {
        let is_typing_or_nav = (b'A' as u16..=b'Z' as u16).contains(&chord.vk)
            || (b'0' as u16..=b'9' as u16).contains(&chord.vk)
            || matches!(
                chord.vk,
                VK_SPACE
                    | VK_RETURN
                    | VK_BACK
                    | VK_TAB
                    | VK_LEFT
                    | VK_UP
                    | VK_RIGHT
                    | VK_DOWN
                    | VK_CAPITAL
                    | 0xBA..=0xC0
                    | 0xDB..=0xDF
                    | 0xE2
            );
        if is_typing_or_nav {
            return Err("普通输入键/方向键需配合 Ctrl 或 Alt，以免影响电视打字与导航");
        }
    }
    Ok(())
}

pub fn vk_to_name(vk: u16) -> String {
    if (b'A' as u16..=b'Z' as u16).contains(&vk) || (b'0' as u16..=b'9' as u16).contains(&vk) {
        return ((vk as u8) as char).to_string();
    }
    if (VK_F1..=VK_F24).contains(&vk) {
        return format!("F{}", vk - VK_F1 + 1);
    }
    if (VK_NUMPAD0..=VK_NUMPAD9).contains(&vk) {
        return format!("Num{}", vk - VK_NUMPAD0);
    }
    let s = match vk {
        VK_ESCAPE => "Esc",
        VK_HOME => "Home",
        VK_END => "End",
        VK_INSERT => "Insert",
        VK_DELETE => "Delete",
        VK_PRIOR => "PgUp",
        VK_NEXT => "PgDn",
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        VK_TAB => "Tab",
        VK_SPACE => "Space",
        VK_RETURN => "Enter",
        VK_BACK => "Backspace",
        VK_PAUSE => "Pause",
        VK_SNAPSHOT => "PrtSc",
        VK_SCROLL => "ScrollLock",
        VK_APPS => "Apps",
        VK_VOLUME_UP => "MediaVolUp",
        VK_VOLUME_DOWN => "MediaVolDown",
        VK_VOLUME_MUTE => "MediaMute",
        VK_MEDIA_PLAY_PAUSE => "MediaPlay",
        VK_MEDIA_NEXT_TRACK => "MediaNext",
        VK_MEDIA_PREV_TRACK => "MediaPrev",
        VK_OEM_MINUS => "-",
        VK_OEM_PLUS => "=",
        VK_OEM_4 => "[",
        VK_OEM_6 => "]",
        VK_OEM_5 => "\\",
        VK_OEM_1 => ";",
        VK_OEM_7 => "'",
        VK_OEM_COMMA => ",",
        VK_OEM_PERIOD => ".",
        VK_OEM_2 => "/",
        VK_OEM_3 => "`",
        _ => return format!("VK_0x{:02X}", vk),
    };
    s.to_string()
}

fn name_to_vk(name: &str) -> Option<u16> {
    let t = name.trim();
    if t.len() == 1 {
        let ch = t.as_bytes()[0].to_ascii_uppercase();
        if ch.is_ascii_uppercase() || ch.is_ascii_digit() {
            return Some(ch as u16);
        }
        return match ch as char {
            '-' => Some(VK_OEM_MINUS),
            '=' => Some(VK_OEM_PLUS),
            '[' => Some(VK_OEM_4),
            ']' => Some(VK_OEM_6),
            '\\' => Some(VK_OEM_5),
            ';' => Some(VK_OEM_1),
            '\'' => Some(VK_OEM_7),
            ',' => Some(VK_OEM_COMMA),
            '.' => Some(VK_OEM_PERIOD),
            '/' => Some(VK_OEM_2),
            '`' => Some(VK_OEM_3),
            _ => None,
        };
    }
    let up = t.to_ascii_uppercase();
    if let Some(rest) = up.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Some(VK_F1 + n - 1);
            }
        }
    }
    if let Some(rest) = up.strip_prefix("NUM") {
        if let Ok(n) = rest.parse::<u16>() {
            if n <= 9 {
                return Some(VK_NUMPAD0 + n);
            }
        }
    }
    if let Some(hex) = up.strip_prefix("VK_0X") {
        if let Ok(v) = u16::from_str_radix(hex, 16) {
            return Some(v);
        }
    }
    match up.as_str() {
        "ESC" | "ESCAPE" => Some(VK_ESCAPE),
        "HOME" => Some(VK_HOME),
        "END" => Some(VK_END),
        "INS" | "INSERT" => Some(VK_INSERT),
        "DEL" | "DELETE" => Some(VK_DELETE),
        "PGUP" | "PAGEUP" | "PRIOR" => Some(VK_PRIOR),
        "PGDN" | "PAGEDOWN" | "NEXT" => Some(VK_NEXT),
        "UP" => Some(VK_UP),
        "DOWN" => Some(VK_DOWN),
        "LEFT" => Some(VK_LEFT),
        "RIGHT" => Some(VK_RIGHT),
        "TAB" => Some(VK_TAB),
        "SPACE" => Some(VK_SPACE),
        "ENTER" | "RETURN" => Some(VK_RETURN),
        "BACKSPACE" | "BACK" => Some(VK_BACK),
        "PAUSE" => Some(VK_PAUSE),
        "PRTSC" | "PRINTSCREEN" | "SNAPSHOT" => Some(VK_SNAPSHOT),
        "SCROLLLOCK" => Some(VK_SCROLL),
        "APPS" => Some(VK_APPS),
        "MEDIAVOLUP" => Some(VK_VOLUME_UP),
        "MEDIAVOLDOWN" => Some(VK_VOLUME_DOWN),
        "MEDIAMUTE" => Some(VK_VOLUME_MUTE),
        "MEDIAPLAY" => Some(VK_MEDIA_PLAY_PAUSE),
        "MEDIANEXT" => Some(VK_MEDIA_NEXT_TRACK),
        "MEDIAPREV" => Some(VK_MEDIA_PREV_TRACK),
        _ => None,
    }
}
