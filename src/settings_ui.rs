use crate::adb::DeviceController;
use crate::config::{from_wide, is_autostart_enabled, set_autostart, to_wide, AppConfig};
use crate::display::visible_work_area_for_cursor;
use crate::gdi::{apply_dark_control_theme, apply_dwm_dark_titlebar, ThemePalette};
use crate::keymap_ui::show_keymap_window;
use crate::lan_scan::{scan_lan_adb_devices, DiscoveredDevice};
use crate::takeover::current_mode;
use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, FillRect, SetBkColor, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL,
    HBRUSH, HFONT, OUT_DEFAULT_PRECIS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const SETTINGS_CLASS: &str = "MiKVMSettingsWnd";
const ID_EDIT_ADDR: i32 = 201;
const ID_BTN_TEST: i32 = 202;
const ID_LBL_STATUS: i32 = 203;
const ID_CHK_TAKEOVER: i32 = 204;
const ID_BTN_SCAN: i32 = 205;
const ID_CHK_AUTOSTART: i32 = 206;
const ID_BTN_SAVE: i32 = 207;
const ID_BTN_CANCEL: i32 = 208;
const ID_BTN_KEYMAP: i32 = 209;
const ID_LBL_GUIDE: i32 = 210;
const ID_LBL_CHK_TAKEOVER: i32 = 214;
const ID_LBL_CHK_AUTOSTART: i32 = 216;
const ID_CHK_EDGE: i32 = 220;
const ID_CHK_CLIP: i32 = 221;
const ID_CHK_AUTO_INT: i32 = 222;
const ID_LBL_CHK_EDGE: i32 = 230;
const ID_LBL_CHK_CLIP: i32 = 231;
const ID_LBL_CHK_AUTO_INT: i32 = 232;
const ID_EDIT_RES_PC: i32 = 240;
const ID_EDIT_RES_TV: i32 = 241;
const WM_TEST_DONE: u32 = WM_APP + 50;
const WM_SCAN_DONE: u32 = WM_APP + 51;

static SCAN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const ES_AUTOHSCROLL: u32 = 0x0080;
const SS_NOTIFY: u32 = 0x0100;

