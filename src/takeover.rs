use crate::adb::DeviceController;
use crate::clipboard::{clipboard_sequence, read_pc_clipboard_text, write_pc_clipboard_text};
use crate::config::{to_wide, AppConfig};
use crate::display::{
    is_foreground_fullscreen_on_pc, pick_visible_pc_window, query_display_topology,
    window_on_rect, DisplayTopology, EdgeSide, SavedDisplayConfig,
};
use crate::keymap::{KeyAction, KeyMap, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use crate::scrcpy_proto::{
    android_metastate_from_held, vk_to_android_keycode, ScrcpyControlClient, ACTION_DOWN,
    ACTION_UP, HMOD_LALT, HMOD_LCTRL, HMOD_LSHIFT, HMOD_LWIN, HMOD_RALT, HMOD_RCTRL, HMOD_RSHIFT,
    HMOD_RWIN, MOUSE_BTN_LEFT, MOUSE_BTN_RIGHT,
};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const LLKHF_INJECTED_FLAG: u32 = 0x0000_0010;
const LLKHF_EXTENDED_FLAG: u32 = 0x0000_0001;
const LLMHF_INJECTED_FLAG: u32 = 0x0000_0001;
const MOUSE_MOVE_ABSOLUTE_FLAG: u16 = 0x0001;

const EDGE_CLASS: &str = "MiKVMEdgeTriggerWnd";
const TIMER_EDGE_DWELL: usize = 601;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
const EDGE_PUSH_PX: i32 = 12;
const EDGE_DWELL_GHOST_MS: u32 = 45;
const EDGE_DWELL_BORDER_MS: u32 = 60;

pub const WM_CONTROL_READY: u32 = WM_APP + 10;
pub const WM_CONTROL_EXITED: u32 = WM_APP + 11;
pub const WM_DP_RETURNED: u32 = WM_APP + 12;
pub const WM_TRIGGER_RETURN_DP: u32 = WM_APP + 14;
pub const WM_TRIGGER_TOGGLE_FOCUS: u32 = WM_APP + 15;
pub const WM_EDGE_ENTER_TV: u32 = WM_APP + 16;
pub const WM_TV_CLIPBOARD: u32 = WM_APP + 17;
// Phase 2 hook: TV-side 2px edge overlay exit notification (wp = y_ratio * 10000)
#[allow(dead_code)]
pub const WM_TV_EDGE_EXIT: u32 = WM_APP + 18;

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

static MODE_ATOMIC: AtomicU8 = AtomicU8::new(TakeoverMode::Idle as u8);
static WHEEL_AS_DPAD: AtomicBool = AtomicBool::new(true);
static LAST_WHEEL_MS: AtomicU32 = AtomicU32::new(0);
static KEY_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static HOST_HWND_GLOBAL: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_DEV: Mutex<Option<Arc<DeviceController>>> = Mutex::new(None);
static GLOBAL_CLIENT: Mutex<Option<Arc<ScrcpyControlClient>>> = Mutex::new(None);
static ACTIVE_KEYMAP: Mutex<Option<KeyMap>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct EdgeRuntimeCfg {
    side: EdgeSide,
    seam: i32,
    is_ghost: bool,
    tv_ghost_rect: Option<RECT>,
}

static EDGE_CFG: Mutex<Option<EdgeRuntimeCfg>> = Mutex::new(None);
static EDGE_DWELL_ACTIVE: AtomicBool = AtomicBool::new(false);
// Tracks physical modifiers pressed while WH_KEYBOARD_LL swallows keys (prevents stuck/missed Ctrl+Alt/Shift!)
static HELD_MODS: AtomicU16 = AtomicU16::new(0);

fn vk_to_hmod(vk: u16) -> u16 {
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

/// Transfers any modifiers held during TvFocused into SWALLOWED_VKS before leaving capture,
/// so releasing Ctrl+Alt after pressing Ctrl+Alt+S doesn't leak stray KEYUPs to Windows.
fn swallow_currently_held_modifiers() {
    let held = HELD_MODS.swap(0, Ordering::AcqRel);
    if (held & HMOD_LSHIFT) != 0 {
        swallowed_test_and_set(VK_LSHIFT);
    }
    if (held & HMOD_RSHIFT) != 0 {
        swallowed_test_and_set(VK_RSHIFT);
    }
    if (held & HMOD_LCTRL) != 0 {
        swallowed_test_and_set(VK_LCONTROL);
    }
    if (held & HMOD_RCTRL) != 0 {
        swallowed_test_and_set(VK_RCONTROL);
    }
    if (held & HMOD_LALT) != 0 {
        swallowed_test_and_set(VK_LMENU);
    }
    if (held & HMOD_RALT) != 0 {
        swallowed_test_and_set(VK_RMENU);
    }
    if (held & HMOD_LWIN) != 0 {
        swallowed_test_and_set(VK_LWIN);
    }
    if (held & HMOD_RWIN) != 0 {
        swallowed_test_and_set(VK_RWIN);
    }
}

// 256-bit lock-free bitset tracking hotkey-swallowed VKs so KEYUP is cleanly swallowed
static SWALLOWED_VKS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn swallowed_test_and_set(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].fetch_or(mask, Ordering::Relaxed) & mask) != 0
}

