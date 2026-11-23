use crate::adb::DeviceController;
use crate::config::AppConfig;
use crate::ime::{enforce_scrcpy_english_ime, restore_saved_ime, save_current_ime, SavedImeState};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CREATE_NO_WINDOW: u32 = 0x08000000;

pub const HK_RETURN_DP: i32 = 9001;
pub const HK_BACK: i32 = 9002;
pub const HK_HOME: i32 = 9003;
pub const HK_VOL_UP: i32 = 9005;
pub const HK_VOL_DOWN: i32 = 9006;
pub const HK_MUTE: i32 = 9007;
pub const HK_POWER: i32 = 9008;

pub const WM_SCRCPY_READY: u32 = WM_APP + 10;
pub const WM_SCRCPY_EXITED: u32 = WM_APP + 11;
pub const WM_DP_RETURNED: u32 = WM_APP + 12;
pub const WM_RESTORE_IME_RETRY: u32 = WM_APP + 13;

static HOOK_ACTIVE: AtomicBool = AtomicBool::new(false);
static WHEEL_AS_DPAD: AtomicBool = AtomicBool::new(true);
static F12_DOWN: AtomicBool = AtomicBool::new(false);
static LAST_WHEEL_MS: AtomicU32 = AtomicU32::new(0);
static KEY_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
static GLOBAL_DEV: Mutex<Option<Arc<DeviceController>>> = Mutex::new(None);

pub struct TakeoverSession {
    scrcpy_dir: PathBuf,
    scrcpy_path: PathBuf,
    cfg: Arc<Mutex<AppConfig>>,
    dev: Arc<DeviceController>,
    host_hwnd: usize,
    english_hkl: isize,
    child: Mutex<Option<Child>>,
    scrcpy_hwnd: AtomicIsize,
    watch_stop: Mutex<Option<Arc<AtomicBool>>>,
    hotkeys_registered: bool,
    saved_ime: Option<SavedImeState>,
}

impl TakeoverSession {
    pub fn new(
        scrcpy_dir: PathBuf,
        cfg: Arc<Mutex<AppConfig>>,
        dev: Arc<DeviceController>,
        host_hwnd: HWND,
        english_hkl: HKL,
    ) -> Self {
        let scrcpy_path = scrcpy_dir.join("scrcpy.exe");
        if let Ok(mut g) = GLOBAL_DEV.lock() {
            *g = Some(Arc::clone(&dev));
        }
        Self {
            scrcpy_dir,
            scrcpy_path,
            cfg,
            dev,
            host_hwnd: host_hwnd as usize,
            english_hkl: english_hkl as isize,
            child: Mutex::new(None),
            scrcpy_hwnd: AtomicIsize::new(0),
            watch_stop: Mutex::new(None),
            hotkeys_registered: false,
            saved_ime: None,
        }
    }

    pub fn is_running(&self) -> bool {
        if let Ok(mut g) = self.child.lock() {
            if let Some(ref mut c) = *g {
                return matches!(c.try_wait(), Ok(None));
            }
        }
        false
    }

    pub fn save_ime_before_switch(&mut self) {
        if self.saved_ime.is_none() {
            self.saved_ime = save_current_ime();
        }
    }

    pub fn sync_wheel_flag(&mut self) {
        let enabled = self.cfg.lock().map(|c| c.wheel_as_dpad).unwrap_or(true);
        WHEEL_AS_DPAD.store(enabled, Ordering::Relaxed);
        if self.is_running() {
            if enabled {
                self.install_mouse_hook();
            } else {
                self.uninstall_mouse_hook();
            }
        }
    }

