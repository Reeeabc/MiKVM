#![windows_subsystem = "windows"]

mod adb;
mod clipboard;
mod config;
#[macro_use]
mod diag;
mod display;
mod flyout;
mod gdi;
mod keymap;
mod keymap_ui;
mod scrcpy_proto;
mod settings_ui;
mod takeover;

use crate::adb::DeviceController;
use crate::config::{
    exe_dir, from_wide, is_system_dark_mode, resolve_asset_dir, resolve_scrcpy_dir, to_wide,
    AppConfig, DeviceStatus, SignalSource,
};
use crate::display::{
    parse_power_broadcast, register_lid_notification, unregister_lid_notification, PowerEvent,
};
use crate::flyout::{
    create_flyout, hide_flyout, is_flyout_visible, refresh_flyout_theme,
    set_flyout_optimistic_source, set_flyout_refreshing, show_flyout_near_tray,
    update_flyout_status, was_just_hidden, FlyoutAction,
};
use crate::gdi::{create_tray_icon, GdipGuard};
use crate::settings_ui::show_settings_window;
use crate::takeover::{
    current_mode, handle_raw_input, TakeoverMode, TakeoverSession, WM_CONTROL_EXITED,
    WM_CONTROL_READY, WM_DP_RETURNED, WM_EDGE_ENTER_TV, WM_RECOVERY_FAILED,
    WM_TRIGGER_RETURN_DP, WM_TRIGGER_TOGGLE_FOCUS, WM_TV_CLIPBOARD, WM_TV_EDGE_EXIT,
};
use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::HPOWERNOTIFY;

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(
        lp_mutex_attributes: *const std::ffi::c_void,
        b_initial_owner: i32,
        lp_name: *const u16,
    ) -> *mut std::ffi::c_void;
}
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HOST_CLASS: &str = "MiKVMTrayHost";
const TRAY_UID: u32 = 1;
const WM_TRAY_ICON: u32 = WM_APP + 1;
const WM_STATUS_UPDATED: u32 = WM_APP + 2;
const WM_SOURCE_OPTIMISTIC: u32 = WM_APP + 3;
const WM_SHOW_BALLOON: u32 = WM_APP + 4;

const IDM_OPEN: usize = 1001;
const IDM_TV: usize = 1002;
const IDM_DP: usize = 1003;
const IDM_TOGGLE_FOCUS: usize = 1004;
const IDM_TOGGLE_CTRL: usize = 1005;
const IDM_SETTINGS: usize = 1006;
const IDM_EXIT: usize = 1007;

static BUSY: AtomicBool = AtomicBool::new(false);

struct AppHost {
    hwnd: HWND,
    cfg: Arc<Mutex<AppConfig>>,
    dev: Arc<DeviceController>,
    takeover: Arc<Mutex<TakeoverSession>>,
    idle_icon: HICON,
    active_icon: HICON,
    lid_notify: HPOWERNOTIFY,
}

thread_local! {
    static APP_HOST: RefCell<Option<AppHost>> = const { RefCell::new(None) };
}

fn with_host<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AppHost) -> R,
{
    APP_HOST.with(|c| c.borrow_mut().as_mut().map(f))
}

fn copy_wide_fixed(src: &str, dst: &mut [u16]) {
    dst.fill(0);
    for (i, ch) in src.encode_utf16().take(dst.len().saturating_sub(1)).enumerate() {
        dst[i] = ch;
    }
}

unsafe fn set_tray_icon_state(hwnd: HWND, icon: HICON, tip: &str, add: bool) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    nid.uCallbackMessage = WM_TRAY_ICON;
    nid.hIcon = icon;
    copy_wide_fixed(tip, &mut nid.szTip);
    Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &nid);
}

unsafe fn show_tray_balloon(hwnd: HWND, title: &str, msg: &str) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_WARNING;
    copy_wide_fixed(title, &mut nid.szInfoTitle);
    copy_wide_fixed(msg, &mut nid.szInfo);
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

