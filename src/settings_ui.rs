use crate::adb::DeviceController;
use crate::config::{from_wide, is_autostart_enabled, set_autostart, to_wide, AppConfig};
use std::cell::RefCell;
use std::ptr;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, GetDC, GetDeviceCaps, ReleaseDC, SetBkColor,
    SetTextColor, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL,
    HBRUSH, HFONT, LOGPIXELSX, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const SETTINGS_CLASS: &str = "MiTVSwitchSettingsWnd";
const ID_EDIT_ADDR: i32 = 201;
const ID_BTN_TEST: i32 = 202;
const ID_LBL_STATUS: i32 = 203;
const ID_CHK_TAKEOVER: i32 = 204;
const ID_CHK_WHEEL: i32 = 205;
const ID_CHK_AUTOSTART: i32 = 206;
const ID_BTN_SAVE: i32 = 207;
const ID_BTN_CANCEL: i32 = 208;
const WM_TEST_DONE: u32 = WM_APP + 50;

const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const ES_AUTOHSCROLL: u32 = 0x0080;

struct SettingsState {
    hwnd: HWND,
    font: HFONT,
    bg_brush: HBRUSH,
    cfg: Arc<Mutex<AppConfig>>,
    dev: Arc<DeviceController>,
    on_saved: Option<Box<dyn Fn()>>,
}

thread_local! {
    static SETTINGS_STATE: RefCell<Option<SettingsState>> = const { RefCell::new(None) };
}

