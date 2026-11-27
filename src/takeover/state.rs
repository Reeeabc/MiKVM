use crate::adb::DeviceController;
use crate::display::EdgeSide;
use crate::keymap::{KeyMap, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use crate::scrcpy_proto::{
    ScrcpyControlClient, HMOD_LALT, HMOD_LCTRL, HMOD_LSHIFT, HMOD_LWIN, HMOD_RALT, HMOD_RCTRL,
    HMOD_RSHIFT, HMOD_RWIN,
};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize,
    Ordering,
};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::WM_APP;

pub(super) const CREATE_NO_WINDOW: u32 = 0x08000000;

pub const WM_CONTROL_READY: u32 = WM_APP + 10;
pub const WM_CONTROL_EXITED: u32 = WM_APP + 11;
pub const WM_DP_RETURNED: u32 = WM_APP + 12;
pub const WM_TRIGGER_RETURN_DP: u32 = WM_APP + 14;
pub const WM_TRIGGER_TOGGLE_FOCUS: u32 = WM_APP + 15;
pub const WM_EDGE_ENTER_TV: u32 = WM_APP + 16;
pub const WM_TV_CLIPBOARD: u32 = WM_APP + 17;
pub const WM_TV_EDGE_EXIT: u32 = WM_APP + 18;
pub const WM_RECOVERY_FAILED: u32 = WM_APP + 19;
pub const WM_TV_IP_UPDATED: u32 = WM_APP + 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TakeoverMode {
    Idle = 0,
    TvLocked = 1,
    TvFocused = 2,
    TvStandby = 3,
}

impl TakeoverMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => TakeoverMode::TvLocked,
            2 => TakeoverMode::TvFocused,
            3 => TakeoverMode::TvStandby,
            _ => TakeoverMode::Idle,
        }
    }

    pub fn is_tv_capturing(self) -> bool {
        matches!(self, TakeoverMode::TvLocked | TakeoverMode::TvFocused)
    }

    pub fn is_session_active(self) -> bool {
        self != TakeoverMode::Idle
    }
}

pub(super) static MODE_ATOMIC: AtomicU8 = AtomicU8::new(TakeoverMode::Idle as u8);
pub(super) static EDGE_SWITCH_ATOMIC: AtomicBool = AtomicBool::new(true);
pub(super) static EDGE_RES_PC: AtomicI32 = AtomicI32::new(120);
pub(super) static EDGE_RES_TV: AtomicI32 = AtomicI32::new(180);
pub(super) static LAST_WHEEL_MS: AtomicU32 = AtomicU32::new(0);
pub(super) static KEY_HOOK: AtomicIsize = AtomicIsize::new(0);
pub(super) static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
pub(super) static HOST_HWND_GLOBAL: AtomicUsize = AtomicUsize::new(0);
pub(super) static GLOBAL_DEV: Mutex<Option<Arc<DeviceController>>> = Mutex::new(None);
pub(super) static GLOBAL_CLIENT: Mutex<Option<Arc<ScrcpyControlClient>>> = Mutex::new(None);
pub(super) static ACTIVE_KEYMAP: Mutex<Option<KeyMap>> = Mutex::new(None);

// Phase 2 TV-side 3px hover sensor state (lock-free hot path in handle_raw_input!)
// TV strip side: 0=None, 1=LEFT (outward -dx), 2=RIGHT (outward +dx), 3=TOP (-dy), 4=BOTTOM (+dy)
pub(super) const TV_SIDE_NONE: u8 = 0;
pub(super) const TV_SIDE_LEFT: u8 = 1;
pub(super) const TV_SIDE_RIGHT: u8 = 2;
pub(super) const TV_SIDE_TOP: u8 = 3;
pub(super) const TV_SIDE_BOTTOM: u8 = 4;
pub(super) const TV_ENTER_GRACE_MS: u32 = 300;
pub(super) const PUSH_DECAY_MS: u32 = 260;