fn trigger_refresh(silent_if_connected: bool) {
    let Some((hwnd, dev, addr)) = with_host(|h| {
        let addr = h.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default();
        (h.hwnd as usize, Arc::clone(&h.dev), addr)
    }) else {
        return;
    };
    set_flyout_refreshing(&addr, silent_if_connected);
    std::thread::spawn(move || {
        let st = dev.query_status();
        let boxed = Box::into_raw(Box::new(st)) as LPARAM;
        unsafe {
            PostMessageW(hwnd as HWND, WM_STATUS_UPDATED, 0, boxed);
        }
    });
}

fn handle_source_click(src: SignalSource) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    if src == SignalSource::TvSystem {
        hide_flyout();
    }

    let Some((hwnd, cfg, dev, takeover)) = with_host(|h| {
        (
            h.hwnd as usize,
            Arc::clone(&h.cfg),
            Arc::clone(&h.dev),
            Arc::clone(&h.takeover),
        )
    }) else {
        BUSY.store(false, Ordering::Release);
        return;
    };

    if src != SignalSource::TvSystem {
        if let Ok(mut to) = takeover.lock() {
            to.stop();
        }
        update_tray_visual();
    }

    std::thread::spawn(move || {
        let (addr, auto_to, dp, h1, h2) = cfg
            .lock()
            .map(|c| {
                (
                    c.tv_address.clone(),
                    c.auto_takeover_on_tv,
                    c.dp_input,
                    c.hdmi1_input,
                    c.hdmi2_input,
                )
            })
            .unwrap_or_else(|_| (String::new(), true, 29, 23, 24));

        if !dev.ensure_connected() {
            let msg = Box::into_raw(Box::new(format!("无法连接到 {}", addr))) as LPARAM;
            unsafe {
                PostMessageW(hwnd as HWND, WM_SHOW_BALLOON, 0, msg);
            }
            BUSY.store(false, Ordering::Release);
            return;
        }

        if src == SignalSource::TvSystem {
            if auto_to {
                let res = takeover.lock().map(|to| to.start_control_blocking());
                if let Ok(Err(err_msg)) = res {
                    let msg = Box::into_raw(Box::new(err_msg)) as LPARAM;
                    unsafe {
                        PostMessageW(hwnd as HWND, WM_SHOW_BALLOON, 0, msg);
                    }
                    BUSY.store(false, Ordering::Release);
                    return;
                }
            }
            let _ = dev.switch_to_tv_desktop();
            unsafe {
                PostMessageW(
                    hwnd as HWND,
                    WM_SOURCE_OPTIMISTIC,
                    src as WPARAM,
                    if auto_to { 1 } else { 0 },
                );
            }
            if auto_to {
                if let Ok(to) = takeover.lock() {
                    to.start_dp_return_watcher();
                }
            }
        } else {
            let input_id = match src {
                SignalSource::Hdmi1 => h1,
                SignalSource::Hdmi2 => h2,
                _ => dp,
            };
            let _ = dev.switch_to_external(input_id);
            unsafe {
                PostMessageW(hwnd as HWND, WM_SOURCE_OPTIMISTIC, src as WPARAM, 0);
            }
        }
        BUSY.store(false, Ordering::Release);
    });
}

fn start_control_only() {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some((hwnd, dev, takeover)) = with_host(|h| {
        (h.hwnd as usize, Arc::clone(&h.dev), Arc::clone(&h.takeover))
    }) else {
        BUSY.store(false, Ordering::Release);
        return;
    };

    std::thread::spawn(move || {
        if dev.ensure_connected() {
            if let Ok(Err(e)) = takeover.lock().map(|to| to.start_control_blocking()) {
                let msg = Box::into_raw(Box::new(e)) as LPARAM;
                unsafe {
                    PostMessageW(hwnd as HWND, WM_SHOW_BALLOON, 0, msg);
                }
            }
        }
        BUSY.store(false, Ordering::Release);
    });
}

