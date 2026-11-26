use crate::adb::DeviceController;
use crate::clipboard::{clipboard_sequence, read_pc_clipboard_text, write_pc_clipboard_text};
use crate::config::{to_wide, AppConfig};
use crate::display::{
    is_foreground_fullscreen_on_pc, pick_visible_pc_window, query_display_topology, rect_contains,
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
use std::process::{Child, ChildStdin, Command, Stdio};
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize,
    Ordering,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

// StickyKeys SPI constants (suppresses Windows 5x-Shift popup strictly while controlling TV)
const SPI_GETSTICKYKEYS_ID: u32 = 0x003A;
const SPI_SETSTICKYKEYS_ID: u32 = 0x003B;
const SKF_STICKYKEYSON_FLAG: u32 = 0x0000_0001;
const SKF_HOTKEYACTIVE_FLAG: u32 = 0x0000_0004;
const SKF_CONFIRMHOTKEY_FLAG: u32 = 0x0000_0008;

#[repr(C)]
#[derive(Clone, Copy)]
struct StickyKeysStruct {
    cb_size: u32,
    dw_flags: u32,
}

#[derive(Default)]
struct StickyKeysGuard {
    saved: Option<StickyKeysStruct>,
}

impl StickyKeysGuard {
    fn suppress(&mut self) {
        if self.saved.is_some() {
            return;
        }
        unsafe {
            let mut sk = StickyKeysStruct {
                cb_size: std::mem::size_of::<StickyKeysStruct>() as u32,
                dw_flags: 0,
            };
            if SystemParametersInfoW(
                SPI_GETSTICKYKEYS_ID,
                sk.cb_size,
                &mut sk as *mut _ as *mut _,
                0,
            ) != 0
            {
                let hotkey_bits = SKF_HOTKEYACTIVE_FLAG | SKF_CONFIRMHOTKEY_FLAG;
                if (sk.dw_flags & SKF_STICKYKEYSON_FLAG) == 0 && (sk.dw_flags & hotkey_bits) != 0 {
                    self.saved = Some(sk);
                    let mut muted = sk;
                    muted.dw_flags &= !hotkey_bits;
                    let _ = SystemParametersInfoW(
                        SPI_SETSTICKYKEYS_ID,
                        muted.cb_size,
                        &mut muted as *mut _ as *mut _,
                        0,
                    );
                }
            }
        }
    }

    fn restore(&mut self) {
        if let Some(mut orig) = self.saved.take() {
            unsafe {
                let _ = SystemParametersInfoW(
                    SPI_SETSTICKYKEYS_ID,
                    orig.cb_size,
                    &mut orig as *mut _ as *mut _,
                    0,
                );
            }
        }
    }
}

impl Drop for StickyKeysGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

pub const WM_CONTROL_READY: u32 = WM_APP + 10;
pub const WM_CONTROL_EXITED: u32 = WM_APP + 11;
pub const WM_DP_RETURNED: u32 = WM_APP + 12;
pub const WM_TRIGGER_RETURN_DP: u32 = WM_APP + 14;
pub const WM_TRIGGER_TOGGLE_FOCUS: u32 = WM_APP + 15;
pub const WM_EDGE_ENTER_TV: u32 = WM_APP + 16;
pub const WM_TV_CLIPBOARD: u32 = WM_APP + 17;
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
static EDGE_SWITCH_ATOMIC: AtomicBool = AtomicBool::new(true);
static EDGE_RES_PC: AtomicI32 = AtomicI32::new(120);
static EDGE_RES_TV: AtomicI32 = AtomicI32::new(180);
static LAST_WHEEL_MS: AtomicU32 = AtomicU32::new(0);
static KEY_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static HOST_HWND_GLOBAL: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_DEV: Mutex<Option<Arc<DeviceController>>> = Mutex::new(None);
static GLOBAL_CLIENT: Mutex<Option<Arc<ScrcpyControlClient>>> = Mutex::new(None);
static ACTIVE_KEYMAP: Mutex<Option<KeyMap>> = Mutex::new(None);

// Phase 2 TV-side 3px hover sensor state (lock-free hot path in handle_raw_input!)
// TV strip side: 0=None, 1=LEFT (outward -dx), 2=RIGHT (outward +dx), 3=TOP (-dy), 4=BOTTOM (+dy)
const TV_SIDE_NONE: u8 = 0;
const TV_SIDE_LEFT: u8 = 1;
const TV_SIDE_RIGHT: u8 = 2;
const TV_SIDE_TOP: u8 = 3;
const TV_SIDE_BOTTOM: u8 = 4;
const TV_ENTER_GRACE_MS: u32 = 300;
const PUSH_DECAY_MS: u32 = 260;

static TV_STRIP_SIDE: AtomicU8 = AtomicU8::new(TV_SIDE_NONE);
static TV_EDGE_ACTIVE: AtomicBool = AtomicBool::new(false);
static TV_EDGE_RATIO: AtomicU32 = AtomicU32::new(5000);
static TV_EDGE_SEQ: AtomicU32 = AtomicU32::new(0);
static TV_SCREEN_W: AtomicI32 = AtomicI32::new(1920);
static TV_SCREEN_H: AtomicI32 = AtomicI32::new(1080);
static TV_EDGE_PUSH_ACCUM: AtomicI32 = AtomicI32::new(0);
static TV_EDGE_PUSH_LAST_MS: AtomicU32 = AtomicU32::new(0);
static TV_ENTER_GRACE_UNTIL: AtomicU32 = AtomicU32::new(0);
static TV_LBUTTON_DOWN: AtomicBool = AtomicBool::new(false);

// PC-side elastic pressure barrier state (resets after 260ms pause)
static PC_EDGE_PUSH_ACCUM: AtomicI32 = AtomicI32::new(0);
static PC_EDGE_PUSH_LAST_MS: AtomicU32 = AtomicU32::new(0);

// PC -> TV velocity-primed single-frame burst alignment (exact linear Android VelocityControl)
static TV_ALIGNING: AtomicBool = AtomicBool::new(false);
static TV_ALIGN_GEN: AtomicU32 = AtomicU32::new(0);
static ALIGN_USER_DX: AtomicI32 = AtomicI32::new(0);
static ALIGN_USER_DY: AtomicI32 = AtomicI32::new(0);
static TV_LAST_EXITED_BY_EDGE: AtomicBool = AtomicBool::new(false);
// Primed pointer acceleration x100 (measured 300 = 3.000x on Redmi G Pro 27U Android 14; auto-calibrates on other TVs)
static TV_PRIMED_ACCEL_X100: AtomicI32 = AtomicI32::new(300);

fn tv_strip_for_pc_edge(pc_side: EdgeSide) -> (u8, &'static str) {
    // TV strip lies on the OPPOSITE border of the TV screen facing back toward the PC monitor
    match pc_side {
        EdgeSide::Left => (TV_SIDE_RIGHT, "RIGHT"),
        EdgeSide::Right => (TV_SIDE_LEFT, "LEFT"),
        EdgeSide::Top => (TV_SIDE_BOTTOM, "BOTTOM"),
        EdgeSide::Bottom => (TV_SIDE_TOP, "TOP"),
    }
}

struct TvEdgeSensor {
    child: Child,
    stdin: ChildStdin,
    stop: Arc<AtomicBool>,
    last_side: String,
}

impl TvEdgeSensor {
    fn spawn_attempt(
        adb_path: &PathBuf,
        addr: &str,
        shell_cmd: &str,
    ) -> Option<(Child, ChildStdin, BufReader<std::process::ChildStdout>)> {
        let mut child = Command::new(adb_path)
            .args(["-s", addr, "shell", shell_cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout_raw = child.stdout.take()?;
        let mut reader = BufReader::new(stdout_raw);
        let mut first = String::new();
        if reader.read_line(&mut first).unwrap_or(0) > 0 && first.trim().starts_with("READY") {
            let mut parts = first.split_whitespace();
            let _ = parts.next();
            if let (Some(Ok(w)), Some(Ok(h))) = (
                parts.next().map(|s| s.parse::<i32>()),
                parts.next().map(|s| s.parse::<i32>()),
            ) {
                if w > 0 && h > 0 {
                    TV_SCREEN_W.store(w, Ordering::Relaxed);
                    TV_SCREEN_H.store(h, Ordering::Relaxed);
                }
            }
            return Some((child, stdin, reader));
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    fn start(
        dev: &Arc<DeviceController>,
        adb_path: &PathBuf,
        addr: &str,
        initial_side: &str,
    ) -> Option<Self> {
        dev.ensure_jars_deployed();
        // 1. Try rootless com.android.shell overlay first
        let _ = dev.shell(
            "appops set com.android.shell SYSTEM_ALERT_WINDOW allow >/dev/null 2>&1",
            2000,
        );
        let rootless_cmd = format!(
            "CLASSPATH=/data/local/tmp/MiTVEdge.jar:/data/local/tmp/scrcpy-server.jar /system/bin/app_process /data/local/tmp MiTVEdge {}",
            initial_side
        );
        let started = Self::spawn_attempt(adb_path, addr, &rootless_cmd).or_else(|| {
            // 2. Fallback to rooted uid 1000 system overlay
            let rooted_cmd = format!(
                "su 1000 -c 'CLASSPATH=/data/local/tmp/MiTVEdge.jar:/data/local/tmp/scrcpy-server.jar /system/bin/app_process /data/local/tmp MiTVEdge {}'",
                initial_side
            );
            Self::spawn_attempt(adb_path, addr, &rooted_cmd)
        });

        let (child, stdin, mut reader) = started?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_r = Arc::clone(&stop);

        std::thread::spawn(move || {
            let mut line = String::new();
            while !stop_r.load(Ordering::Relaxed) {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim();
                        if t == "L" {
                            TV_EDGE_ACTIVE.store(false, Ordering::Release);
                            TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                        } else if let Some(rest) = t.strip_prefix("H ") {
                            if let Ok(r10k) = rest.trim().parse::<u32>() {
                                TV_EDGE_RATIO.store(r10k.min(10000), Ordering::Relaxed);
                                TV_EDGE_ACTIVE.store(true, Ordering::Release);
                                TV_EDGE_SEQ.fetch_add(1, Ordering::Release);
                            }
                        }
                    }
                }
            }
            TV_EDGE_ACTIVE.store(false, Ordering::Release);
            TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        });

        Some(Self {
            child,
            stdin,
            stop,
            last_side: initial_side.to_string(),
        })
    }

    fn set_side(&mut self, side_cmd: &str) {
        if self.last_side == side_cmd {
            return;
        }
        self.last_side.clear();
        self.last_side.push_str(side_cmd);
        let _ = self.stdin.write_all(format!("S {}\n", side_cmd).as_bytes());
        let _ = self.stdin.flush();
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.stdin.write_all(b"Q\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        let deadline = Instant::now() + Duration::from_millis(300);
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

#[derive(Clone, Copy)]
struct EdgeRuntimeCfg {
    side: EdgeSide,
    seam: i32,
    is_ghost: bool,
    tv_ghost_rect: Option<RECT>,
    adj_pc_rect: RECT,
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

/// Releases any modifier keys currently down in Windows before entering TV capture,
/// so switching PC -> TV via a combo hotkey (e.g. Ctrl+Shift+S / Ctrl+Alt+S) never leaves
/// Windows with stuck Ctrl/Shift/Alt when the user releases the keys seconds later!
fn release_pc_modifiers_and_swallow(record_swallow: bool) {
    const MOD_KEYS: [(u16, bool); 8] = [
        (VK_LCONTROL, false),
        (VK_RCONTROL, true),
        (VK_LSHIFT, false),
        (VK_RSHIFT, false),
        (VK_LMENU, false),
        (VK_RMENU, true),
        (VK_LWIN, true),
        (VK_RWIN, true),
    ];

    unsafe {
        let mut inputs: Vec<INPUT> = Vec::with_capacity(10);
        let mut need_menu_mask = false;

        for &(vk, extended) in &MOD_KEYS {
            if (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 {
                if matches!(vk, VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN) {
                    need_menu_mask = true;
                }
                if record_swallow {
                    swallowed_test_and_set(vk);
                }
                let mut flags = KEYEVENTF_KEYUP;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }

        if inputs.is_empty() {
            return;
        }

        // Mask Alt/Win release with unassigned VK 0xFF so foreground window menus / Start Menu never pop
        if need_menu_mask {
            let mut masked = Vec::with_capacity(inputs.len() + 2);
            masked.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0xFF,
                        wScan: 0,
                        dwFlags: 0,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            masked.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0xFF,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            masked.extend(inputs);
            inputs = masked;
        }

        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
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

fn swallowed_is_set(vk: u16) -> bool {
    if vk >= 256 {
        return false;
    }
    let idx = (vk as usize) >> 6;
    let mask = 1u64 << (vk & 63);
    (SWALLOWED_VKS[idx].load(Ordering::Relaxed) & mask) != 0
}

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
    edge_sensor: Arc<Mutex<Option<TvEdgeSensor>>>,
    edge_sensor_gen: Arc<AtomicU32>,
    watch_stop: Mutex<Option<Arc<AtomicBool>>>,
    lid_open: AtomicBool,
    topo: DisplayTopology,
    saved_display: SavedDisplayConfig,
    sticky_keys: StickyKeysGuard,
    saved_pc_cursor: Option<POINT>,
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
        let (tv_hint, fallback_side, edge_on, res_pc, res_tv, keymap) = cfg
            .lock()
            .map(|c| {
                (
                    c.tv_monitor_hint.clone(),
                    c.tv_edge_side,
                    c.edge_switch,
                    c.edge_resistance_pc,
                    c.edge_resistance_tv,
                    c.keymap.clone(),
                )
            })
            .unwrap_or_else(|_| {
                ("Mi Monitor".to_string(), EdgeSide::Left, true, 120, 180, KeyMap::default())
            });
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        EDGE_SWITCH_ATOMIC.store(edge_on, Ordering::Relaxed);
        EDGE_RES_PC.store(res_pc.clamp(0, 300), Ordering::Relaxed);
        EDGE_RES_TV.store(res_tv.clamp(0, 300), Ordering::Relaxed);

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
            edge_sensor: Arc::new(Mutex::new(None)),
            edge_sensor_gen: Arc::new(AtomicU32::new(0)),
            watch_stop: Mutex::new(None),
            lid_open: AtomicBool::new(true),
            topo,
            saved_display: SavedDisplayConfig::default(),
            sticky_keys: StickyKeysGuard::default(),
            saved_pc_cursor: None,
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
        let (edge_on, res_pc, res_tv, keymap) = self
            .cfg
            .lock()
            .map(|c| (c.edge_switch, c.edge_resistance_pc, c.edge_resistance_tv, c.keymap.clone()))
            .unwrap_or_else(|_| (true, 120, 180, KeyMap::default()));
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        EDGE_SWITCH_ATOMIC.store(edge_on, Ordering::Relaxed);
        EDGE_RES_PC.store(res_pc.clamp(0, 300), Ordering::Relaxed);
        EDGE_RES_TV.store(res_tv.clamp(0, 300), Ordering::Relaxed);
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
        self.capture_pc_cursor_if_visible();

        self.dev.start_persistent_shell();
        self.dev.set_keep_warm(true);

        if self.topo.has_visible_pc() {
            self.enter_mode(TakeoverMode::TvFocused);
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
        self.start_edge_sensor_async();
    }

    fn capture_pc_cursor_if_visible(&mut self) {
        unsafe {
            let mut pt: POINT = std::mem::zeroed();
            if GetCursorPos(&mut pt) != 0 {
                if self.topo.visible_pc_monitors.iter().any(|m| rect_contains(&m.rect, pt.x, pt.y)) {
                    self.saved_pc_cursor = Some(pt);
                }
            }
        }
    }

    /// Parks the Windows cursor out of sight while controlling the TV:
    /// - Extend mode with ghost TV monitor: inside the invisible 27U ghost rect (100% hidden on laptop!)
    /// - Single-screen / Duplicate mode: extreme bottom-right pixel of PC monitor (arrow off-screen!)
    fn park_pc_cursor_hidden(&self) {
        unsafe {
            if let Some(ref ghost) = self.topo.tv_ghost_monitor {
                let gx = ghost.rect.left + ((ghost.rect.right - ghost.rect.left) / 4).min(160);
                let gy = ghost.rect.top + ((ghost.rect.bottom - ghost.rect.top) / 4).min(160);
                SetCursorPos(gx, gy);
            } else if let Some(pc) = self.topo.visible_pc_monitors.first() {
                SetCursorPos(pc.rect.right - 1, pc.rect.bottom - 1);
            }
        }
    }

    fn point_on_pc_edge(&self, ratio_10k: usize) -> POINT {
        let ratio = (ratio_10k.min(10000) as f32) / 10000.0;
        let pc_rect = self
            .topo
            .pc_fence_rect
            .or_else(|| self.topo.visible_pc_monitors.first().map(|m| m.rect));
        let Some(r) = pc_rect else {
            return self.topo.return_cursor_pt;
        };
        match self.topo.edge_side.unwrap_or(EdgeSide::Left) {
            EdgeSide::Left => POINT {
                x: r.left + 14,
                y: (r.top + ((r.bottom - r.top) as f32 * ratio) as i32).clamp(r.top + 8, r.bottom - 8),
            },
            EdgeSide::Right => POINT {
                x: r.right - 15,
                y: (r.top + ((r.bottom - r.top) as f32 * ratio) as i32).clamp(r.top + 8, r.bottom - 8),
            },
            EdgeSide::Top => POINT {
                x: (r.left + ((r.right - r.left) as f32 * ratio) as i32).clamp(r.left + 8, r.right - 8),
                y: r.top + 14,
            },
            EdgeSide::Bottom => POINT {
                x: (r.left + ((r.right - r.left) as f32 * ratio) as i32).clamp(r.left + 8, r.right - 8),
                y: r.bottom - 15,
            },
        }
    }

    /// Velocity-primed single-frame PC -> TV edge y-alignment!
    /// Sends Packet 1 = `(pin_dx, 0)` (pins cursor onto the 3px strip & primes Android VelocityControl)
    /// followed immediately by `(0, sy)` packets in ONE single TCP write so all movement runs at
    /// exact linear `3.000x` acceleration (+/-1px accuracy across the full 1080p height on Frame 1!).
    fn spawn_tv_cursor_edge_align(&self, target_r10k: usize) {
        let client_opt = self.client.lock().ok().and_then(|g| g.clone());
        let Some(client) = client_opt else {
            return;
        };
        let strip_side = TV_STRIP_SIDE.load(Ordering::Acquire);
        if strip_side == TV_SIDE_NONE {
            return;
        }
        let target = target_r10k.min(10000) as i32;
        let gen = TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        ALIGN_USER_DX.store(0, Ordering::Relaxed);
        ALIGN_USER_DY.store(0, Ordering::Relaxed);
        TV_ALIGNING.store(true, Ordering::Release);

        let was_on_edge = TV_LAST_EXITED_BY_EDGE.swap(false, Ordering::AcqRel);

        std::thread::spawn(move || {
            let aborted = || {
                TV_ALIGN_GEN.load(Ordering::Acquire) != gen
                    || current_mode() != TakeoverMode::TvFocused
                    || !client.is_alive()
            };

            let wait_edge_seq = |seq_before: u32, max_ms: u32| -> bool {
                let iters = (max_ms / 4).max(1);
                for _ in 0..iters {
                    if aborted() {
                        return false;
                    }
                    if TV_EDGE_SEQ.load(Ordering::Acquire) != seq_before {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(4));
                }
                false
            };

            let (dock_dx, dock_dy, pin_dx, pin_dy, nudge_dx, nudge_dy, horizontal_strip) =
                match strip_side {
                    TV_SIDE_RIGHT => (800, 0, 40, 0, -16, 0, false),
                    TV_SIDE_LEFT => (-800, 0, -40, 0, 16, 0, false),
                    TV_SIDE_BOTTOM => (0, 800, 0, 40, 0, -16, true),
                    TV_SIDE_TOP => (0, -800, 0, -40, 0, 16, true),
                    _ => {
                        TV_ALIGNING.store(false, Ordering::Release);
                        return;
                    }
                };

            let tv_span = if horizontal_strip {
                TV_SCREEN_W.load(Ordering::Relaxed).max(480)
            } else {
                TV_SCREEN_H.load(Ordering::Relaxed).max(480)
            };

            let send_primed_axis = |counts: i32| {
                if horizontal_strip {
                    client.send_primed_mouse_burst(0, pin_dy, counts, 0);
                } else {
                    client.send_primed_mouse_burst(pin_dx, 0, 0, counts);
                }
            };

            let counts_for_err = |err_r10k: i32| -> i32 {
                let accel = TV_PRIMED_ACCEL_X100.load(Ordering::Relaxed).clamp(100, 500) as i64;
                let want_px = (err_r10k as i64 * tv_span as i64) / 10000;
                // Rounded integer division: round(want_px * 100 / accel)
                let num = want_px * 100;
                let rounded = if num >= 0 {
                    (num + accel / 2) / accel
                } else {
                    (num - accel / 2) / accel
                };
                rounded as i32
            };

            let mut need_dock = !was_on_edge;

            if was_on_edge {
                let start_r10k = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                let err0 = target - start_r10k;

                // Fast path: cursor is ALREADY sitting on the edge at practically the exact same height!
                if err0.abs() <= 120 {
                    TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                    TV_ENTER_GRACE_UNTIL
                        .store(unsafe { GetTickCount() }.wrapping_add(160), Ordering::Release);
                    TV_ALIGNING.store(false, Ordering::Release);
                    if !aborted() {
                        let udx = ALIGN_USER_DX.swap(0, Ordering::Relaxed);
                        let udy = ALIGN_USER_DY.swap(0, Ordering::Relaxed);
                        client.send_mouse_burst(nudge_dx + udx, nudge_dy + udy);
                    }
                    return;
                }

                // Frame 1 (VERY FIRST TCP WRITE): Primed burst teleports straight to target_y (+/-1px!)
                let pred_counts = counts_for_err(err0);
                let seq0 = TV_EDGE_SEQ.load(Ordering::Acquire);
                if pred_counts != 0 {
                    send_primed_axis(pred_counts);
                } else {
                    client.send_mouse_burst(pin_dx, pin_dy);
                }

                if wait_edge_seq(seq0, 160) {
                    let mut landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                    // If Packet 1 (prime) emitted an early H before Packet 2's H arrived, wait 1 VSync
                    if (target - landed).abs() > 220 && !aborted() {
                        std::thread::sleep(Duration::from_millis(18));
                        landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                    }
                    // Self-calibrate primed acceleration if non-saturated & moved cleanly in expected direction
                    if pred_counts.abs() >= 25 && landed > 250 && landed < 9750 {
                        let moved_px =
                            (((landed - start_r10k) as i64 * tv_span as i64) / 10000) as i32;
                        if moved_px.signum() == pred_counts.signum() && moved_px.abs() >= 45 {
                            let measured = ((moved_px.abs() as i64 * 100) / pred_counts.abs() as i64)
                                .clamp(100, 500) as i32;
                            let prev = TV_PRIMED_ACCEL_X100.load(Ordering::Relaxed).clamp(100, 500);
                            let blended = (prev * 4 + measured * 6) / 10;
                            TV_PRIMED_ACCEL_X100.store(blended.clamp(100, 500), Ordering::Relaxed);
                        }
                    }
                } else if !aborted() {
                    // Cursor wasn't actually on the strip (drifted inward) -> dock & align cleanly!
                    need_dock = true;
                }
            }

            if need_dock && !aborted() {
                let seq_d = TV_EDGE_SEQ.load(Ordering::Acquire);
                client.send_mouse_burst(dock_dx, dock_dy);
                let _ = wait_edge_seq(seq_d, 160);
            }

            // Closed-loop safety net (runs 0 iterations on normal primed hits; up to 2 if residual > 2.2%)
            for _ in 0..2 {
                if aborted() {
                    TV_ALIGNING.store(false, Ordering::Release);
                    return;
                }
                let cur = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                let err = target - cur;
                if err.abs() <= 220 {
                    break;
                }
                let corr = counts_for_err(err);
                if corr == 0 {
                    break;
                }
                let seq_c = TV_EDGE_SEQ.load(Ordering::Acquire);
                send_primed_axis(corr);
                if wait_edge_seq(seq_c, 150) {
                    let landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                    if (target - landed).abs() > 220 && !aborted() {
                        std::thread::sleep(Duration::from_millis(18));
                    }
                } else {
                    break;
                }
            }

            // Step 3: Release alignment lock & apply inward nudge + buffered user swipe momentum
            TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
            TV_ENTER_GRACE_UNTIL
                .store(unsafe { GetTickCount() }.wrapping_add(160), Ordering::Release);
            TV_ALIGNING.store(false, Ordering::Release);
            if !aborted() {
                let udx = ALIGN_USER_DX.swap(0, Ordering::Relaxed);
                let udy = ALIGN_USER_DY.swap(0, Ordering::Relaxed);
                client.send_mouse_burst(nudge_dx + udx, nudge_dy + udy);
            }
        });
    }

    fn desired_tv_edge_cmd(&self) -> (u8, &'static str) {
        let edge_on = EDGE_SWITCH_ATOMIC.load(Ordering::Relaxed);
        // Keep the 3px transparent TV strip warm/VISIBLE during both TvFocused AND TvStandby
        // (zero interference while user is on PC, and 0ms WindowManager redraw latency on re-entry!).
        // Only hide it in TvLocked (single-screen) or Idle or when edge_switch is disabled.
        let mode = self.mode();
        if (mode == TakeoverMode::TvFocused || mode == TakeoverMode::TvStandby) && edge_on {
            if let Some(pc_side) = self.topo.edge_side {
                return tv_strip_for_pc_edge(pc_side);
            }
        }
        (TV_SIDE_NONE, "HIDE")
    }

    fn sync_tv_edge_sensor_side(&self) {
        let (side_code, side_cmd) = self.desired_tv_edge_cmd();
        TV_STRIP_SIDE.store(side_code, Ordering::Release);
        TV_EDGE_ACTIVE.store(false, Ordering::Release);
        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        if let Ok(mut g) = self.edge_sensor.try_lock() {
            if let Some(ref mut sensor) = *g {
                sensor.set_side(side_cmd);
            }
        }
    }

    fn start_edge_sensor_async(&self) {
        let gen = self.edge_sensor_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let gen_ref = Arc::clone(&self.edge_sensor_gen);
        let slot = Arc::clone(&self.edge_sensor);
        let dev = Arc::clone(&self.dev);
        let adb_path = self.adb_path.clone();
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();
        let (_, init_cmd) = self.desired_tv_edge_cmd();
        let init_side = init_cmd.to_string();

        std::thread::spawn(move || {
            // Stop any prior instance first
            if let Ok(mut g) = slot.lock() {
                if let Some(old) = g.take() {
                    old.shutdown();
                }
            }
            if gen_ref.load(Ordering::Acquire) != gen || !current_mode().is_session_active() {
                return;
            }
            if let Some(mut sensor) = TvEdgeSensor::start(&dev, &adb_path, &addr, &init_side) {
                if gen_ref.load(Ordering::Acquire) != gen || !current_mode().is_session_active() {
                    sensor.shutdown();
                    return;
                }
                // Re-sync side in case mode changed while sensor was starting up
                let side_code = TV_STRIP_SIDE.load(Ordering::Acquire);
                let live_cmd = match side_code {
                    TV_SIDE_LEFT => "LEFT",
                    TV_SIDE_RIGHT => "RIGHT",
                    TV_SIDE_TOP => "TOP",
                    TV_SIDE_BOTTOM => "BOTTOM",
                    _ => "HIDE",
                };
                if live_cmd != init_side {
                    sensor.set_side(live_cmd);
                }
                if let Ok(mut g) = slot.lock() {
                    *g = Some(sensor);
                } else {
                    sensor.shutdown();
                }
            }
        });
    }

    fn stop_edge_sensor(&self) {
        self.edge_sensor_gen.fetch_add(1, Ordering::AcqRel);
        TV_STRIP_SIDE.store(TV_SIDE_NONE, Ordering::Release);
        TV_EDGE_ACTIVE.store(false, Ordering::Release);
        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        let old = self.edge_sensor.lock().ok().and_then(|mut g| g.take());
        if let Some(sensor) = old {
            std::thread::spawn(move || sensor.shutdown());
        }
    }

    pub fn toggle_focus_ui(&mut self) -> TakeoverMode {
        match self.mode() {
            TakeoverMode::TvFocused => {
                self.refresh_topology();
                if self.topo.has_visible_pc() {
                    TV_LAST_EXITED_BY_EDGE.store(false, Ordering::Release);
                    self.enter_mode(TakeoverMode::TvStandby);
                } else {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvStandby => {
                self.refresh_topology();
                self.capture_last_pc_window();
                self.capture_pc_cursor_if_visible();
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

    pub fn on_edge_enter_tv_ui(&mut self, entry_ratio_10k: usize) {
        if self.mode() != TakeoverMode::TvStandby {
            return;
        }
        self.refresh_topology();
        self.capture_last_pc_window();
        // Save matching PC edge point in case the user later returns via hotkey
        self.saved_pc_cursor = Some(self.point_on_pc_edge(entry_ratio_10k));
        if self.topo.has_visible_pc() {
            self.enter_mode(TakeoverMode::TvFocused);
            self.spawn_tv_cursor_edge_align(entry_ratio_10k);
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
    }

    /// Phase 2 entry point: called when TV-side 3px edge overlay + outward push exits back to PC.
    pub fn on_tv_edge_exit_ui(&mut self, y_ratio_10k: usize) {
        if self.mode() != TakeoverMode::TvFocused {
            return;
        }
        self.refresh_topology();
        if !self.topo.has_visible_pc() {
            return;
        }
        TV_LAST_EXITED_BY_EDGE.store(true, Ordering::Release);
        let pt = self.point_on_pc_edge(y_ratio_10k);
        self.saved_pc_cursor = Some(pt);
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
                } else {
                    self.sync_tv_edge_sensor_side();
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

        // When entering TV capture from PC (Idle/TvStandby), flush any held Windows modifiers
        // BEFORE switching MODE_ATOMIC so Windows never gets stuck with Ctrl/Shift/Alt held!
        if next.is_tv_capturing() && !prev.is_tv_capturing() {
            release_pc_modifiers_and_swallow(true);
        }

        MODE_ATOMIC.store(next as u8, Ordering::Release);

        match next {
            TakeoverMode::Idle => {
                TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel);
                TV_ALIGNING.store(false, Ordering::Release);
                HELD_MODS.store(0, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                self.sticky_keys.restore();
                self.release_fence();
                self.hide_edge_trigger();
                self.set_raw_input(false);
                self.uninstall_mouse_hook();
                self.uninstall_keyboard_hook();
                self.sync_tv_edge_sensor_side();
                // Restore remembered PC cursor if it was parked hidden
                if prev.is_tv_capturing() {
                    if let Some(pt) = self.saved_pc_cursor.take() {
                        unsafe {
                            SetCursorPos(pt.x, pt.y);
                        }
                    }
                }
            }
            TakeoverMode::TvLocked | TakeoverMode::TvFocused => {
                HELD_MODS.store(0, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                let now = unsafe { GetTickCount() };
                TV_ENTER_GRACE_UNTIL.store(now.wrapping_add(TV_ENTER_GRACE_MS), Ordering::Release);
                self.sticky_keys.suppress();
                self.release_fence();
                self.hide_edge_trigger();
                // Hide Windows cursor off the visible laptop screen while controlling TV
                self.park_pc_cursor_hidden();
                self.install_keyboard_hook();
                self.install_mouse_hook();
                self.set_raw_input(true);
                self.sync_tv_edge_sensor_side();
                self.sync_pc_clipboard_to_tv_if_changed();
            }
            TakeoverMode::TvStandby => {
                TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel);
                TV_ALIGNING.store(false, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                self.sticky_keys.restore();
                self.sync_tv_edge_sensor_side();
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
                // Restore remembered PC cursor position (for hotkey) or matched edge point (for edge exit)
                let pt = self
                    .saved_pc_cursor
                    .filter(|p| {
                        self.topo
                            .visible_pc_monitors
                            .iter()
                            .any(|m| rect_contains(&m.rect, p.x, p.y))
                    })
                    .unwrap_or(self.topo.return_cursor_pt);
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
        let adj_pc_rect = self
            .topo
            .pc_fence_rect
            .or_else(|| self.topo.visible_pc_monitors.first().map(|m| m.rect))
            .unwrap_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 });
        if let Ok(mut g) = EDGE_CFG.lock() {
            *g = Some(EdgeRuntimeCfg {
                side,
                seam: self.topo.seam_coord,
                is_ghost: self.topo.edge_trigger_is_ghost,
                tv_ghost_rect: self.topo.tv_ghost_monitor.as_ref().map(|m| m.rect),
                adj_pc_rect,
            });
        }
        EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
        PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
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
        self.stop_edge_sensor();
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
        // While closed-loop entry y-alignment is running (~50ms), buffer user swipe deltas
        // so leftward flick momentum is preserved cleanly without fighting alignment!
        if TV_ALIGNING.load(Ordering::Acquire) {
            ALIGN_USER_DX.fetch_add(dx, Ordering::Relaxed);
            ALIGN_USER_DY.fetch_add(dy, Ordering::Relaxed);
            return;
        }
        if let Ok(g) = GLOBAL_CLIENT.try_lock() {
            if let Some(ref client) = *g {
                client.send_mouse_move(dx, dy);
            }
        }

        // Phase 2: Bidirectional edge exit (TV -> PC) with customizable outward-push resistance
        if current_mode() == TakeoverMode::TvFocused
            && EDGE_SWITCH_ATOMIC.load(Ordering::Relaxed)
            && TV_EDGE_ACTIVE.load(Ordering::Acquire)
            && !TV_LBUTTON_DOWN.load(Ordering::Relaxed)
        {
            let now = GetTickCount();
            let grace = TV_ENTER_GRACE_UNTIL.load(Ordering::Relaxed);
            if now.wrapping_sub(grace) < 0x8000_0000 {
                let outward = match TV_STRIP_SIDE.load(Ordering::Relaxed) {
                    TV_SIDE_RIGHT => dx,
                    TV_SIDE_LEFT => -dx,
                    TV_SIDE_BOTTOM => dy,
                    TV_SIDE_TOP => -dy,
                    _ => 0,
                };
                if outward > 0 {
                    let prev_ms = TV_EDGE_PUSH_LAST_MS.swap(now, Ordering::Relaxed);
                    if now.wrapping_sub(prev_ms) > PUSH_DECAY_MS {
                        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                    }
                    let thresh = EDGE_RES_TV.load(Ordering::Relaxed).clamp(0, 300);
                    let accum = TV_EDGE_PUSH_ACCUM.fetch_add(outward, Ordering::Relaxed) + outward;
                    if accum >= thresh {
                        TV_EDGE_ACTIVE.store(false, Ordering::Release);
                        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                        let ratio = TV_EDGE_RATIO.load(Ordering::Relaxed) as usize;
                        let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
                        if host != 0 {
                            PostMessageW(host as HWND, WM_TV_EDGE_EXIT, ratio, 0);
                        }
                    }
                } else if outward < -4 {
                    TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Used ONLY in TvStandby (where Windows handles modifiers natively).
unsafe fn standby_mod_mask() -> u8 {
    let mut m = 0u8;
    if (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0 {
        m |= MOD_CTRL;
    }
    if (GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000) != 0 {
        m |= MOD_ALT;
    }
    if (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0 {
        m |= MOD_SHIFT;
    }
    m
}

/// Used ONLY in TvFocused / TvLocked (strictly isolated from Windows GetAsyncKeyState).
fn tv_held_mod_mask() -> u8 {
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
                    let mods = standby_mod_mask();
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
                    // Ignore auto-repeat of a modifier that was still held down from PC->TV switch
                    if hmod_bit != 0 && swallowed_is_set(vk) {
                        return 1;
                    }
                    if hmod_bit != 0 {
                        HELD_MODS.fetch_or(hmod_bit, Ordering::Relaxed);
                    }
                    let mods = tv_held_mod_mask();
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
                    TV_LBUTTON_DOWN.store(true, Ordering::Relaxed);
                    if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                        if let Some(ref c) = *g {
                            c.send_mouse_button(MOUSE_BTN_LEFT, true);
                        }
                    }
                    return 1;
                }
                WM_LBUTTONUP => {
                    TV_LBUTTON_DOWN.store(false, Ordering::Relaxed);
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
                    }
                    return 1;
                }
                WM_MOUSEHWHEEL => {
                    let delta = ((ms.mouseData >> 16) & 0xFFFF) as i16;
                    if delta != 0 {
                        let now = GetTickCount();
                        let prev = LAST_WHEEL_MS.load(Ordering::Relaxed);
                        if now.wrapping_sub(prev) >= 50 {
                            LAST_WHEEL_MS.store(now, Ordering::Relaxed);
                            let kc = if delta > 0 { 22 } else { 21 }; // DPAD_RIGHT / DPAD_LEFT
                            if let Ok(g) = GLOBAL_CLIENT.try_lock() {
                                if let Some(ref c) = *g {
                                    c.tap_keycode(kc);
                                }
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

fn compute_pc_entry_ratio_10k(cfg: EdgeRuntimeCfg, pt: POINT) -> usize {
    let r = cfg.adj_pc_rect;
    let ratio = match cfg.side {
        EdgeSide::Left | EdgeSide::Right => {
            ((pt.y - r.top) as f32 / (r.bottom - r.top).max(1) as f32).clamp(0.0, 1.0)
        }
        EdgeSide::Top | EdgeSide::Bottom => {
            ((pt.x - r.left) as f32 / (r.right - r.left).max(1) as f32).clamp(0.0, 1.0)
        }
    };
    (ratio * 10000.0).round() as usize
}

fn fire_edge_enter(hwnd: HWND, cfg: EdgeRuntimeCfg) {
    EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
    unsafe {
        KillTimer(hwnd, TIMER_EDGE_DWELL);
    }
    let mut pt: POINT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let ratio_10k = compute_pc_entry_ratio_10k(cfg, pt);
    let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
    if host != 0 {
        unsafe {
            PostMessageW(host as HWND, WM_EDGE_ENTER_TV, ratio_10k, 0);
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

            let res_pc = EDGE_RES_PC.load(Ordering::Relaxed).clamp(0, 300);
            if cfg.is_ghost {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let depth = match cfg.side {
                    EdgeSide::Left => cfg.seam - pt.x,
                    EdgeSide::Right => pt.x - cfg.seam,
                    EdgeSide::Top => cfg.seam - pt.y,
                    EdgeSide::Bottom => pt.y - cfg.seam,
                }
                .max(1);

                let now = GetTickCount();
                let prev_ms = PC_EDGE_PUSH_LAST_MS.swap(now, Ordering::Relaxed);
                if now.wrapping_sub(prev_ms) > PUSH_DECAY_MS {
                    PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                }
                let accum = PC_EDGE_PUSH_ACCUM.fetch_add(depth, Ordering::Relaxed) + depth;
                if accum >= res_pc {
                    PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                    fire_edge_enter(hwnd, cfg);
                    return 0;
                }
                // Elastic pressure barrier: bounce cursor right back onto the visible PC screen border
                // until cumulative push exceeds `res_pc` (cursor never vanishes into ghost screen!)
                match cfg.side {
                    EdgeSide::Left => SetCursorPos(cfg.seam, pt.y),
                    EdgeSide::Right => SetCursorPos(cfg.seam, pt.y),
                    EdgeSide::Top => SetCursorPos(pt.x, cfg.seam),
                    EdgeSide::Bottom => SetCursorPos(pt.x, cfg.seam),
                };
            } else if !EDGE_DWELL_ACTIVE.swap(true, Ordering::AcqRel) {
                let dwell_ms = (40 + res_pc).clamp(40, 340) as u32;
                SetTimer(hwnd, TIMER_EDGE_DWELL, dwell_ms, None);
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
                                fire_edge_enter(hwnd, c);
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