pub(super) static TV_STRIP_SIDE: AtomicU8 = AtomicU8::new(TV_SIDE_NONE);
pub(super) static TV_EDGE_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(super) static TV_EDGE_RATIO: AtomicU32 = AtomicU32::new(5000);
pub(super) static TV_EDGE_SEQ: AtomicU32 = AtomicU32::new(0);
pub(super) static TV_SCREEN_W: AtomicI32 = AtomicI32::new(1920);
pub(super) static TV_SCREEN_H: AtomicI32 = AtomicI32::new(1080);
pub(super) static TV_EDGE_PUSH_ACCUM: AtomicI32 = AtomicI32::new(0);
pub(super) static TV_EDGE_PUSH_LAST_MS: AtomicU32 = AtomicU32::new(0);
pub(super) static TV_ENTER_GRACE_UNTIL: AtomicU32 = AtomicU32::new(0);
pub(super) static TV_LBUTTON_DOWN: AtomicBool = AtomicBool::new(false);

// PC-side elastic pressure barrier state (resets after 260ms pause)
pub(super) static PC_EDGE_PUSH_ACCUM: AtomicI32 = AtomicI32::new(0);
pub(super) static PC_EDGE_PUSH_LAST_MS: AtomicU32 = AtomicU32::new(0);

// PC -> TV velocity-primed single-frame burst alignment (exact linear Android VelocityControl)
pub(super) static TV_ALIGNING: AtomicBool = AtomicBool::new(false);
pub(super) static TV_ALIGN_GEN: AtomicU32 = AtomicU32::new(0);
pub(super) static ALIGN_USER_DX: AtomicI32 = AtomicI32::new(0);
pub(super) static ALIGN_USER_DY: AtomicI32 = AtomicI32::new(0);
pub(super) static TV_LAST_EXITED_BY_EDGE: AtomicBool = AtomicBool::new(false);
// Primed pointer acceleration x100 (measured 300 = 3.000x on Redmi G Pro 27U Android 14; auto-calibrates on other TVs)
pub(super) static TV_PRIMED_ACCEL_X100: AtomicI32 = AtomicI32::new(300);

#[derive(Clone, Copy)]
pub(super) struct EdgeRuntimeCfg {
    pub(super) side: EdgeSide,
    pub(super) seam: i32,
    pub(super) seam_span: (i32, i32),
    pub(super) is_ghost: bool,
    pub(super) tv_ghost_rect: Option<RECT>,
}

pub(super) static EDGE_CFG: Mutex<Option<EdgeRuntimeCfg>> = Mutex::new(None);
pub(super) static EDGE_DWELL_ACTIVE: AtomicBool = AtomicBool::new(false);
// Tracks physical modifiers pressed while WH_KEYBOARD_LL swallows keys (prevents stuck/missed Ctrl+Alt/Shift!)
pub(super) static HELD_MODS: AtomicU16 = AtomicU16::new(0);

// 256-bit lock-free bitset tracking hotkey-swallowed VKs so KEYUP is cleanly swallowed
static SWALLOWED_VKS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

pub fn current_mode() -> TakeoverMode {
    TakeoverMode::from_u8(MODE_ATOMIC.load(Ordering::Acquire))
}

pub(super) fn tv_strip_for_pc_edge(pc_side: EdgeSide) -> (u8, &'static str) {
    // TV strip lies on the OPPOSITE border of the TV screen facing back toward the PC monitor
    match pc_side {
        EdgeSide::Left => (TV_SIDE_RIGHT, "RIGHT"),
        EdgeSide::Right => (TV_SIDE_LEFT, "LEFT"),
        EdgeSide::Top => (TV_SIDE_BOTTOM, "BOTTOM"),
        EdgeSide::Bottom => (TV_SIDE_TOP, "TOP"),
    }
}