fn update_tray_visual() {
    let mode = current_mode();
    let info = with_host(|h| {
        let (dp_key, focus_key) = h
            .cfg
            .lock()
            .map(|c| (c.keymap.return_dp_label(), c.keymap.toggle_focus_label()))
            .unwrap_or_else(|_| ("Ctrl+Alt+D".to_string(), "Ctrl+Alt+S".to_string()));
        let recovering = h
            .takeover
            .lock()
            .map(|t| t.is_recovering())
            .unwrap_or(false);
        let icon = if mode.is_tv_capturing() {
            h.active_icon
        } else {
            h.idle_icon
        };
        (h.hwnd, icon, dp_key, focus_key, recovering)
    });
    if let Some((hwnd, icon, dp_key, focus_key, recovering)) = info {
        let tip = if recovering {
            "MiKVM 网络波动，正在后台静默重连电视...".to_string()
        } else {
            match mode {
                TakeoverMode::TvFocused => {
                    format!("MiKVM 控制电视中 ({} 切电脑 / {} 回DP)", focus_key, dp_key)
                }
                TakeoverMode::TvLocked => {
                    format!("MiKVM 单屏独占电视 ({} 切回电脑 DP)", dp_key)
                }
                TakeoverMode::TvStandby => {
                    format!("MiKVM 电视待命 · 焦点在电脑 (滑边或 {} 切入)", focus_key)
                }
                TakeoverMode::Idle => "MiKVM — 信号源 / 背光 / 多屏键鼠控制".to_string(),
            }
        };
        unsafe {
            set_tray_icon_state(hwnd, icon, &tip, false);
        }
    }
}

fn toggle_flyout_panel() {
    if is_flyout_visible() {
        hide_flyout();
        return;
    }
    if was_just_hidden() {
        return;
    }
    let (tv_hint, lid_open) = with_host(|h| {
        let hint = h
            .cfg
            .lock()
            .map(|c| c.tv_monitor_hint.clone())
            .unwrap_or_else(|_| "Mi Monitor".to_string());
        let lid = h.takeover.lock().map(|t| t.is_lid_open()).unwrap_or(true);
        (hint, lid)
    })
    .unwrap_or_else(|| ("Mi Monitor".to_string(), true));

    show_flyout_near_tray(current_mode().is_session_active(), &tv_hint, lid_open);
    trigger_refresh(true);
}

fn open_settings() {
    let Some((cfg, dev, takeover)) = with_host(|h| {
        (Arc::clone(&h.cfg), Arc::clone(&h.dev), Arc::clone(&h.takeover))
    }) else {
        return;
    };
    show_settings_window(cfg, dev, move || {
        if let Ok(mut to) = takeover.lock() {
            to.sync_config();
        }
        update_tray_visual();
    });
}

fn exit_app() {
    let info = with_host(|h| {
        (
            h.hwnd,
            h.idle_icon,
            h.active_icon,
            h.lid_notify,
            Arc::clone(&h.takeover),
            Arc::clone(&h.dev),
        )
    });
    if let Some((hwnd, idle_icon, active_icon, lid_notify, takeover, dev)) = info {
        if let Ok(mut to) = takeover.lock() {
            to.stop();
        }
        dev.stop_daemon_now();
        unregister_lid_notification(lid_notify);
        unsafe {
            remove_tray_icon(hwnd);
            DestroyIcon(idle_icon);
            DestroyIcon(active_icon);
            PostQuitMessage(0);
        }
    }
}