pub fn show_settings_window<F>(cfg: Arc<Mutex<AppConfig>>, dev: Arc<DeviceController>, on_saved: F)
where
    F: Fn() + 'static,
{
    let existing = SETTINGS_STATE.with(|c| c.borrow().as_ref().map(|s| s.hwnd));
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
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(SETTINGS_CLASS);
        let bg_brush = CreateSolidBrush(0x00F9F6F5); // BGR of #F5F6F9
        static CLASS_REG: std::sync::Once = std::sync::Once::new();
        CLASS_REG.call_once(|| {
            let class_brush = CreateSolidBrush(0x00F9F6F5);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(settings_wnd_proc),
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

        let cw = sc(430.0);
        let ch = sc(372.0);
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
            to_wide("ADB 与键鼠设置 — G Pro 27U").as_ptr(),
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

        let font = CreateFontW(
            -sc(14.5),
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

        let pad = sc(20.0);
        let inner = cw - pad * 2;
        let (addr, auto_to, wheel_dp) = cfg
            .lock()
            .map(|c| (c.tv_address.clone(), c.auto_takeover_on_tv, c.wheel_as_dpad))
            .unwrap_or_else(|_| ("192.168.1.200:5555".to_string(), true, true));

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
            hw
        };

        make_ctrl("STATIC", "电视无线 ADB 地址 (IP:端口)：", 0, 0, pad, sc(16.0), inner, sc(20.0), 0);
        make_ctrl("EDIT", &addr, WS_EX_CLIENTEDGE, ES_AUTOHSCROLL | WS_TABSTOP, pad, sc(42.0), inner - sc(96.0), sc(28.0), ID_EDIT_ADDR);
        make_ctrl("BUTTON", "测试连接", 0, WS_TABSTOP, cw - pad - sc(88.0), sc(41.0), sc(88.0), sc(30.0), ID_BTN_TEST);
        make_ctrl("STATIC", "键盘模式已固定为 SDK（兼容 TV 应用与小企鹅中文）", 0, 0, pad, sc(76.0), inner, sc(20.0), ID_LBL_STATUS);

        let chk_to = make_ctrl("BUTTON", "切换到「电视系统」时自动接管电脑键鼠", 0, BS_AUTOCHECKBOX | WS_TABSTOP, pad, sc(108.0), inner, sc(24.0), ID_CHK_TAKEOVER);
        let chk_wh = make_ctrl("BUTTON", "接管期间鼠标滚轮映射为上下方向键", 0, BS_AUTOCHECKBOX | WS_TABSTOP, pad, sc(138.0), inner, sc(24.0), ID_CHK_WHEEL);
        let chk_as = make_ctrl("BUTTON", "Windows 开机自动启动", 0, BS_AUTOCHECKBOX | WS_TABSTOP, pad, sc(168.0), inner, sc(24.0), ID_CHK_AUTOSTART);

        SendMessageW(chk_to, BM_SETCHECK, if auto_to { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_wh, BM_SETCHECK, if wheel_dp { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_as, BM_SETCHECK, if is_autostart_enabled() { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);

        let guide = "接管电视期间遥控快捷键（回电脑自动注销，零误触）：\r\n\
                     • 方向 / 确认：方向键 / Enter（滚轮 = 上下方向）\r\n\
                     • 返回 / 主页 / 菜单：Esc / Home / F12（侧键 = 返回/主页）\r\n\
                     • 音量 + / - / 静音：PgUp / PgDn / Ctrl+Alt+M\r\n\
                     • 待机息屏 / 切回电脑 DP：Ctrl+Alt+P / Ctrl+Alt+D";
        make_ctrl("STATIC", guide, 0, WS_BORDER, pad, sc(204.0), inner, sc(104.0), 0);

        make_ctrl("BUTTON", "保存", 0, BS_DEFPUSHBUTTON | WS_TABSTOP, cw - pad - sc(168.0), ch - sc(46.0), sc(78.0), sc(32.0), ID_BTN_SAVE);
        make_ctrl("BUTTON", "取消", 0, WS_TABSTOP, cw - pad - sc(82.0), ch - sc(46.0), sc(82.0), sc(32.0), ID_BTN_CANCEL);

        SETTINGS_STATE.with(|c| {
            *c.borrow_mut() = Some(SettingsState {
                hwnd,
                font,
                bg_brush,
                cfg,
                dev,
                on_saved: Some(Box::new(on_saved)),
            });
        });
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

unsafe fn get_edit_text(hwnd: HWND, id: i32) -> String {
    let ctrl = GetDlgItem(hwnd, id);
    if ctrl.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let len = GetWindowTextW(ctrl, buf.as_mut_ptr(), buf.len() as i32);
    if len <= 0 {
        return String::new();
    }
    from_wide(&buf[..len as usize])
}

unsafe extern "system" fn settings_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = wp as windows_sys::Win32::Graphics::Gdi::HDC;
            SetBkColor(hdc, 0x00F9F6F5);
            SetTextColor(hdc, 0x00241E1A);
            let br = SETTINGS_STATE.with(|c| c.borrow().as_ref().map(|s| s.bg_brush).unwrap_or(ptr::null_mut()));
            return br as LRESULT;
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as i32;
            match id {
                ID_BTN_TEST => {
                    let raw = get_edit_text(hwnd, ID_EDIT_ADDR);
                    let addr = AppConfig::normalize_address(&raw);
                    let lbl = GetDlgItem(hwnd, ID_LBL_STATUS);
                    SetWindowTextW(lbl, to_wide(&format!("正在测试连接 {}...", addr)).as_ptr());
                    let dev = SETTINGS_STATE.with(|c| c.borrow().as_ref().map(|s| Arc::clone(&s.dev)));
                    if let Some(dev) = dev {
                        let hwnd_raw = hwnd as usize;
                        std::thread::spawn(move || {
                            let ok = dev.test_connect(&addr);
                            PostMessageW(hwnd_raw as HWND, WM_TEST_DONE, if ok { 1 } else { 0 }, 0);
                        });
                    }
                    0
                }
                ID_BTN_SAVE => {
                    let raw = get_edit_text(hwnd, ID_EDIT_ADDR);
                    let addr = AppConfig::normalize_address(&raw);
                    let auto_to = SendMessageW(GetDlgItem(hwnd, ID_CHK_TAKEOVER), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    let wheel_dp = SendMessageW(GetDlgItem(hwnd, ID_CHK_WHEEL), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    let auto_start = SendMessageW(GetDlgItem(hwnd, ID_CHK_AUTOSTART), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;

                    let cb: Option<Box<dyn Fn()>> = SETTINGS_STATE.with(|c| {
                        let mut b = c.borrow_mut();
                        if let Some(st) = b.as_mut() {
                            if let Ok(mut cfg) = st.cfg.lock() {
                                if !addr.is_empty() && !addr.eq_ignore_ascii_case(&cfg.tv_address) {
                                    cfg.tv_address = addr;
                                    st.dev.reset_jar_cache();
                                }
                                cfg.auto_takeover_on_tv = auto_to;
                                cfg.wheel_as_dpad = wheel_dp;
                                cfg.save();
                            }
                            set_autostart(auto_start);
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
                    DestroyWindow(hwnd);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_TEST_DONE => {
            let lbl = GetDlgItem(hwnd, ID_LBL_STATUS);
            let txt = if wp != 0 {
                "连接成功！设备已就绪"
            } else {
                "连接失败，请检查电视 IP、同一局域网及 ADB 开关"
            };
            SetWindowTextW(lbl, to_wide(txt).as_ptr());
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            SETTINGS_STATE.with(|c| {
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