fn swallowed_clear(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].fetch_and(!mask, Ordering::Relaxed) & mask) != 0
}

fn swallowed_reset_all() {
    for slot in &SWALLOWED_VKS {
        slot.store(0, Ordering::Relaxed);
    }
}

pub fn current_mode() -> TakeoverMode {
    TakeoverMode::from_u8(MODE_ATOMIC.load(Ordering::Acquire))
}

pub struct TakeoverSession {
    scrcpy_dir: PathBuf,
    adb_path: PathBuf,
    cfg: Arc<Mutex<AppConfig>>,
    dev: Arc<DeviceController>,
    host_hwnd: usize,
    edge_hwnd: usize,
    client: Mutex<Option<Arc<ScrcpyControlClient>>>,
    watch_stop: Mutex<Option<Arc<AtomicBool>>>,
    lid_open: AtomicBool,
    topo: DisplayTopology,
    saved_display: SavedDisplayConfig,
    last_pc_hwnd: usize,
    raw_input_active: bool,
    fence_active: bool,
    last_clip_seq: u32,
    last_synced_clip: String,
}

impl TakeoverSession {
    pub fn new(
        scrcpy_dir: PathBuf,
        cfg: Arc<Mutex<AppConfig>>,
        dev: Arc<DeviceController>,
        host_hwnd: HWND,
    ) -> Self {
        let adb_path = scrcpy_dir.join("adb.exe");
        HOST_HWND_GLOBAL.store(host_hwnd as usize, Ordering::Release);
        if let Ok(mut g) = GLOBAL_DEV.lock() {
            *g = Some(Arc::clone(&dev));
        }
        let (tv_hint, fallback_side, wheel_on, keymap) = cfg
            .lock()
            .map(|c| {
                (
                    c.tv_monitor_hint.clone(),
                    c.tv_edge_side,
                    c.wheel_as_dpad,
                    c.keymap.clone(),
                )
            })
            .unwrap_or_else(|_| ("Mi Monitor".to_string(), EdgeSide::Left, true, KeyMap::default()));
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        WHEEL_AS_DPAD.store(wheel_on, Ordering::Relaxed);

        let edge_hwnd = create_edge_trigger_window();
        let topo = query_display_topology(&tv_hint, true, fallback_side);

        Self {
            scrcpy_dir,
            adb_path,
            cfg,
            dev,
            host_hwnd: host_hwnd as usize,
            edge_hwnd: edge_hwnd as usize,
            client: Mutex::new(None),
            watch_stop: Mutex::new(None),
            lid_open: AtomicBool::new(true),
            topo,
            saved_display: SavedDisplayConfig::default(),
            last_pc_hwnd: 0,
            raw_input_active: false,
            fence_active: false,
            last_clip_seq: clipboard_sequence(),
            last_synced_clip: String::new(),
        }
    }

    pub fn mode(&self) -> TakeoverMode {
        current_mode()
    }

    pub fn is_running(&self) -> bool {
        if let Ok(g) = self.client.lock() {
            if let Some(ref c) = *g {
                return c.is_alive();
            }
        }
        false
    }

    pub fn is_lid_open(&self) -> bool {
        self.lid_open.load(Ordering::Acquire)
    }