    pub fn start_scrcpy_blocking(&self) -> Result<HWND, String> {
        if !self.scrcpy_path.exists() {
            return Err(format!("未找到 {}", self.scrcpy_path.display()));
        }
        if self.is_running() {
            let h = self.scrcpy_hwnd.load(Ordering::Acquire) as HWND;
            return Ok(h);
        }

        let addr = self.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default();
        let mut child = Command::new(&self.scrcpy_path)
            .current_dir(&self.scrcpy_dir)
            .args([
                &format!("--serial={}", addr),
                "--no-video",
                "--no-audio",
                "--keyboard=sdk",
                "--raw-key-events",
                "--mouse=uhid",
                "--mouse-bind=+hbh:b+++",
                "--window-borderless",
                "--window-width=120",
                "--window-height=120",
                "--window-title=MiTV-Switch-Capture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("无法启动 scrcpy.exe: {}", e))?;

        let pid = child.id();
        let mut found_hwnd: HWND = std::ptr::null_mut();
        for _ in 0..130 {
            if let Ok(Some(_)) = child.try_wait() {
                return Err("scrcpy 异常退出，请确认电视 ADB 已连接".to_string());
            }
            let h = find_main_window_by_pid(pid);
            if !h.is_null() {
                make_window_invisible(h);
                found_hwnd = h;
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }

        if found_hwnd.is_null() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("scrcpy 窗口初始化超时".to_string());
        }

        std::thread::sleep(Duration::from_millis(220));
        if let Ok(Some(_)) = child.try_wait() {
            return Err("scrcpy 连接后退出".to_string());
        }

        self.scrcpy_hwnd.store(found_hwnd as isize, Ordering::Release);
        if let Ok(mut g) = self.child.lock() {
            *g = Some(child);
        }

        // Notify UI thread to activate hooks & capture
        let host = self.host_hwnd;
        let found_raw = found_hwnd as usize;
        unsafe {
            PostMessageW(host as HWND, WM_SCRCPY_READY, found_raw as WPARAM, 0);
        }

        // Background exit watcher
        let host_exit = self.host_hwnd;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(350));
                if !HOOK_ACTIVE.load(Ordering::Relaxed) {
                    break;
                }
                let alive = unsafe { IsWindow(found_raw as HWND) != 0 };
                if !alive {
                    unsafe {
                        PostMessageW(host_exit as HWND, WM_SCRCPY_EXITED, 0, 0);
                    }
                    break;
                }
            }
        });

        Ok(found_hwnd)
    }

    pub fn on_scrcpy_ready_ui(&mut self, hwnd: HWND) {
        self.save_ime_before_switch();
        HOOK_ACTIVE.store(true, Ordering::Release);
        let wheel_on = self.cfg.lock().map(|c| c.wheel_as_dpad).unwrap_or(true);
        WHEEL_AS_DPAD.store(wheel_on, Ordering::Relaxed);

        self.dev.start_persistent_shell();
        self.register_hotkeys();
        self.install_keyboard_hook();
        if wheel_on {
            self.install_mouse_hook();
        }
        focus_and_capture_scrcpy(hwnd);
        enforce_scrcpy_english_ime(hwnd, self.english_hkl as HKL, true);
    }

    pub fn on_ime_guard_tick(&self) {
        if !self.is_running() {
            return;
        }
        let h = self.scrcpy_hwnd.load(Ordering::Acquire) as HWND;
        if !h.is_null() {
            enforce_scrcpy_english_ime(h, self.english_hkl as HKL, false);
        }
    }

    pub fn start_dp_return_watcher(&self) {
        self.stop_dp_return_watcher();
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut g) = self.watch_stop.lock() {
            *g = Some(Arc::clone(&stop));
        }

        let dev = Arc::clone(&self.dev);
        let host = self.host_hwnd;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            let mut saw_non_ext = false;
            let mut consecutive_ext = 0;

            while !stop.load(Ordering::Relaxed) && HOOK_ACTIVE.load(Ordering::Relaxed) {
                let (_, out, _) = dev.shell("dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'", 2500);
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
                let sleep_ms = if saw_non_ext { 650 } else { 450 };
                std::thread::sleep(Duration::from_millis(sleep_ms));
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
        HOOK_ACTIVE.store(false, Ordering::Release);
        self.stop_dp_return_watcher();
        self.uninstall_mouse_hook();
        self.uninstall_keyboard_hook();
        self.unregister_hotkeys();
        self.dev.stop_persistent_shell();

        let h = self.scrcpy_hwnd.swap(0, Ordering::AcqRel) as HWND;
        if !h.is_null() {
            unsafe {
                PostMessageW(h, WM_CLOSE, 0, 0);
            }
        }
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut c) = g.take() {
                let deadline = Instant::now() + Duration::from_millis(500);
                loop {
                    if !matches!(c.try_wait(), Ok(None)) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let _ = c.kill();
                        let _ = c.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }

        if let Some(saved) = self.saved_ime.take() {
            restore_saved_ime(&saved);
            let host = self.host_hwnd;
            let boxed = Box::into_raw(Box::new(saved)) as LPARAM;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                unsafe {
                    PostMessageW(host as HWND, WM_RESTORE_IME_RETRY, 0, boxed);
                }
            });
        }
    }

    pub fn handle_hotkey(&self, id: i32) -> bool {
        if !HOOK_ACTIVE.load(Ordering::Relaxed) {
            return false;
        }
        match id {
            HK_BACK => self.dev.send_tv_key_fast(4),
            HK_HOME => self.dev.send_tv_key_fast(3),
            HK_VOL_UP => self.dev.send_tv_key_fast(24),
            HK_VOL_DOWN => self.dev.send_tv_key_fast(25),
            HK_MUTE => self.dev.send_tv_key_fast(164),
            HK_POWER => self.dev.send_tv_key_fast(26),
            _ => return false,
        }
        true
    }

    fn register_hotkeys(&mut self) {
        if self.hotkeys_registered {
            return;
        }
        unsafe {
            let h = self.host_hwnd as HWND;
            RegisterHotKey(h, HK_RETURN_DP, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'D' as u32);
            RegisterHotKey(h, HK_BACK, MOD_NOREPEAT, VK_ESCAPE as u32);
            RegisterHotKey(h, HK_HOME, MOD_NOREPEAT, VK_HOME as u32);
            RegisterHotKey(h, HK_VOL_UP, 0, VK_PRIOR as u32);
            RegisterHotKey(h, HK_VOL_DOWN, 0, VK_NEXT as u32);
            RegisterHotKey(h, HK_MUTE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'M' as u32);
            RegisterHotKey(h, HK_POWER, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'P' as u32);
        }
        self.hotkeys_registered = true;
    }

    fn unregister_hotkeys(&mut self) {
        if !self.hotkeys_registered {
            return;
        }
        unsafe {
            let h = self.host_hwnd as HWND;
            for id in [HK_RETURN_DP, HK_BACK, HK_HOME, HK_VOL_UP, HK_VOL_DOWN, HK_MUTE, HK_POWER] {
                UnregisterHotKey(h, id);
            }
        }
        self.hotkeys_registered = false;
    }

    fn install_keyboard_hook(&mut self) {
        if KEY_HOOK.load(Ordering::Relaxed) != 0 {
            return;
        }
        F12_DOWN.store(false, Ordering::Relaxed);
        unsafe {
            let hmod = GetModuleHandleW(std::ptr::null());
            let hk = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hmod, 0);
            KEY_HOOK.store(hk as isize, Ordering::Release);
        }
    }

    fn uninstall_keyboard_hook(&mut self) {
        F12_DOWN.store(false, Ordering::Relaxed);
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
            let hmod = GetModuleHandleW(std::ptr::null());
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

unsafe extern "system" fn keyboard_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && HOOK_ACTIVE.load(Ordering::Relaxed) {
        let kb = &*(lp as *const KBDLLHOOKSTRUCT);
        if kb.vkCode == VK_F12 as u32 {
            let msg = wp as u32;
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                if !F12_DOWN.swap(true, Ordering::Relaxed) {
                    if let Ok(g) = GLOBAL_DEV.lock() {
                        if let Some(ref dev) = *g {
                            dev.send_tv_key_fast(82); // KEYCODE_MENU
                        }
                    }
                }
                return 1;
            }
            if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                F12_DOWN.store(false, Ordering::Relaxed);
                return 1;
            }
        }
    }
    CallNextHookEx(KEY_HOOK.load(Ordering::Relaxed) as HHOOK, code, wp, lp)
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && wp as u32 == WM_MOUSEWHEEL
        && HOOK_ACTIVE.load(Ordering::Relaxed)
        && WHEEL_AS_DPAD.load(Ordering::Relaxed)
    {
        let ms = &*(lp as *const MSLLHOOKSTRUCT);
        let delta = ((ms.mouseData >> 16) & 0xFFFF) as i16;
        if delta != 0 {
            let now = GetTickCount();
            let prev = LAST_WHEEL_MS.load(Ordering::Relaxed);
            if now.wrapping_sub(prev) >= 50 {
                LAST_WHEEL_MS.store(now, Ordering::Relaxed);
                if let Ok(g) = GLOBAL_DEV.lock() {
                    if let Some(ref dev) = *g {
                        dev.send_tv_key_fast(if delta > 0 { 19 } else { 20 });
                    }
                }
            }
            return 1;
        }
    }
    CallNextHookEx(MOUSE_HOOK.load(Ordering::Relaxed) as HHOOK, code, wp, lp)
}

#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount() -> u32;
}

fn find_main_window_by_pid(target_pid: u32) -> HWND {
    struct Search {
        pid: u32,
        found: HWND,
    }
    unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp as *mut Search);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == s.pid && IsWindowVisible(hwnd) != 0 {
            s.found = hwnd;
            return 0;
        }
        1
    }
    let mut search = Search { pid: target_pid, found: std::ptr::null_mut() };
    unsafe {
        EnumWindows(Some(enum_cb), &mut search as *mut _ as LPARAM);
    }
    search.found
}

fn make_window_invisible(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_ex = (ex | WS_EX_LAYERED as isize | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);
        SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
    }
}

fn focus_and_capture_scrcpy(hwnd: HWND) {
    unsafe {
        make_window_invisible(hwnd);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);

        let mut clip: RECT = std::mem::zeroed();
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let already_clipped = GetClipCursor(&mut clip) != 0
            && ((clip.right - clip.left) < sw - 10 || (clip.bottom - clip.top) < sh - 10);

        if !already_clipped {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) != 0 {
                SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                std::thread::sleep(Duration::from_millis(40));
                mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
                std::thread::sleep(Duration::from_millis(20));
                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
            }
        }
    }
}
