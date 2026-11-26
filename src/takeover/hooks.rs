use super::state::*;
use crate::keymap::{KeyAction, MOD_ALT, MOD_CTRL, MOD_SHIFT};
use crate::scrcpy_proto::{
    android_metastate_from_held, vk_to_android_keycode, ACTION_DOWN, ACTION_UP, HMOD_LALT,
    HMOD_LCTRL, HMOD_LSHIFT, HMOD_LWIN, HMOD_RALT, HMOD_RCTRL, HMOD_RSHIFT, HMOD_RWIN,
    MOUSE_BTN_LEFT, MOUSE_BTN_RIGHT,
};
use std::ptr;
use std::sync::atomic::Ordering;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const LLKHF_INJECTED_FLAG: u32 = 0x0000_0010;
const LLKHF_EXTENDED_FLAG: u32 = 0x0000_0001;
const LLMHF_INJECTED_FLAG: u32 = 0x0000_0001;
const MOUSE_MOVE_ABSOLUTE_FLAG: u16 = 0x0001;

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
pub(super) struct StickyKeysGuard {
    saved: Option<StickyKeysStruct>,
}

impl StickyKeysGuard {
    pub(super) fn suppress(&mut self) {
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

    pub(super) fn restore(&mut self) {
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

/// Releases any modifier keys currently down in Windows before entering TV capture,
/// so switching PC -> TV via a combo hotkey (e.g. Ctrl+Shift+S / Ctrl+Alt+S) never leaves
/// Windows with stuck Ctrl/Shift/Alt when the user releases the keys seconds later!
pub(super) fn release_pc_modifiers_and_swallow(record_swallow: bool) {
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
pub(super) fn swallow_currently_held_modifiers() {
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

pub(super) fn set_raw_input_device(host_hwnd: HWND, enable: bool) -> bool {
    unsafe {
        let rid = RAWINPUTDEVICE {
            usUsagePage: 0x01, // HID_USAGE_PAGE_GENERIC
            usUsage: 0x02,     // HID_USAGE_GENERIC_MOUSE
            dwFlags: if enable { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
            hwndTarget: if enable { host_hwnd } else { ptr::null_mut() },
        };
        let ok = RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
        ok != 0 || !enable
    }
}

pub(super) fn install_keyboard_hook() {
    if KEY_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let hmod = GetModuleHandleW(ptr::null());
        let hk = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hmod, 0);
        KEY_HOOK.store(hk as isize, Ordering::Release);
    }
}

pub(super) fn uninstall_keyboard_hook() {
    swallowed_reset_all();
    let hk = KEY_HOOK.swap(0, Ordering::AcqRel) as HHOOK;
    if !hk.is_null() {
        unsafe {
            UnhookWindowsHookEx(hk);
        }
    }
}

pub(super) fn install_mouse_hook() {
    if MOUSE_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let hmod = GetModuleHandleW(ptr::null());
        let hk = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hmod, 0);
        MOUSE_HOOK.store(hk as isize, Ordering::Release);
    }
}

pub(super) fn uninstall_mouse_hook() {
    let hk = MOUSE_HOOK.swap(0, Ordering::AcqRel) as HHOOK;
    if !hk.is_null() {
        unsafe {
            UnhookWindowsHookEx(hk);
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
            let now = get_tick_count();
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
                        let now = get_tick_count();
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
                        let now = get_tick_count();
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