    pub fn sync_config(&mut self) {
        let (wheel_on, keymap) = self
            .cfg
            .lock()
            .map(|c| (c.wheel_as_dpad, c.keymap.clone()))
            .unwrap_or_else(|_| (true, KeyMap::default()));
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        WHEEL_AS_DPAD.store(wheel_on, Ordering::Relaxed);
        if self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        }
    }

    fn refresh_topology(&mut self) {
        let (hint, fallback) = self
            .cfg
            .lock()
            .map(|c| (c.tv_monitor_hint.clone(), c.tv_edge_side))
            .unwrap_or_else(|_| ("Mi Monitor".to_string(), EdgeSide::Left));
        let lid = self.lid_open.load(Ordering::Acquire);
        self.topo = query_display_topology(&hint, lid, fallback);
        // Remember auto-detected physical side whenever Extend mode gives us a real seam
        if self.topo.edge_trigger_is_ghost {
            if let Some(side) = self.topo.edge_side {
                if let Ok(mut c) = self.cfg.lock() {
                    if c.tv_edge_side != side {
                        c.tv_edge_side = side;
                    }
                }
            }
        }
    }

    /// Blocking connect called on background worker thread; posts WM_CONTROL_READY on success.
    pub fn start_control_blocking(&self) -> Result<(), String> {
        if self.is_running() {
            unsafe {
                PostMessageW(self.host_hwnd as HWND, WM_CONTROL_READY, 0, 0);
            }
            return Ok(());
        }
        let server_path = self.scrcpy_dir.join("scrcpy-server");
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();

        let host_clip = self.host_hwnd;
        let host_exit = self.host_hwnd;

        let client = ScrcpyControlClient::connect(
            &self.dev,
            self.adb_path.clone(),
            server_path,
            addr,
            move |text| {
                let boxed = Box::into_raw(Box::new(text)) as LPARAM;
                unsafe {
                    PostMessageW(host_clip as HWND, WM_TV_CLIPBOARD, 0, boxed);
                }
            },
            move || unsafe {
                PostMessageW(host_exit as HWND, WM_CONTROL_EXITED, 0, 0);
            },
        )?;

        if let Ok(mut g) = self.client.lock() {
            *g = Some(Arc::clone(&client));
        }
        if let Ok(mut gg) = GLOBAL_CLIENT.lock() {
            *gg = Some(client);
        }

        unsafe {
            PostMessageW(self.host_hwnd as HWND, WM_CONTROL_READY, 0, 0);
        }
        Ok(())
    }

    pub fn on_control_ready_ui(&mut self) {
        self.sync_config();
        let (auto_internal, tv_hint) = self
            .cfg
            .lock()
            .map(|c| (c.auto_internal_on_tv, c.tv_monitor_hint.clone()))
            .unwrap_or((false, "Mi Monitor".to_string()));
        if auto_internal {
            let _ = self
                .saved_display
                .switch_to_internal_if_extended(&tv_hint, self.is_lid_open());
        }
        self.refresh_topology();
        self.capture_last_pc_window();

        self.dev.start_persistent_shell();
        self.dev.set_keep_warm(true);

        if self.topo.has_visible_pc() {
            self.enter_mode(TakeoverMode::TvFocused);
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
    }

    pub fn toggle_focus_ui(&mut self) -> TakeoverMode {
        match self.mode() {
            TakeoverMode::TvFocused => {
                self.refresh_topology();
                if self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvStandby);
                } else {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvStandby => {
                self.refresh_topology();
                self.capture_last_pc_window();
                if self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvFocused);
                } else {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvLocked => {
                // Re-check if user opened lid or plugged a screen
                self.refresh_topology();
                if self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvStandby);
                }
            }
            TakeoverMode::Idle => {}
        }
        self.mode()
    }

    pub fn on_edge_enter_tv_ui(&mut self) {
        if self.mode() != TakeoverMode::TvStandby {
            return;
        }
        self.refresh_topology();
        self.capture_last_pc_window();
        if self.topo.has_visible_pc() {
            self.enter_mode(TakeoverMode::TvFocused);
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
    }

    /// Phase 2 entry point: called when TV-side 2px edge overlay reports pointer exit at `y_ratio`.
    pub fn on_tv_edge_exit_ui(&mut self, y_ratio_10k: usize) {
        if self.mode() != TakeoverMode::TvFocused {
            return;
        }
        self.refresh_topology();
        if !self.topo.has_visible_pc() {
            return;
        }
        let ratio = (y_ratio_10k.min(10000) as f32) / 10000.0;
        let pc = &self.topo.visible_pc_monitors[0];
        let mut pt = self.topo.return_cursor_pt;
        match self.topo.edge_side.unwrap_or(EdgeSide::Left) {
            EdgeSide::Left | EdgeSide::Right => {
                pt.y = pc.rect.top + ((pc.rect.bottom - pc.rect.top) as f32 * ratio) as i32;
            }
            EdgeSide::Top | EdgeSide::Bottom => {
                pt.x = pc.rect.left + ((pc.rect.right - pc.rect.left) as f32 * ratio) as i32;
            }
        }
        self.topo.return_cursor_pt = pt;
        self.enter_mode(TakeoverMode::TvStandby);
    }

    pub fn on_lid_state_changed(&mut self, is_open: bool) {
        let prev = self.lid_open.swap(is_open, Ordering::AcqRel);
        if prev != is_open && self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        }
    }

    pub fn on_display_changed(&mut self) {
        if self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        } else {
            self.refresh_topology();
        }
    }

    fn refresh_topology_and_reconcile(&mut self) {
        self.refresh_topology();
        let cur = self.mode();
        match cur {
            TakeoverMode::TvLocked => {
                if self.topo.has_visible_pc() {
                    // User opened laptop lid or switched to Extend -> unlock multi-screen!
                    self.enter_mode(TakeoverMode::TvFocused);
                }
            }
            TakeoverMode::TvFocused => {
                if !self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvStandby => {
                if !self.topo.has_visible_pc() {
                    // User closed laptop lid while working on PC -> pull focus cleanly to TV!
                    self.enter_mode(TakeoverMode::TvLocked);
                } else {
                    self.apply_standby_edge_or_fence();
                }
            }
            TakeoverMode::Idle => {}
        }
    }

    fn enter_mode(&mut self, next: TakeoverMode) {
        let prev = self.mode();
        MODE_ATOMIC.store(next as u8, Ordering::Release);

        match next {
            TakeoverMode::Idle => {
                HELD_MODS.store(0, Ordering::Release);
                self.release_fence();
                self.hide_edge_trigger();
                self.set_raw_input(false);
                self.uninstall_mouse_hook();
                self.uninstall_keyboard_hook();
            }
            TakeoverMode::TvLocked | TakeoverMode::TvFocused => {
                HELD_MODS.store(0, Ordering::Release);
                self.release_fence();
                self.hide_edge_trigger();
                // Park Windows cursor neatly inside visible PC edge if returning from standby
                if next == TakeoverMode::TvFocused {
                    let pt = self.topo.return_cursor_pt;
                    unsafe {
                        SetCursorPos(pt.x, pt.y);
                    }
                }
                self.install_keyboard_hook();
                self.install_mouse_hook();
                self.set_raw_input(true);
                self.sync_pc_clipboard_to_tv_if_changed();
            }
            TakeoverMode::TvStandby => {
                // Release any held keys/buttons on TV first so zero stuck keys!
                if prev.is_tv_capturing() {
                    swallow_currently_held_modifiers();
                    if let Ok(g) = self.client.lock() {
                        if let Some(ref c) = *g {
                            c.release_all_inputs();
                        }
                    }
                } else {
                    HELD_MODS.store(0, Ordering::Release);
                }
                self.set_raw_input(false);
                self.uninstall_mouse_hook();
                self.install_keyboard_hook(); // Keeps only ToggleFocus / ReturnDp active!
                // Place cursor safely inside visible PC monitor & restore focus to a visible PC window
                let pt = self.topo.return_cursor_pt;
                unsafe {
                    SetCursorPos(pt.x, pt.y);
                }
                self.restore_visible_pc_focus();
                self.apply_standby_edge_or_fence();
            }
        }
    }

    fn apply_standby_edge_or_fence(&mut self) {
        let edge_on = self
            .cfg
            .lock()
            .map(|c| c.edge_switch)
            .unwrap_or(true);
        if edge_on {
            self.release_fence();
            self.show_edge_trigger();
        } else {
            self.hide_edge_trigger();
            if let Some(r) = self.topo.pc_fence_rect {
                unsafe {
                    ClipCursor(&r);
                }
                self.fence_active = true;
            } else {
                self.release_fence();
            }
        }
    }

    fn release_fence(&mut self) {
        if self.fence_active {
            unsafe {
                ClipCursor(ptr::null());
            }
            self.fence_active = false;
        }
    }

    fn show_edge_trigger(&self) {
        let hwnd = self.edge_hwnd as HWND;
        if hwnd.is_null() {
            return;
        }
        let Some(rect) = self.topo.edge_trigger_rect else {
            self.hide_edge_trigger();
            return;
        };
        let Some(side) = self.topo.edge_side else {
            self.hide_edge_trigger();
            return;
        };
        if let Ok(mut g) = EDGE_CFG.lock() {
            *g = Some(EdgeRuntimeCfg {
                side,
                seam: self.topo.seam_coord,
                is_ghost: self.topo.edge_trigger_is_ghost,
                tv_ghost_rect: self.topo.tv_ghost_monitor.as_ref().map(|m| m.rect),
            });
        }
        EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
        unsafe {
            KillTimer(hwnd, TIMER_EDGE_DWELL);
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                (rect.right - rect.left).max(1),
                (rect.bottom - rect.top).max(1),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn hide_edge_trigger(&self) {
        let hwnd = self.edge_hwnd as HWND;
        EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
        if !hwnd.is_null() {
            unsafe {
                KillTimer(hwnd, TIMER_EDGE_DWELL);
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    fn capture_last_pc_window(&mut self) {
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() || fg == self.host_hwnd as HWND || fg == self.edge_hwnd as HWND {
                return;
            }
            let on_ghost = self
                .topo
                .tv_ghost_monitor
                .as_ref()
                .map(|m| window_on_rect(fg, &m.rect))
                .unwrap_or(false);
            if !on_ghost {
                self.last_pc_hwnd = fg as usize;
            }
        }
    }

    fn restore_visible_pc_focus(&self) {
        let ghost = self.topo.tv_ghost_monitor.as_ref().map(|m| m.rect);
        unsafe {
            let mut target: HWND = self.last_pc_hwnd as HWND;
            let valid = !target.is_null()
                && IsWindow(target) != 0
                && IsWindowVisible(target) != 0
                && !ghost.map(|g| window_on_rect(target, &g)).unwrap_or(false);
            if !valid {
                target = pick_visible_pc_window(self.edge_hwnd as HWND, ghost)
                    .unwrap_or(ptr::null_mut());
            }
            if !target.is_null() {
                SetForegroundWindow(target);
            }
        }
    }

    fn sync_pc_clipboard_to_tv_if_changed(&mut self) {
        let enabled = self
            .cfg
            .lock()
            .map(|c| c.clipboard_sync)
            .unwrap_or(true);
        if !enabled {
            return;
        }
        let seq = clipboard_sequence();
        if seq == self.last_clip_seq {
            return;
        }
        self.last_clip_seq = seq;
        if let Some(text) = read_pc_clipboard_text() {
            if text != self.last_synced_clip {
                self.last_synced_clip = text.clone();
                if let Ok(g) = self.client.lock() {
                    if let Some(ref c) = *g {
                        c.set_tv_clipboard(&text, false);
                    }
                }
            }
        }
    }

    pub fn on_tv_clipboard_ui(&mut self, text: String) {
        let enabled = self
            .cfg
            .lock()
            .map(|c| c.clipboard_sync)
            .unwrap_or(true);
        if !enabled || text.is_empty() || text == self.last_synced_clip {
            return;
        }
        if let Some(new_seq) = write_pc_clipboard_text(&text) {
            self.last_clip_seq = new_seq;
            self.last_synced_clip = text;
        }
    }

    /// Optimized DP Watcher: reuses a single persistent `adb shell` stream instead of spawning
    /// ~5500 short-lived `adb.exe` processes per hour during long multi-screen sessions!
    pub fn start_dp_return_watcher(&self) {
        self.stop_dp_return_watcher();
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut g) = self.watch_stop.lock() {
            *g = Some(Arc::clone(&stop));
        }

        let dev = Arc::clone(&self.dev);
        let adb_path = self.adb_path.clone();
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();
        let host = self.host_hwnd;

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

                let is_ext = out.to_ascii_lowercase().contains("externalsourceactivity");
                if !is_ext && !out.trim().is_empty() {
                    saw_non_ext = true;
                    consecutive_ext = 0;
                } else if is_ext {
                    consecutive_ext += 1;
                    if saw_non_ext || consecutive_ext >= 2 {
                        unsafe {
                            PostMessageW(host as HWND, WM_DP_RETURNED, 0, 0);
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

    pub fn stop_dp_return_watcher(&self) {
        if let Ok(mut g) = self.watch_stop.lock() {
            if let Some(flag) = g.take() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn stop(&mut self) {
        self.enter_mode(TakeoverMode::Idle);
        self.stop_dp_return_watcher();
        self.dev.stop_persistent_shell();
        self.dev.set_keep_warm(false);

        if let Ok(mut gg) = GLOBAL_CLIENT.lock() {
            *gg = None;
        }
        if let Ok(mut g) = self.client.lock() {
            if let Some(c) = g.take() {
                c.close();
            }
        }
        self.saved_display.restore_if_needed();
    }

    fn set_raw_input(&mut self, enable: bool) {
        if self.raw_input_active == enable {
            return;
        }
        unsafe {
            let rid = RAWINPUTDEVICE {
                usUsagePage: 0x01, // HID_USAGE_PAGE_GENERIC
                usUsage: 0x02,     // HID_USAGE_GENERIC_MOUSE
                dwFlags: if enable { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
                hwndTarget: if enable {
                    self.host_hwnd as HWND
                } else {
                    ptr::null_mut()
                },
            };
            let ok = RegisterRawInputDevices(
                &rid,
                1,
                std::mem::size_of::<RAWINPUTDEVICE>() as u32,
            );
            if ok != 0 || !enable {
                self.raw_input_active = enable;
            }
        }
    }

    fn install_keyboard_hook(&mut self) {
        if KEY_HOOK.load(Ordering::Relaxed) != 0 {
            return;
        }
        swallowed_reset_all();
        unsafe {
            let hmod = GetModuleHandleW(ptr::null());
            let hk = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hmod, 0);
            KEY_HOOK.store(hk as isize, Ordering::Release);
        }
    }

    fn uninstall_keyboard_hook(&mut self) {
        swallowed_reset_all();
        let hk = KEY_HOOK.swap(0, Ordering::AcqRel) as HHOOK;
        if !hk.is_null() {
            unsafe {
                UnhookWindowsHookEx(hk);
            }
        }
    }

    fn install_mouse_hook(&mut self) {
        if MOUSE_HOOK.load(Ordering::Relaxed) != 0 {
            return;
        }
        unsafe {
            let hmod = GetModuleHandleW(ptr::null());
            let hk = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hmod, 0);
            MOUSE_HOOK.store(hk as isize, Ordering::Release);
        }
    }

    fn uninstall_mouse_hook(&mut self) {
        let hk = MOUSE_HOOK.swap(0, Ordering::AcqRel) as HHOOK;
        if !hk.is_null() {
            unsafe {
                UnhookWindowsHookEx(hk);
            }
        }
    }
}

/// Called from host_wnd_proc on WM_INPUT to forward high-precision relative mouse deltas to TV.
pub fn handle_raw_input(lp: LPARAM) {
    if !current_mode().is_tv_capturing() {
        return;
    }
    unsafe {
        let mut raw: RAWINPUT = std::mem::zeroed();
        let mut sz = std::mem::size_of::<RAWINPUT>() as u32;
        let hdr_sz = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let got = GetRawInputData(
            lp as HRAWINPUT,
            RID_INPUT,
            &mut raw as *mut _ as *mut _,
            &mut sz,
            hdr_sz,
        );
        if got == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
            return;
        }
        let m = raw.data.mouse;
        if (m.usFlags & MOUSE_MOVE_ABSOLUTE_FLAG) != 0 {
            return;
        }
        let dx = m.lLastX;
        let dy = m.lLastY;
        if dx == 0 && dy == 0 {
            return;
        }
        if let Ok(g) = GLOBAL_CLIENT.try_lock() {
            if let Some(ref client) = *g {
                client.send_mouse_move(dx, dy);
            }
        }
    }
}

unsafe fn current_mod_mask() -> u8 {
    let held = HELD_MODS.load(Ordering::Relaxed);
    let mut m = 0u8;
    if (held & (HMOD_LCTRL | HMOD_RCTRL)) != 0
        || (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0
    {
        m |= MOD_CTRL;
    }
    if (held & (HMOD_LALT | HMOD_RALT)) != 0
        || (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0
    {
        m |= MOD_ALT;
    }
    if (held & (HMOD_LSHIFT | HMOD_RSHIFT)) != 0
        || (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0
    {
        m |= MOD_SHIFT;
    }
    m
}

fn dispatch_key_action(action: KeyAction) {
    match action {
        KeyAction::ReturnDp => {
            let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
            if host != 0 {
                unsafe {
                    PostMessageW(host as HWND, WM_TRIGGER_RETURN_DP, 0, 0);
                }
            }
        }
        KeyAction::ToggleFocus => {
            let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
            if host != 0 {
                unsafe {
                    PostMessageW(host as HWND, WM_TRIGGER_TOGGLE_FOCUS, 0, 0);
                }
            }
        }
        KeyAction::Voice => {
            if let Ok(g) = GLOBAL_DEV.try_lock() {
                if let Some(ref dev) = *g {
                    dev.trigger_voice_assistant();
                }
            }
        }
        KeyAction::BrightnessUp => {
            if let Ok(g) = GLOBAL_DEV.try_lock() {
                if let Some(ref dev) = *g {
                    dev.queue_step_backlight(5);
                }
            }
        }
        KeyAction::BrightnessDown => {
            if let Ok(g) = GLOBAL_DEV.try_lock() {
                if let Some(ref dev) = *g {
                    dev.queue_step_backlight(-5);
                }
            }
        }
        other => {
            if let Some(kc) = other.android_keycode() {
                let mut sent_direct = false;
                if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                    if let Some(ref client) = *g {
                        if client.is_alive() {
                            client.tap_keycode(kc);
                            sent_direct = true;
                        }
                    }
                }
                if !sent_direct {
                    if let Ok(g) = GLOBAL_DEV.try_lock() {
                        if let Some(ref dev) = *g {
                            dev.send_tv_key_fast(kc);
                        }
                    }
                }
            }
        }
    }
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let mode = current_mode();
    if code >= 0 && mode.is_session_active() {
        let kb = &*(lp as *const KBDLLHOOKSTRUCT);
        if (kb.flags & LLKHF_INJECTED_FLAG) == 0 {
            let vk = kb.vkCode as u16;
            let msg = wp as u32;
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if mode == TakeoverMode::TvStandby {
                // Only intercept standby-global hotkeys (ReturnDp & ToggleFocus); pass all other keys to PC!
                if is_down {
                    let mods = current_mod_mask();
                    let matched = ACTIVE_KEYMAP
                        .try_lock()
                        .ok()
                        .and_then(|g| g.as_ref().and_then(|km| km.match_event(mods, vk)))
                        .filter(|a| a.is_standby_global());
                    if let Some(action) = matched {
                        let was_down = swallowed_test_and_set(vk);
                        if !was_down {
                            dispatch_key_action(action);
                        }
                        return 1;
                    }
                } else if is_up && swallowed_clear(vk) {
                    return 1;
                }
            } else if mode.is_tv_capturing() {
                // Capture EVERY physical key for TV -> zero Windows focus / IME disturbance!
                let hmod_bit = vk_to_hmod(vk);
                if is_down {
                    if hmod_bit != 0 {
                        HELD_MODS.fetch_or(hmod_bit, Ordering::Relaxed);
                    }
                    let mods = current_mod_mask();
                    let matched = ACTIVE_KEYMAP
                        .try_lock()
                        .ok()
                        .and_then(|g| g.as_ref().and_then(|km| km.match_event(mods, vk)));
                    if let Some(action) = matched {
                        let was_down = swallowed_test_and_set(vk);
                        if !was_down || action.is_repeatable() {
                            dispatch_key_action(action);
                        }
                        return 1;
                    }
                    let extended = (kb.flags & LLKHF_EXTENDED_FLAG) != 0;
                    if let Some(kc) = vk_to_android_keycode(vk, extended) {
                        let meta = android_metastate_from_held(HELD_MODS.load(Ordering::Relaxed));
                        if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                            if let Some(ref client) = *g {
                                client.send_keycode(ACTION_DOWN, kc, 0, meta);
                            }
                        }
                    }
                    return 1;
                } else if is_up {
                    if hmod_bit != 0 {
                        HELD_MODS.fetch_and(!hmod_bit, Ordering::Relaxed);
                    }
                    if swallowed_clear(vk) {
                        return 1;
                    }
                    let extended = (kb.flags & LLKHF_EXTENDED_FLAG) != 0;
                    if let Some(kc) = vk_to_android_keycode(vk, extended) {
                        let meta = android_metastate_from_held(HELD_MODS.load(Ordering::Relaxed));
                        if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                            if let Some(ref client) = *g {
                                client.send_keycode(ACTION_UP, kc, 0, meta);
                            }
                        }
                    }
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(KEY_HOOK.load(Ordering::Relaxed) as HHOOK, code, wp, lp)
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && current_mode().is_tv_capturing() {
        let ms = &*(lp as *const MSLLHOOKSTRUCT);
        if (ms.flags & LLMHF_INJECTED_FLAG) == 0 {
            let msg = wp as u32;
            match msg {
                WM_MOUSEMOVE => {
                    // Relative deltas are delivered at full fidelity via WM_INPUT; freeze Windows cursor.
                    return 1;
                }
                WM_LBUTTONDOWN => {
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.send_mouse_button(MOUSE_BTN_LEFT, true);
                        }
                    }
                    return 1;
                }
                WM_LBUTTONUP => {
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.send_mouse_button(MOUSE_BTN_LEFT, false);
                        }
                    }
                    return 1;
                }
                WM_RBUTTONDOWN => {
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.send_mouse_button(MOUSE_BTN_RIGHT, true);
                        }
                    }
                    return 1;
                }
                WM_RBUTTONUP => {
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.send_mouse_button(MOUSE_BTN_RIGHT, false);
                        }
                    }
                    return 1;
                }
                WM_MBUTTONDOWN => {
                    // Preserves MiKVM default +hbh mapping: Middle button -> HOME (3)
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.tap_keycode(3);
                        }
                    }
                    return 1;
                }
                WM_MBUTTONUP => return 1,
                WM_XBUTTONDOWN => {
                    let xbtn = ((ms.mouseData >> 16) & 0xFFFF) as u16;
                    // XBUTTON1 (Back) -> BACK (4), XBUTTON2 (Forward) -> HOME (3)
                    let kc = if xbtn == 1 { 4 } else { 3 };
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.tap_keycode(kc);
                        }
                    }
                    return 1;
                }
                WM_XBUTTONUP => return 1,
                WM_MOUSEWHEEL => {
                    let delta = ((ms.mouseData >> 16) & 0xFFFF) as i16;
                    if delta != 0 {
                        if WHEEL_AS_DPAD.load(Ordering::Relaxed) {
                            let now = GetTickCount();
                            let prev = LAST_WHEEL_MS.load(Ordering::Relaxed);
                            if now.wrapping_sub(prev) >= 50 {
                                LAST_WHEEL_MS.store(now, Ordering::Relaxed);
                                let kc = if delta > 0 { 19 } else { 20 };
                                if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                                    if let Some(ref c) = *g {
                                        c.tap_keycode(kc);
                                    }
                                }
                            }
                        } else {
                            let steps = (delta / 120).clamp(-10, 10) as i8;
                            let steps = if steps == 0 {
                                if delta > 0 { 1 } else { -1 }
                            } else {
                                steps
                            };
                            if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                                if let Some(ref c) = *g {
                                    c.send_mouse_wheel(steps, 0);
                                }
                            }
                        }
                    }
                    return 1;
                }
                WM_MOUSEHWHEEL => {
                    let delta = ((ms.mouseData >> 16) & 0xFFFF) as i16;
                    if delta != 0 {
                        let steps = (delta / 120).clamp(-10, 10) as i8;
                        let steps = if steps == 0 {
                            if delta > 0 { 1 } else { -1 }
                        } else {
                            steps
                        };
                        if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                            if let Some(ref c) = *g {
                                c.send_mouse_wheel(0, steps);
                            }
                        }
                    }
                    return 1;
                }
                _ => {}
            }
        }
    }
    CallNextHookEx(MOUSE_HOOK.load(Ordering::Relaxed) as HHOOK, code, wp, lp)
}

