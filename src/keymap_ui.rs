use crate::config::{to_wide, AppConfig};
use crate::gdi::{apply_dark_control_theme, apply_dwm_dark_titlebar, ThemePalette};
use crate::keymap::{
    is_modifier_vk, validate_chord, KeyAction, KeyChord, KeyMap, ACTION_COUNT, ALL_ACTIONS,
    MOD_ALT, MOD_CTRL, MOD_SHIFT,
};
use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, FillRect, GetDC, GetDeviceCaps, ReleaseDC,
    SetBkColor, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL, HBRUSH, HFONT, LOGPIXELSX, OUT_DEFAULT_PRECIS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const KEYMAP_CLASS: &str = "MiKVMKeymapWnd";
const ID_BTN_PRESET_75: i32 = 301;
const ID_BTN_PRESET_60: i32 = 302;
const ID_BTN_RESET: i32 = 303;
const ID_LBL_MSG: i32 = 304;
const ID_BTN_SAVE: i32 = 305;
const ID_BTN_CANCEL: i32 = 306;
const ID_BTN_KEY_BASE: i32 = 320;
const ID_BTN_CLEAR_BASE: i32 = 350;

const WM_KEY_RECORDED: u32 = WM_APP + 60;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const LLKHF_INJECTED_FLAG: u32 = 0x0000_0010;

static REC_HOOK: AtomicIsize = AtomicIsize::new(0);
static REC_HWND: AtomicUsize = AtomicUsize::new(0);

struct KeymapUiState {
    hwnd: HWND,
    font: HFONT,
    palette: ThemePalette,
    bg_brush: HBRUSH,
    cfg: Arc<Mutex<AppConfig>>,
    draft: KeyMap,
    recording: Option<usize>,
    on_saved: Option<Box<dyn Fn()>>,
}

thread_local! {
    static KEYMAP_STATE: RefCell<Option<KeymapUiState>> = const { RefCell::new(None) };
}

fn install_recorder_hook(hwnd: HWND) {
    uninstall_recorder_hook();
    REC_HWND.store(hwnd as usize, Ordering::Release);
    unsafe {
        let hmod = GetModuleHandleW(ptr::null());
        let hk = SetWindowsHookExW(WH_KEYBOARD_LL, Some(record_hook_proc), hmod, 0);
        REC_HOOK.store(hk as isize, Ordering::Release);
    }
}

fn uninstall_recorder_hook() {
    REC_HWND.store(0, Ordering::Release);
    let hk = REC_HOOK.swap(0, Ordering::AcqRel) as HHOOK;
    if !hk.is_null() {
        unsafe {
            UnhookWindowsHookEx(hk);
        }
    }
}