struct SettingsState {
    hwnd: HWND,
    font: HFONT,
    palette: ThemePalette,
    bg_brush: HBRUSH,
    edit_brush: HBRUSH,
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
        let pal = ThemePalette::current();
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(SETTINGS_CLASS);
        let bg_brush = CreateSolidBrush(pal.win32_bg_bgr);
        let edit_brush = CreateSolidBrush(pal.win32_edit_bg_bgr);
        static CLASS_REG: std::sync::Once = std::sync::Once::new();
        CLASS_REG.call_once(|| {
            let class_brush = CreateSolidBrush(pal.win32_bg_bgr);
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

        let tv_hint = cfg
            .lock()
            .map(|c| c.tv_monitor_hint.clone())
            .unwrap_or_else(|_| "Mi Monitor".to_string());
        let (wa, s) = visible_work_area_for_cursor(current_mode().is_session_active(), &tv_hint, true);
        let sc = |v: f32| (v * s).round() as i32;

        let cw = sc(468.0);
        let ch = sc(512.0);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut wr = RECT { left: 0, top: 0, right: cw, bottom: ch };
        AdjustWindowRectEx(&mut wr, style, 0, 0);
        let ww = wr.right - wr.left;
        let wh = wr.bottom - wr.top;
        let wx = wa.left + ((wa.right - wa.left) - ww) / 2;
        let wy = wa.top + ((wa.bottom - wa.top) - wh) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            cls_w.as_ptr(),
            to_wide("ADB 与多屏键鼠设置 — MiKVM").as_ptr(),
            style,
            wx,
            wy,
            ww,
            wh,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        apply_dwm_dark_titlebar(hwnd, pal.dark);

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
        let (addr, auto_to, edge_sw, res_pc, res_tv, clip_sync, auto_int, guide) = cfg
            .lock()
            .map(|c| {
                (
                    c.tv_address.clone(),
                    c.auto_takeover_on_tv,
                    c.edge_switch,
                    c.edge_resistance_pc,
                    c.edge_resistance_tv,
                    c.clipboard_sync,
                    c.auto_internal_on_tv,
                    c.keymap.summary_text(),
                )
            })
            .unwrap_or_else(|_| {
                (
                    "192.168.1.200:5555".to_string(),
                    true,
                    true,
                    120,
                    180,
                    true,
                    false,
                    crate::keymap::KeyMap::default().summary_text(),
                )
            });

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
            if cls == "BUTTON" || cls == "EDIT" {
                apply_dark_control_theme(hw, pal.dark);
            }
            hw
        };

        let make_checkbox = |label: &str, y: i32, chk_id: i32, lbl_id: i32| -> HWND {
            let box_w = sc(20.0);
            let chk = make_ctrl("BUTTON", "", 0, BS_AUTOCHECKBOX | WS_TABSTOP, pad, y, box_w, sc(24.0), chk_id);
            make_ctrl("STATIC", label, 0, SS_NOTIFY, pad + box_w + sc(4.0), y + sc(2.0), inner - box_w - sc(4.0), sc(22.0), lbl_id);
            chk
        };

        make_ctrl("STATIC", "电视无线 ADB 地址 (IP:端口)：", 0, 0, pad, sc(16.0), inner, sc(20.0), 0);
        make_ctrl("EDIT", &addr, WS_EX_CLIENTEDGE, ES_AUTOHSCROLL | WS_TABSTOP, pad, sc(42.0), inner - sc(174.0), sc(28.0), ID_EDIT_ADDR);
        make_ctrl("BUTTON", "扫描设备", 0, WS_TABSTOP, cw - pad - sc(166.0), sc(41.0), sc(78.0), sc(30.0), ID_BTN_SCAN);
        make_ctrl("BUTTON", "测试连接", 0, WS_TABSTOP, cw - pad - sc(82.0), sc(41.0), sc(82.0), sc(30.0), ID_BTN_TEST);
        make_ctrl("STATIC", "", 0, 0, pad, sc(74.0), inner, sc(18.0), ID_LBL_STATUS);

        let chk_to = make_checkbox("切换到「电视系统」时自动开启直连键鼠控制", sc(96.0), ID_CHK_TAKEOVER, ID_LBL_CHK_TAKEOVER);
        let chk_edge = make_checkbox("多屏时鼠标滑过边界双向无缝跨屏（防误触阻尼）", sc(124.0), ID_CHK_EDGE, ID_LBL_CHK_EDGE);
        let chk_clip = make_checkbox("电脑 ↔ 电视剪贴板按需同步（自动过滤密码隐私）", sc(152.0), ID_CHK_CLIP, ID_LBL_CHK_CLIP);
        let chk_int = make_checkbox("切电视时自动临时切为「仅电脑屏幕」（回 DP 还原）", sc(180.0), ID_CHK_AUTO_INT, ID_LBL_CHK_AUTO_INT);
        let chk_as = make_checkbox("Windows 开机自动启动", sc(208.0), ID_CHK_AUTOSTART, ID_LBL_CHK_AUTOSTART);

        // Edge resistance section placed cleanly below all checkboxes with ample spacing
        make_ctrl("STATIC", "撞边推力（0~300，越大越防误触，0 为一碰即过）：", 0, 0, pad, sc(240.0), inner, sc(20.0), 0);
        let res_y = sc(264.0);
        make_ctrl("STATIC", "电脑 → 电视：", 0, 0, pad, res_y + sc(3.0), sc(98.0), sc(22.0), 0);
        make_ctrl("EDIT", &res_pc.to_string(), WS_EX_CLIENTEDGE, ES_AUTOHSCROLL | WS_TABSTOP, pad + sc(100.0), res_y, sc(68.0), sc(25.0), ID_EDIT_RES_PC);
        make_ctrl("STATIC", "电视 → 电脑：", 0, 0, pad + sc(196.0), res_y + sc(3.0), sc(98.0), sc(22.0), 0);
        make_ctrl("EDIT", &res_tv.to_string(), WS_EX_CLIENTEDGE, ES_AUTOHSCROLL | WS_TABSTOP, pad + sc(296.0), res_y, sc(68.0), sc(25.0), ID_EDIT_RES_TV);

        SendMessageW(chk_to, BM_SETCHECK, if auto_to { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_edge, BM_SETCHECK, if edge_sw { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_clip, BM_SETCHECK, if clip_sync { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_int, BM_SETCHECK, if auto_int { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
        SendMessageW(chk_as, BM_SETCHECK, if is_autostart_enabled() { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);

        make_ctrl("STATIC", &guide, 0, WS_BORDER, pad, sc(302.0), inner, sc(146.0), ID_LBL_GUIDE);

        make_ctrl("BUTTON", "自定义快捷键...", 0, WS_TABSTOP, pad, ch - sc(46.0), sc(126.0), sc(32.0), ID_BTN_KEYMAP);
        make_ctrl("BUTTON", "保存", 0, BS_DEFPUSHBUTTON | WS_TABSTOP, cw - pad - sc(168.0), ch - sc(46.0), sc(78.0), sc(32.0), ID_BTN_SAVE);
        make_ctrl("BUTTON", "取消", 0, WS_TABSTOP, cw - pad - sc(82.0), ch - sc(46.0), sc(82.0), sc(32.0), ID_BTN_CANCEL);

        SETTINGS_STATE.with(|c| {
            *c.borrow_mut() = Some(SettingsState {
                hwnd,
                font,
                palette: pal,
                bg_brush,
                edit_brush,
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
        WM_ERASEBKGND => {
            let hdc = wp as windows_sys::Win32::Graphics::Gdi::HDC;
            let br = SETTINGS_STATE
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
            let info = SETTINGS_STATE.with(|c| {
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
        WM_CTLCOLOREDIT => {
            let hdc = wp as windows_sys::Win32::Graphics::Gdi::HDC;
            let info = SETTINGS_STATE.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|s| (s.palette.win32_edit_bg_bgr, s.palette.win32_text_bgr, s.edit_brush))
            });
            if let Some((bg, fg, br)) = info {
                SetBkColor(hdc, bg);
                SetTextColor(hdc, fg);
                return br as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as i32;
            match id {
                ID_LBL_CHK_TAKEOVER
                | ID_LBL_CHK_AUTOSTART
                | ID_LBL_CHK_EDGE
                | ID_LBL_CHK_CLIP
                | ID_LBL_CHK_AUTO_INT => {
                    let chk_id = id - 10;
                    let chk = GetDlgItem(hwnd, chk_id);
                    if !chk.is_null() {
                        let cur = SendMessageW(chk, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                        SendMessageW(
                            chk,
                            BM_SETCHECK,
                            if cur { BST_UNCHECKED } else { BST_CHECKED } as WPARAM,
                            0,
                        );
                    }
                    0
                }
                ID_BTN_SCAN => {
                    if SCAN_IN_PROGRESS.swap(true, Ordering::AcqRel) {
                        return 0;
                    }
                    let hint = get_edit_text(hwnd, ID_EDIT_ADDR);
                    let lbl = GetDlgItem(hwnd, ID_LBL_STATUS);
                    let btn = GetDlgItem(hwnd, ID_BTN_SCAN);
                    if !btn.is_null() {
                        EnableWindow(btn, 0);
                    }
                    SetWindowTextW(lbl, to_wide("正在扫描局域网 5555 无线 ADB 设备...").as_ptr());
                    let dev = SETTINGS_STATE.with(|c| c.borrow().as_ref().map(|s| Arc::clone(&s.dev)));
                    if let Some(dev) = dev {
                        let hwnd_raw = hwnd as usize;
                        std::thread::spawn(move || {
                            let found = scan_lan_adb_devices(&dev, &hint);
                            let boxed = Box::into_raw(Box::new(found)) as LPARAM;
                            let posted = PostMessageW(hwnd_raw as HWND, WM_SCAN_DONE, 0, boxed);
                            if posted == 0 {
                                // Window closed before scan completed: free cleanly & clear guard
                                let _ = Box::from_raw(boxed as *mut Vec<DiscoveredDevice>);
                                SCAN_IN_PROGRESS.store(false, Ordering::Release);
                            }
                        });
                    } else {
                        SCAN_IN_PROGRESS.store(false, Ordering::Release);
                        if !btn.is_null() {
                            EnableWindow(btn, 1);
                        }
                    }
                    0
                }
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
                ID_BTN_KEYMAP => {
                    let cfg_opt = SETTINGS_STATE.with(|c| c.borrow().as_ref().map(|s| Arc::clone(&s.cfg)));
                    if let Some(cfg_arc) = cfg_opt {
                        let hwnd_raw = hwnd as usize;
                        let cfg_for_cb = Arc::clone(&cfg_arc);
                        show_keymap_window(cfg_arc, move || {
                            if let Ok(c) = cfg_for_cb.lock() {
                                let summary = c.keymap.summary_text();
                                let h = hwnd_raw as HWND;
                                unsafe {
                                    if IsWindow(h) != 0 {
                                        let lbl = GetDlgItem(h, ID_LBL_GUIDE);
                                        if !lbl.is_null() {
                                            SetWindowTextW(lbl, to_wide(&summary).as_ptr());
                                        }
                                    }
                                }
                            }
                            let cb_ptr: *const dyn Fn() = SETTINGS_STATE.with(|c| {
                                c.borrow()
                                    .as_ref()
                                    .and_then(|s| s.on_saved.as_deref().map(|f| f as *const _))
                                    .unwrap_or(ptr::null::<fn()>() as *const _)
                            });
                            if !cb_ptr.is_null() {
                                unsafe { (*cb_ptr)(); }
                            }
                        });
                    }
                    0
                }
                ID_BTN_SAVE => {
                    let raw = get_edit_text(hwnd, ID_EDIT_ADDR);
                    let addr = AppConfig::normalize_address(&raw);
                    let auto_to = SendMessageW(GetDlgItem(hwnd, ID_CHK_TAKEOVER), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    let edge_sw = SendMessageW(GetDlgItem(hwnd, ID_CHK_EDGE), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    let res_pc = get_edit_text(hwnd, ID_EDIT_RES_PC).trim().parse::<i32>().ok().map(|v| v.clamp(0, 300));
                    let res_tv = get_edit_text(hwnd, ID_EDIT_RES_TV).trim().parse::<i32>().ok().map(|v| v.clamp(0, 300));
                    let clip_sync = SendMessageW(GetDlgItem(hwnd, ID_CHK_CLIP), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    let auto_int = SendMessageW(GetDlgItem(hwnd, ID_CHK_AUTO_INT), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
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
                                cfg.edge_switch = edge_sw;
                                if let Some(v) = res_pc {
                                    cfg.edge_resistance_pc = v;
                                }
                                if let Some(v) = res_tv {
                                    cfg.edge_resistance_tv = v;
                                }
                                cfg.clipboard_sync = clip_sync;
                                cfg.auto_internal_on_tv = auto_int;
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
        WM_SCAN_DONE => {
            SCAN_IN_PROGRESS.store(false, Ordering::Release);
            let btn = GetDlgItem(hwnd, ID_BTN_SCAN);
            if !btn.is_null() {
                EnableWindow(btn, 1);
            }
            let lbl = GetDlgItem(hwnd, ID_LBL_STATUS);
            if lp != 0 {
                let found = Box::from_raw(lp as *mut Vec<DiscoveredDevice>);
                if let Some(best) = found.first() {
                    let edit = GetDlgItem(hwnd, ID_EDIT_ADDR);
                    if !edit.is_null() {
                        SetWindowTextW(edit, to_wide(&best.addr).as_ptr());
                    }
                    let msg = if found.len() == 1 {
                        format!("已发现: {} ({})，点保存生效", best.addr, best.label)
                    } else {
                        format!(
                            "发现 {} 台，已填入 {} ({})",
                            found.len(),
                            best.addr,
                            best.label
                        )
                    };
                    SetWindowTextW(lbl, to_wide(&msg).as_ptr());
                } else {
                    SetWindowTextW(
                        lbl,
                        to_wide("未发现局域网 5555 无线 ADB 设备，请确认同网段").as_ptr(),
                    );
                }
            }
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
                    if !st.edit_brush.is_null() {
                        DeleteObject(st.edit_brush as _);
                    }
                }
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