fn create_edge_trigger_window() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(EDGE_CLASS);
        static REG: std::sync::Once = std::sync::Once::new();
        REG.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(edge_wnd_proc),
                hInstance: hinst,
                hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                lpszClassName: cls_w.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            cls_w.as_ptr(),
            to_wide("MiKVM Edge").as_ptr(),
            WS_POPUP,
            0,
            0,
            2,
            2,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        if !hwnd.is_null() {
            SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
        }
        hwnd
    }
}

fn should_suppress_edge_crossing(tv_ghost_rect: Option<RECT>) -> bool {
    unsafe {
        // Don't hijack while user is dragging a window or selecting text with left mouse button
        if (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 {
            return true;
        }
        if is_foreground_fullscreen_on_pc(tv_ghost_rect) {
            return true;
        }
    }
    false
}

fn fire_edge_enter(hwnd: HWND) {
    EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
    unsafe {
        KillTimer(hwnd, TIMER_EDGE_DWELL);
    }
    let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
    if host != 0 {
        unsafe {
            PostMessageW(host as HWND, WM_EDGE_ENTER_TV, 0, 0);
        }
    }
}

unsafe extern "system" fn edge_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            if current_mode() != TakeoverMode::TvStandby {
                return 0;
            }
            let cfg = EDGE_CFG.try_lock().ok().and_then(|g| *g);
            let Some(cfg) = cfg else {
                return 0;
            };
            if should_suppress_edge_crossing(cfg.tv_ghost_rect) {
                return 0;
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);

            if cfg.is_ghost {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let depth = match cfg.side {
                    EdgeSide::Left => cfg.seam - pt.x,
                    EdgeSide::Right => pt.x - cfg.seam,
                    EdgeSide::Top => cfg.seam - pt.y,
                    EdgeSide::Bottom => pt.y - cfg.seam,
                };
                if depth >= EDGE_PUSH_PX {
                    fire_edge_enter(hwnd);
                    return 0;
                }
                if !EDGE_DWELL_ACTIVE.swap(true, Ordering::AcqRel) {
                    SetTimer(hwnd, TIMER_EDGE_DWELL, EDGE_DWELL_GHOST_MS, None);
                }
            } else if !EDGE_DWELL_ACTIVE.swap(true, Ordering::AcqRel) {
                SetTimer(hwnd, TIMER_EDGE_DWELL, EDGE_DWELL_BORDER_MS, None);
            }
            0
        }
        WM_MOUSELEAVE_MSG => {
            EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
            KillTimer(hwnd, TIMER_EDGE_DWELL);
            0
        }
        WM_TIMER => {
            if wp == TIMER_EDGE_DWELL {
                EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
                KillTimer(hwnd, TIMER_EDGE_DWELL);
                if current_mode() == TakeoverMode::TvStandby {
                    let cfg = EDGE_CFG.try_lock().ok().and_then(|g| *g);
                    if let Some(c) = cfg {
                        if !should_suppress_edge_crossing(c.tv_ghost_rect) {
                            let mut pt: POINT = std::mem::zeroed();
                            let mut wr: RECT = std::mem::zeroed();
                            if GetCursorPos(&mut pt) != 0
                                && GetWindowRect(hwnd, &mut wr) != 0
                                && pt.x >= wr.left
                                && pt.x < wr.right
                                && pt.y >= wr.top
                                && pt.y < wr.bottom
                            {
                                fire_edge_enter(hwnd);
                            }
                        }
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount() -> u32;
}