unsafe fn current_mod_mask() -> u8 {
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

unsafe extern "system" fn record_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let target = REC_HWND.load(Ordering::Acquire);
    if code >= 0 && target != 0 && GetForegroundWindow() == target as HWND {
        let kb = &*(lp as *const KBDLLHOOKSTRUCT);
        if (kb.flags & LLKHF_INJECTED_FLAG) == 0 {
            let msg = wp as u32;
            let vk = kb.vkCode as u16;
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                if !is_modifier_vk(vk) {
                    let mods = current_mod_mask();
                    PostMessageW(target as HWND, WM_KEY_RECORDED, vk as WPARAM, mods as LPARAM);
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(REC_HOOK.load(Ordering::Relaxed) as HHOOK, code, wp, lp)
}

pub fn show_keymap_window<F>(cfg: Arc<Mutex<AppConfig>>, on_saved: F)
where
    F: Fn() + 'static,
{
    let existing = KEYMAP_STATE.with(|c| c.borrow().as_ref().map(|s| s.hwnd));
    if let Some(h) = existing {
        unsafe {
            if IsWindow(h) != 0 {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
                return;
            }
        }
    }

    unsafe {
        let pal = ThemePalette::current();
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(KEYMAP_CLASS);
        let bg_brush = CreateSolidBrush(pal.win32_bg_bgr);
        static CLASS_REG: std::sync::Once = std::sync::Once::new();
        CLASS_REG.call_once(|| {
            let class_brush = CreateSolidBrush(pal.win32_bg_bgr);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(keymap_wnd_proc),
                hInstance: hinst,
                hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                hbrBackground: class_brush,
                lpszClassName: cls_w.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        });

        let hdc = GetDC(ptr::null_mut());
        let dpi = if !hdc.is_null() {
            let d = GetDeviceCaps(hdc, LOGPIXELSX as i32);
            ReleaseDC(ptr::null_mut(), hdc);
            d
        } else {
            96
        };
        let s = (dpi as f32 / 96.0).max(1.0);
        let sc = |v: f32| (v * s).round() as i32;

        let cw = sc(476.0);
        let ch = sc(545.0);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut wr = RECT { left: 0, top: 0, right: cw, bottom: ch };
        AdjustWindowRectEx(&mut wr, style, 0, 0);
        let ww = wr.right - wr.left;
        let wh = wr.bottom - wr.top;
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            cls_w.as_ptr(),
            to_wide("自定义遥控快捷键 — MiKVM").as_ptr(),
            style,
            (sw - ww) / 2,
            (sh - wh) / 2,
            ww,
            wh,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        apply_dwm_dark_titlebar(hwnd, pal.dark);

        let font = CreateFontW(
            -sc(14.0),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            to_wide("Microsoft YaHei UI").as_ptr(),
        );

        let draft = cfg
            .lock()
            .map(|c| c.keymap.clone())
            .unwrap_or_default();

        let make_ctrl = |cls: &str, text: &str, ex: u32, st: u32, x: i32, y: i32, w: i32, h: i32, id: i32| -> HWND {
            let hw = CreateWindowExW(
                ex,
                to_wide(cls).as_ptr(),
                to_wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | st,
                x,
                y,
                w,
                h,
                hwnd,
                id as isize as HMENU,
                hinst,
                ptr::null(),
            );
            SendMessageW(hw, WM_SETFONT, font as WPARAM, 1);
            if cls == "BUTTON" {
                apply_dark_control_theme(hw, pal.dark);
            }
            hw
        };

        let pad = sc(18.0);
        let inner = cw - pad * 2;

        // Top presets row
        make_ctrl("BUTTON", "预设 1", 0, WS_TABSTOP, pad, sc(14.0), sc(92.0), sc(28.0), ID_BTN_PRESET_75);
        make_ctrl("BUTTON", "预设 2", 0, WS_TABSTOP, pad + sc(100.0), sc(14.0), sc(92.0), sc(28.0), ID_BTN_PRESET_60);
        make_ctrl("BUTTON", "恢复默认", 0, WS_TABSTOP, cw - pad - sc(86.0), sc(14.0), sc(86.0), sc(28.0), ID_BTN_RESET);

        make_ctrl(
            "STATIC",
            "点击按键框后直接按键盘即可录制；再次点击同一按钮可取消录制",
            0,
            0,
            pad,
            sc(50.0),
            inner,
            sc(20.0),
            0,
        );

        let label_w = sc(172.0);
        let clear_w = sc(56.0);
        let key_w = inner - label_w - clear_w - sc(16.0);
        let key_x = pad + label_w + sc(8.0);
        let clear_x = key_x + key_w + sc(8.0);

        for (i, act) in ALL_ACTIONS.iter().enumerate() {
            let y = sc(78.0 + (i as f32) * 33.0);
            make_ctrl("STATIC", act.label(), 0, 0, pad, y + sc(4.0), label_w, sc(22.0), 0);
            make_ctrl(
                "BUTTON",
                &draft.format_action(*act),
                0,
                WS_TABSTOP,
                key_x,
                y,
                key_w,
                sc(27.0),
                ID_BTN_KEY_BASE + i as i32,
            );
            let clr = make_ctrl(
                "BUTTON",
                "清除",
                0,
                WS_TABSTOP,
                clear_x,
                y,
                clear_w,
                sc(27.0),
                ID_BTN_CLEAR_BASE + i as i32,
            );
            if act.is_required() {
                EnableWindow(clr, 0);
            }
        }

        make_ctrl(
            "STATIC",
            "提示：仅在接管电视期间生效，切回电脑 DP 自动注销零误触",
            0,
            0,
            pad,
            sc(449.0),
            inner,
            sc(34.0),
            ID_LBL_MSG,
        );

        make_ctrl("BUTTON", "保存", 0, BS_DEFPUSHBUTTON | WS_TABSTOP, cw - pad - sc(168.0), ch - sc(46.0), sc(78.0), sc(32.0), ID_BTN_SAVE);
        make_ctrl("BUTTON", "取消", 0, WS_TABSTOP, cw - pad - sc(82.0), ch - sc(46.0), sc(82.0), sc(32.0), ID_BTN_CANCEL);

        KEYMAP_STATE.with(|c| {
            *c.borrow_mut() = Some(KeymapUiState {
                hwnd,
                font,
                palette: pal,
                bg_brush,
                cfg,
                draft,
                recording: None,
                on_saved: Some(Box::new(on_saved)),
            });
        });

        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

unsafe fn refresh_key_buttons(hwnd: HWND, draft: &KeyMap, recording: Option<usize>) {
    for (i, act) in ALL_ACTIONS.iter().enumerate() {
        let btn = GetDlgItem(hwnd, ID_BTN_KEY_BASE + i as i32);
        if !btn.is_null() {
            let txt = if recording == Some(i) {
                "[ 请按下快捷键... ]".to_string()
            } else {
                draft.format_action(*act)
            };
            SetWindowTextW(btn, to_wide(&txt).as_ptr());
        }
    }
}

unsafe fn set_msg(hwnd: HWND, text: &str) {
    let lbl = GetDlgItem(hwnd, ID_LBL_MSG);
    if !lbl.is_null() {
        SetWindowTextW(lbl, to_wide(text).as_ptr());
    }
}

unsafe extern "system" fn keymap_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let hdc = wp as windows_sys::Win32::Graphics::Gdi::HDC;
            let br = KEYMAP_STATE
                .with(|c| c.borrow().as_ref().map(|s| s.bg_brush).unwrap_or(ptr::null_mut()));
            if !br.is_null() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, br);
                return 1;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = wp as windows_sys::Win32::Graphics::Gdi::HDC;
            let info = KEYMAP_STATE.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|s| (s.palette.win32_bg_bgr, s.palette.win32_text_bgr, s.bg_brush))
            });
            if let Some((bg, fg, br)) = info {
                SetBkColor(hdc, bg);
                SetTextColor(hdc, fg);
                return br as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_ACTIVATE => {
            if (wp & 0xFFFF) as u32 == WA_INACTIVE {
                let snapshot = KEYMAP_STATE.with(|c| {
                    let mut b = c.borrow_mut();
                    if let Some(st) = b.as_mut() {
                        if st.recording.is_some() {
                            st.recording = None;
                            return Some(st.draft.clone());
                        }
                    }
                    None
                });
                if let Some(draft) = snapshot {
                    uninstall_recorder_hook();
                    refresh_key_buttons(hwnd, &draft, None);
                }
            }
            0
        }
        WM_KEY_RECORDED => {
            let vk = wp as u16;
            let mods = (lp & 0xFF) as u8;
            let chord = KeyChord::new(mods, vk);
            let result = KEYMAP_STATE.with(|c| {
                let mut b = c.borrow_mut();
                let st = b.as_mut()?;
                let idx = st.recording?;
                let act = ALL_ACTIONS[idx];
                match validate_chord(chord) {
                    Err(err_msg) => {
                        st.recording = None;
                        Some((st.draft.clone(), format!("无效按键（{}）：{}", chord.format(), err_msg)))
                    }
                    Ok(()) => {
                        let msg = if let Some(conflict) = st.draft.find_conflict(act, chord) {
                            if conflict.is_required() {
                                st.recording = None;
                                return Some((
                                    st.draft.clone(),
                                    format!("{} 已被必选项「{}」占用，请换一个键", chord.format(), conflict.label()),
                                ));
                            }
                            st.draft.set(conflict, None);
                            st.draft.set(act, Some(chord));
                            st.recording = None;
                            format!(
                                "已绑定：{} → {}（原「{}」的绑定已自动清除）",
                                act.label(),
                                chord.format(),
                                conflict.label()
                            )
                        } else {
                            st.draft.set(act, Some(chord));
                            st.recording = None;
                            format!("已绑定：{} → {}", act.label(), chord.format())
                        };
                        Some((st.draft.clone(), msg))
                    }
                }
            });
            uninstall_recorder_hook();
            if let Some((draft, status)) = result {
                refresh_key_buttons(hwnd, &draft, None);
                set_msg(hwnd, &status);
            }
            0
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as i32;
            if (ID_BTN_KEY_BASE..ID_BTN_KEY_BASE + ACTION_COUNT as i32).contains(&id) {
                let idx = (id - ID_BTN_KEY_BASE) as usize;
                let (draft, new_rec, status) = KEYMAP_STATE.with(|c| {
                    let mut b = c.borrow_mut();
                    if let Some(st) = b.as_mut() {
                        if st.recording == Some(idx) {
                            st.recording = None;
                            return (Some(st.draft.clone()), None, "已取消录制".to_string());
                        } else {
                            st.recording = Some(idx);
                            let act = ALL_ACTIONS[idx];
                            return (
                                Some(st.draft.clone()),
                                Some(idx),
                                format!("正在为「{}」录制快捷键，请直接按下目标按键或组合键...", act.label()),
                            );
                        }
                    }
                    (None, None, String::new())
                });
                if new_rec.is_some() {
                    install_recorder_hook(hwnd);
                } else {
                    uninstall_recorder_hook();
                }
                if let Some(d) = draft {
                    refresh_key_buttons(hwnd, &d, new_rec);
                    set_msg(hwnd, &status);
                }
                return 0;
            }
            if (ID_BTN_CLEAR_BASE..ID_BTN_CLEAR_BASE + ACTION_COUNT as i32).contains(&id) {
                let idx = (id - ID_BTN_CLEAR_BASE) as usize;
                let act = ALL_ACTIONS[idx];
                if !act.is_required() {
                    uninstall_recorder_hook();
                    let draft = KEYMAP_STATE.with(|c| {
                        let mut b = c.borrow_mut();
                        if let Some(st) = b.as_mut() {
                            st.recording = None;
                            st.draft.set(act, None);
                            return Some(st.draft.clone());
                        }
                        None
                    });
                    if let Some(d) = draft {
                        refresh_key_buttons(hwnd, &d, None);
                        set_msg(hwnd, &format!("已清除「{}」的快捷键", act.label()));
                    }
                }
                return 0;
            }
            match id {
                ID_BTN_PRESET_75 | ID_BTN_RESET => {
                    uninstall_recorder_hook();
                    let draft = KEYMAP_STATE.with(|c| {
                        let mut b = c.borrow_mut();
                        if let Some(st) = b.as_mut() {
                            st.recording = None;
                            st.draft = KeyMap::preset_1();
                            return Some(st.draft.clone());
                        }
                        None
                    });
                    if let Some(d) = draft {
                        refresh_key_buttons(hwnd, &d, None);
                        set_msg(hwnd, "已切换为「预设 1」（点「保存」生效）");
                    }
                    0
                }
                ID_BTN_PRESET_60 => {
                    uninstall_recorder_hook();
                    let draft = KEYMAP_STATE.with(|c| {
                        let mut b = c.borrow_mut();
                        if let Some(st) = b.as_mut() {
                            st.recording = None;
                            st.draft = KeyMap::preset_2();
                            return Some(st.draft.clone());
                        }
                        None
                    });
                    if let Some(d) = draft {
                        refresh_key_buttons(hwnd, &d, None);
                        set_msg(hwnd, "已切换为「预设 2」（Alt 组合键，点「保存」生效）");
                    }
                    0
                }
                ID_BTN_SAVE => {
                    uninstall_recorder_hook();
                    let cb = KEYMAP_STATE.with(|c| {
                        let mut b = c.borrow_mut();
                        if let Some(st) = b.as_mut() {
                            st.recording = None;
                            if st.draft.get(KeyAction::ReturnDp).is_none() {
                                st.draft.set(
                                    KeyAction::ReturnDp,
                                    KeyMap::preset_1().get(KeyAction::ReturnDp),
                                );
                            }
                            if let Ok(mut cfg) = st.cfg.lock() {
                                cfg.keymap = st.draft.clone();
                                cfg.save();
                            }
                            return st.on_saved.take();
                        }
                        None
                    });
                    DestroyWindow(hwnd);
                    if let Some(f) = cb {
                        f();
                    }
                    0
                }
                ID_BTN_CANCEL => {
                    uninstall_recorder_hook();
                    DestroyWindow(hwnd);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_CLOSE => {
            uninstall_recorder_hook();
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            uninstall_recorder_hook();
            KEYMAP_STATE.with(|c| {
                if let Some(st) = c.borrow_mut().take() {
                    if !st.font.is_null() {
                        DeleteObject(st.font as _);
                    }
                    if !st.bg_brush.is_null() {
                        DeleteObject(st.bg_brush as _);
                    }
                }
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