unsafe fn show_right_click_menu(hwnd: HWND) {
    let mode = current_mode();
    let running = with_host(|h| {
        h.takeover
            .lock()
            .map(|t| t.is_running() || t.is_recovering())
            .unwrap_or(false)
    })
    .unwrap_or(false);
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, IDM_OPEN, to_wide("打开控制面板").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_TV, to_wide("切换到电视系统").as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_DP, to_wide("切回电脑 DP").as_ptr());
    if mode == TakeoverMode::TvFocused || mode == TakeoverMode::TvStandby {
        let focus_lbl = if mode == TakeoverMode::TvFocused {
            "切回电脑焦点 (保持电视画面)"
        } else {
            "切入电视控制"
        };
        AppendMenuW(menu, MF_STRING, IDM_TOGGLE_FOCUS, to_wide(focus_lbl).as_ptr());
    }
    AppendMenuW(
        menu,
        MF_STRING,
        IDM_TOGGLE_CTRL,
        to_wide(if running { "停止键鼠通道" } else { "仅开启直连键鼠控制" }).as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_SETTINGS, to_wide("ADB 与多屏设置...").as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_EXIT, to_wide("退出").as_ptr());

    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, ptr::null());
    DestroyMenu(menu);
}

fn source_from_wparam(wp: WPARAM) -> SignalSource {
    match wp {
        x if x == SignalSource::TvSystem as WPARAM => SignalSource::TvSystem,
        x if x == SignalSource::Dp as WPARAM => SignalSource::Dp,
        x if x == SignalSource::Hdmi1 as WPARAM => SignalSource::Hdmi1,
        x if x == SignalSource::Hdmi2 as WPARAM => SignalSource::Hdmi2,
        _ => SignalSource::Unknown,
    }
}