pub(super) fn vk_to_hmod(vk: u16) -> u16 {
    match vk {
        VK_LSHIFT | VK_SHIFT => HMOD_LSHIFT,
        VK_RSHIFT => HMOD_RSHIFT,
        VK_LCONTROL | VK_CONTROL => HMOD_LCTRL,
        VK_RCONTROL => HMOD_RCTRL,
        VK_LMENU | VK_MENU => HMOD_LALT,
        VK_RMENU => HMOD_RALT,
        VK_LWIN => HMOD_LWIN,
        VK_RWIN => HMOD_RWIN,
        _ => 0,
    }
}

/// Used ONLY in TvFocused / TvLocked (strictly isolated from Windows GetAsyncKeyState).
pub(super) fn tv_held_mod_mask() -> u8 {
    let held = HELD_MODS.load(Ordering::Relaxed);
    let mut m = 0u8;
    if (held & (HMOD_LCTRL | HMOD_RCTRL)) != 0 {
        m |= MOD_CTRL;
    }
    if (held & (HMOD_LALT | HMOD_RALT)) != 0 {
        m |= MOD_ALT;
    }
    if (held & (HMOD_LSHIFT | HMOD_RSHIFT)) != 0 {
        m |= MOD_SHIFT;
    }
    m
}

pub(super) fn swallowed_is_set(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].load(Ordering::Relaxed) & mask) != 0
}

pub(super) fn swallowed_test_and_set(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].fetch_or(mask, Ordering::Relaxed) & mask) != 0
}

pub(super) fn swallowed_clear(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].fetch_and(!mask, Ordering::Relaxed) & mask) != 0
}

pub(super) fn swallowed_reset_all() {
    for slot in &SWALLOWED_VKS {
        slot.store(0, Ordering::Relaxed);
    }
}

pub(super) fn get_tick_count() -> u32 {
    unsafe { GetTickCount() }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_mode_transitions_and_flags() {
        assert_eq!(TakeoverMode::from_u8(0), TakeoverMode::Idle);
        assert_eq!(TakeoverMode::from_u8(1), TakeoverMode::TvLocked);
        assert_eq!(TakeoverMode::from_u8(2), TakeoverMode::TvFocused);
        assert_eq!(TakeoverMode::from_u8(3), TakeoverMode::TvStandby);
        assert_eq!(TakeoverMode::from_u8(99), TakeoverMode::Idle);

        assert!(!TakeoverMode::Idle.is_session_active());
        assert!(!TakeoverMode::Idle.is_tv_capturing());
        assert!(TakeoverMode::TvLocked.is_tv_capturing());
        assert!(TakeoverMode::TvFocused.is_tv_capturing());
        assert!(TakeoverMode::TvStandby.is_session_active());
        assert!(!TakeoverMode::TvStandby.is_tv_capturing());
    }

    #[test]
    fn tv_strip_side_is_opposite_of_pc_edge() {
        assert_eq!(tv_strip_for_pc_edge(EdgeSide::Left), (TV_SIDE_RIGHT, "RIGHT"));
        assert_eq!(tv_strip_for_pc_edge(EdgeSide::Right), (TV_SIDE_LEFT, "LEFT"));
        assert_eq!(tv_strip_for_pc_edge(EdgeSide::Top), (TV_SIDE_BOTTOM, "BOTTOM"));
        assert_eq!(tv_strip_for_pc_edge(EdgeSide::Bottom), (TV_SIDE_TOP, "TOP"));
    }

    #[test]
    fn swallowed_vk_bitset_operations() {
        swallowed_reset_all();
        assert!(!swallowed_is_set(0x53));
        assert!(!swallowed_test_and_set(0x53));
        assert!(swallowed_is_set(0x53));
        assert!(swallowed_test_and_set(0x53));
        assert!(swallowed_clear(0x53));
        assert!(!swallowed_is_set(0x53));
        assert!(!swallowed_clear(0x53));
        assert!(!swallowed_is_set(300));
    }
}