unsafe extern "system" fn host_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            handle_raw_input(lp);
            0
        }
        WM_DISPLAYCHANGE => {
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.on_display_changed();
                }
            }
            update_tray_visual();
            0
        }
        WM_POWERBROADCAST => {
            if let Some(ev) = parse_power_broadcast(msg, wp, lp) {
                let to_opt = with_host(|h| Arc::clone(&h.takeover));
                let mut should_refresh = false;
                if let Some(to) = to_opt {
                    if let Ok(mut g) = to.lock() {
                        match ev {
                            PowerEvent::LidChanged(lid_open) => g.on_lid_state_changed(lid_open),
                            PowerEvent::Suspend => g.on_power_suspend_ui(),
                            PowerEvent::Resume => {
                                should_refresh = g.on_power_resume_ui();
                            }
                        }
                    }
                }
                update_tray_visual();
                if should_refresh && is_flyout_visible() {
                    trigger_refresh(true);
                }
            }
            1
        }
        WM_SETTINGCHANGE => {
            let is_theme_change = if lp == 0 {
                true
            } else {
                let p = lp as *const u16;
                let mut len = 0usize;
                while len < 64 && *p.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(p, len);
                from_wide(slice).eq_ignore_ascii_case("ImmersiveColorSet")
            };
            if is_theme_change {
                let dark_tb = is_system_dark_mode();
                let new_idle = create_tray_icon(false, dark_tb);
                let new_active = create_tray_icon(true, dark_tb);
                let swapped = with_host(|h| {
                    let old_idle = std::mem::replace(&mut h.idle_icon, new_idle);
                    let old_active = std::mem::replace(&mut h.active_icon, new_active);
                    (old_idle, old_active)
                });
                if let Some((old_idle, old_active)) = swapped {
                    update_tray_visual();
                    DestroyIcon(old_idle);
                    DestroyIcon(old_active);
                } else {
                    DestroyIcon(new_idle);
                    DestroyIcon(new_active);
                }
                refresh_flyout_theme();
            }
            0
        }
        WM_TRAY_ICON => {
            let ev = (lp & 0xFFFF) as u32;
            if ev == WM_LBUTTONUP {
                toggle_flyout_panel();
            } else if ev == WM_RBUTTONUP {
                hide_flyout();
                show_right_click_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as usize;
            match id {
                IDM_OPEN => toggle_flyout_panel(),
                IDM_TV => handle_source_click(SignalSource::TvSystem),
                IDM_DP => handle_source_click(SignalSource::Dp),
                IDM_TOGGLE_FOCUS => {
                    let to_opt = with_host(|h| Arc::clone(&h.takeover));
                    if let Some(to) = to_opt {
                        if let Ok(mut g) = to.lock() {
                            g.toggle_focus_ui();
                        }
                    }
                    update_tray_visual();
                }
                IDM_TOGGLE_CTRL => {
                    let to_opt = with_host(|h| Arc::clone(&h.takeover));
                    let running = to_opt
                        .as_ref()
                        .and_then(|t| {
                            t.lock()
                                .ok()
                                .map(|g| g.is_running() || g.is_recovering())
                        })
                        .unwrap_or(false);
                    if running {
                        if let Some(to) = to_opt {
                            if let Ok(mut g) = to.lock() {
                                g.stop();
                            }
                        }
                        update_tray_visual();
                    } else {
                        start_control_only();
                    }
                }
                IDM_SETTINGS => open_settings(),
                IDM_EXIT => exit_app(),
                _ => {}
            }
            0
        }
        WM_TRIGGER_RETURN_DP => {
            handle_source_click(SignalSource::Dp);
            0
        }
        WM_TRIGGER_TOGGLE_FOCUS => {
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.toggle_focus_ui();
                }
            }
            update_tray_visual();
            0
        }
        WM_EDGE_ENTER_TV => {
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.on_edge_enter_tv_ui(wp);
                }
            }
            update_tray_visual();
            0
        }
        WM_TV_EDGE_EXIT => {
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.on_tv_edge_exit_ui(wp);
                }
            }
            update_tray_visual();
            0
        }
        WM_TV_CLIPBOARD => {
            if lp != 0 {
                let text = Box::from_raw(lp as *mut String);
                let to_opt = with_host(|h| Arc::clone(&h.takeover));
                if let Some(to) = to_opt {
                    if let Ok(mut g) = to.lock() {
                        g.on_tv_clipboard_ui(*text);
                    }
                }
            }
            0
        }
        WM_CONTROL_READY => {
            let prefer_standby = wp != 0;
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.on_control_ready_ui(prefer_standby);
                }
            }
            update_tray_visual();
            0
        }
        WM_CONTROL_EXITED => {
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.on_control_exited_ui();
                }
            }
            update_tray_visual();
            0
        }
        WM_RECOVERY_FAILED => {
            let notify = wp != 0;
            let to_opt = with_host(|h| Arc::clone(&h.takeover));
            if let Some(to) = to_opt {
                if let Ok(mut g) = to.lock() {
                    g.stop();
                }
            }
            update_tray_visual();
            if notify {
                show_tray_balloon(
                    hwnd,
                    "MiKVM",
                    "电视连接已断开，后台自动重连失败，可点击托盘重连",
                );
            }
            if is_flyout_visible() {
                trigger_refresh(false);
            }
            0
        }
        WM_DP_RETURNED => {
            let info = with_host(|h| {
                let addr = h.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default();
                (Arc::clone(&h.takeover), addr)
            });
            if let Some((to, addr)) = info {
                if let Ok(mut g) = to.lock() {
                    g.stop();
                }
                update_tray_visual();
                set_flyout_optimistic_source(SignalSource::Dp, &addr, TakeoverMode::Idle);
                trigger_refresh(true);
            }
            0
        }
        WM_STATUS_UPDATED => {
            if lp != 0 {
                let ds = Box::from_raw(lp as *mut DeviceStatus);
                let addr = with_host(|h| h.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default())
                    .unwrap_or_default();
                update_flyout_status(&ds, &addr, current_mode());
            }
            0
        }
        WM_SOURCE_OPTIMISTIC => {
            let src = source_from_wparam(wp);
            let active = lp != 0;
            let addr = with_host(|h| h.cfg.lock().map(|c| c.tv_address.clone()).unwrap_or_default())
                .unwrap_or_default();
            let mode = if active {
                let cur = current_mode();
                if cur.is_session_active() { cur } else { TakeoverMode::TvFocused }
            } else {
                TakeoverMode::Idle
            };
            set_flyout_optimistic_source(src, &addr, mode);
            0
        }
        WM_SHOW_BALLOON => {
            if lp != 0 {
                let text = Box::from_raw(lp as *mut String);
                show_tray_balloon(hwnd, "MiKVM", &text);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let mutex = CreateMutexW(
            ptr::null(),
            1,
            to_wide("Local\\MiKVM_SingleInstance").as_ptr(),
        );
        if mutex.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }

    let _gdip = GdipGuard::start();

    let base = exe_dir();
    let cfg_path = if base.join("config.ini").exists() {
        base.join("config.ini")
    } else if let Some(p2) = base.parent().and_then(|p| p.parent()) {
        if p2.join("config.ini").exists() {
            p2.join("config.ini")
        } else {
            base.join("config.ini")
        }
    } else {
        base.join("config.ini")
    };

    let cfg_dir = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| base.clone());
    let loaded_cfg = AppConfig::load(cfg_path);
    let cli_debug = std::env::args().any(|a| a.eq_ignore_ascii_case("--debug-log"));
    diag::init(cfg_dir.join("mikvm.log"), loaded_cfg.debug_log || cli_debug);
    diag_log!(
        "APP",
        "start addr={} edge={} res_pc={} res_tv={} auto_int={}",
        loaded_cfg.tv_address,
        loaded_cfg.edge_switch,
        loaded_cfg.edge_resistance_pc,
        loaded_cfg.edge_resistance_tv,
        loaded_cfg.auto_internal_on_tv
    );

    let cfg = Arc::new(Mutex::new(loaded_cfg));
    let scrcpy_dir = resolve_scrcpy_dir(&base);
    let assets_dir = resolve_asset_dir(&base);
    let adb_path = scrcpy_dir.join("adb.exe");
    let dev = Arc::new(DeviceController::new(adb_path, assets_dir, Arc::clone(&cfg)));

    let host_hwnd = unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(HOST_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: hinst,
            lpszClassName: cls_w.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            0,
            cls_w.as_ptr(),
            to_wide("MiKVM Host").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        )
    };

    let lid_notify = register_lid_notification(host_hwnd);
    let dark_tb = is_system_dark_mode();
    let idle_icon = create_tray_icon(false, dark_tb);
    let active_icon = create_tray_icon(true, dark_tb);
    let takeover = Arc::new(Mutex::new(TakeoverSession::new(
        scrcpy_dir,
        Arc::clone(&cfg),
        Arc::clone(&dev),
        host_hwnd,
    )));

    APP_HOST.with(|c| {
        *c.borrow_mut() = Some(AppHost {
            hwnd: host_hwnd,
            cfg: Arc::clone(&cfg),
            dev: Arc::clone(&dev),
            takeover: Arc::clone(&takeover),
            idle_icon,
            active_icon,
            lid_notify,
        });
    });

    let dev_for_flyout = Arc::clone(&dev);
    let takeover_for_flyout = Arc::clone(&takeover);
    let _flyout_hwnd = create_flyout(move |act| match act {
        FlyoutAction::SwitchSource(src) => handle_source_click(src),
        FlyoutAction::SetBacklight(v, commit) => dev_for_flyout.queue_backlight(v, commit),
        FlyoutAction::SetVolume(v) => dev_for_flyout.queue_volume(v),
        FlyoutAction::VisibilityChanged(vis) => {
            if vis {
                dev_for_flyout.set_keep_warm(true);
            } else {
                let running = takeover_for_flyout
                    .lock()
                    .map(|t| t.is_running())
                    .unwrap_or(false);
                if !running {
                    dev_for_flyout.set_keep_warm(false);
                }
            }
        }
        FlyoutAction::OpenSettings => open_settings(),
        FlyoutAction::Refresh => trigger_refresh(false),
        FlyoutAction::Exit => exit_app(),
    });

    unsafe {
        set_tray_icon_state(
            host_hwnd,
            idle_icon,
            "MiKVM — 信号源 / 背光 / 多屏键鼠控制",
            true,
        );

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        CoUninitialize();
    }
}
