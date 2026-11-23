use crate::config::{to_wide, DeviceStatus, SignalSource};
use crate::gdi::*;
use std::cell::RefCell;
use std::ptr;
use std::time::Instant;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EndPaint, GetDC, GetDeviceCaps, InvalidateRect, ReleaseDC, SelectObject, LOGPIXELSX,
    PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipCreateFromHDC, GdipDeleteGraphics, GdipSetSmoothingMode, GdipSetTextRenderingHint,
    SmoothingModeAntiAlias, StringAlignmentCenter, StringAlignmentFar, StringAlignmentNear,
    TextRenderingHintClearTypeGridFit,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const FLYOUT_CLASS: &str = "MiTVSwitchFluentFlyout";
const TIMER_BL_DEBOUNCE: usize = 101;
const TIMER_VOL_DEBOUNCE: usize = 102;
const DWMWCP_ROUND: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlyoutAction {
    SwitchSource(SignalSource),
    SetBacklight(i32),
    SetVolume(i32),
    OpenSettings,
    Refresh,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HitPart {
    None,
    Tile(SignalSource),
    SliderBacklight,
    SliderVolume,
    BtnSettings,
    BtnRefresh,
    BtnExit,
}

#[derive(Clone, Copy)]
struct RectF4 {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl RectF4 {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

struct FlyoutLayout {
    s: f32,
    w: i32,
    h: i32,
    pad: f32,
    tiles: [(SignalSource, &'static str, RectF4, RectF4); 4], // (src, label, full_hit, card_rect)
    bl_slider: RectF4,
    vol_slider: RectF4,
    footer_y: f32,
    btn_settings: RectF4,
    btn_refresh: RectF4,
    btn_exit: RectF4,
}

impl FlyoutLayout {
    fn new(s: f32) -> Self {
        let w = (376.0 * s).round() as i32;
        let h = (422.0 * s).round() as i32;
        let wf = w as f32;
        let hf = h as f32;
        let pad = 20.0 * s;
        let inner_w = wf - pad * 2.0;
        let col_gap = 14.0 * s;
        let tile_w = (inner_w - col_gap) / 2.0;
        let tile_h = 78.0 * s;
        let card_h = tile_h - 26.0 * s;
        let grid_top = 84.0 * s;
        let row2 = grid_top + tile_h + 12.0 * s;

        let make_tile = |src, label, x, y| {
            (
                src,
                label,
                RectF4 { x, y, w: tile_w, h: tile_h },
                RectF4 { x, y, w: tile_w, h: card_h },
            )
        };

        let icon_w = 26.0 * s;
        let val_w = 34.0 * s;
        let slider_x = pad + icon_w + 6.0 * s;
        let slider_w = inner_w - icon_w - val_w - 12.0 * s;

        let footer_h = 52.0 * s;
        let footer_y = hf - footer_h;
        let btn_y = footer_y + 10.0 * s;
        let btn_h = 31.0 * s;

        Self {
            s,
            w,
            h,
            pad,
            tiles: [
                make_tile(SignalSource::TvSystem, "电视系统", pad, grid_top),
                make_tile(SignalSource::Dp, "电脑 DP", pad + tile_w + col_gap, grid_top),
                make_tile(SignalSource::Hdmi1, "HDMI 1", pad, row2),
                make_tile(SignalSource::Hdmi2, "HDMI 2", pad + tile_w + col_gap, row2),
            ],
            bl_slider: RectF4 { x: slider_x, y: 280.0 * s, w: slider_w, h: 26.0 * s },
            vol_slider: RectF4 { x: slider_x, y: 334.0 * s, w: slider_w, h: 26.0 * s },
            footer_y,
            btn_settings: RectF4 { x: pad, y: btn_y, w: 92.0 * s, h: btn_h },
            btn_refresh: RectF4 { x: pad + 100.0 * s, y: btn_y, w: 62.0 * s, h: btn_h },
            btn_exit: RectF4 { x: wf - pad - 62.0 * s, y: btn_y, w: 62.0 * s, h: btn_h },
        }
    }

    fn hit_test(&self, px: f32, py: f32) -> HitPart {
        for (src, _, hit, _) in &self.tiles {
            if hit.contains(px, py) {
                return HitPart::Tile(*src);
            }
        }
        if self.bl_slider.contains(px, py) {
            return HitPart::SliderBacklight;
        }
        if self.vol_slider.contains(px, py) {
            return HitPart::SliderVolume;
        }
        if self.btn_settings.contains(px, py) {
            return HitPart::BtnSettings;
        }
        if self.btn_refresh.contains(px, py) {
            return HitPart::BtnRefresh;
        }
        if self.btn_exit.contains(px, py) {
            return HitPart::BtnExit;
        }
        HitPart::None
    }

    fn slider_value_from_x(&self, r: &RectF4, px: f32) -> i32 {
        let pad = 11.0 * self.s;
        let track_w = (r.w - pad * 2.0).max(1.0);
        let ratio = ((px - (r.x + pad)) / track_w).clamp(0.0, 1.0);
        (ratio * 100.0).round() as i32
    }
}

struct FlyoutState {
    hwnd: HWND,
    layout: FlyoutLayout,
    connected: bool,
    refreshing: bool,
    status_text: String,
    active_source: SignalSource,
    backlight: Option<i32>,
    volume: Option<i32>,
    hover: HitPart,
    pressed: HitPart,
    dragging_bl: bool,
    dragging_vol: bool,
    visible: bool,
    suppress_deactivate: bool,
    last_hidden: Option<Instant>,
    on_action: Option<Box<dyn Fn(FlyoutAction)>>,
}

thread_local! {
    static FLYOUT_STATE: RefCell<Option<FlyoutState>> = const { RefCell::new(None) };
}

fn current_dpi_scale() -> f32 {
    unsafe {
        let hdc = GetDC(ptr::null_mut());
        let dpi = if !hdc.is_null() {
            let d = GetDeviceCaps(hdc, LOGPIXELSX as i32);
            ReleaseDC(ptr::null_mut(), hdc);
            d
        } else {
            96
        };
        (dpi as f32 / 96.0).max(1.0)
    }
}

pub fn create_flyout<F>(on_action: F) -> HWND
where
    F: Fn(FlyoutAction) + 'static,
{
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(FLYOUT_CLASS);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(flyout_wnd_proc),
            hInstance: hinst,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls_w.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);

        let scale = current_dpi_scale();
        let layout = FlyoutLayout::new(scale);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls_w.as_ptr(),
            to_wide("MiTV-Switch").as_ptr(),
            WS_POPUP,
            0,
            0,
            layout.w,
            layout.h,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );

        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );

        FLYOUT_STATE.with(|cell| {
            *cell.borrow_mut() = Some(FlyoutState {
                hwnd,
                layout,
                connected: false,
                refreshing: false,
                status_text: "正在连接...".to_string(),
                active_source: SignalSource::Unknown,
                backlight: None,
                volume: None,
                hover: HitPart::None,
                pressed: HitPart::None,
                dragging_bl: false,
                dragging_vol: false,
                visible: false,
                suppress_deactivate: false,
                last_hidden: None,
                on_action: Some(Box::new(on_action)),
            });
        });
        hwnd
    }
}

pub fn is_flyout_visible() -> bool {
    FLYOUT_STATE.with(|c| c.borrow().as_ref().map(|s| s.visible).unwrap_or(false))
}

pub fn was_just_hidden() -> bool {
    FLYOUT_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| s.last_hidden)
            .map(|t| t.elapsed().as_millis() < 220)
            .unwrap_or(false)
    })
}

pub fn show_flyout_near_tray() {
    FLYOUT_STATE.with(|c| {
        let mut borrow = c.borrow_mut();
        if let Some(st) = borrow.as_mut() {
            unsafe {
                let mut wa: RECT = std::mem::zeroed();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
                let margin = (12.0 * st.layout.s).round() as i32;
                let x = wa.right - st.layout.w - margin;
                let y = wa.bottom - st.layout.h - margin;
                st.suppress_deactivate = true;
                st.visible = true;
                SetWindowPos(
                    st.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    st.layout.w,
                    st.layout.h,
                    SWP_SHOWWINDOW,
                );
                SetForegroundWindow(st.hwnd);
                InvalidateRect(st.hwnd, ptr::null(), 0);
                st.suppress_deactivate = false;
            }
        }
    });
}

pub fn hide_flyout() {
    FLYOUT_STATE.with(|c| {
        let mut borrow = c.borrow_mut();
        if let Some(st) = borrow.as_mut() {
            if st.visible {
                st.visible = false;
                st.hover = HitPart::None;
                st.pressed = HitPart::None;
                st.last_hidden = Some(Instant::now());
                unsafe {
                    ShowWindow(st.hwnd, SW_HIDE);
                }
            }
        }
    });
}

pub fn set_flyout_refreshing(addr: &str, silent_if_connected: bool) {
    FLYOUT_STATE.with(|c| {
        let mut borrow = c.borrow_mut();
        if let Some(st) = borrow.as_mut() {
            if silent_if_connected && st.connected {
                return;
            }
            st.refreshing = true;
            st.status_text = format!("正在刷新 {}...", addr);
            unsafe { InvalidateRect(st.hwnd, ptr::null(), 0); }
        }
    });
}

pub fn update_flyout_status(ds: &DeviceStatus, addr: &str, takeover_active: bool) {
    FLYOUT_STATE.with(|c| {
        let mut borrow = c.borrow_mut();
        if let Some(st) = borrow.as_mut() {
            st.refreshing = false;
            st.connected = ds.connected;
            st.active_source = ds.active_source;
            st.status_text = if !ds.connected {
                format!("未连接 {}", addr)
            } else if takeover_active {
                format!("键鼠接管中 · 当前：{}", ds.active_source.label())
            } else {
                format!("已连接 {} · 当前：{}", addr, ds.active_source.label())
            };
            if let Some(b) = ds.backlight {
                if !st.dragging_bl {
                    st.backlight = Some(b);
                }
            }
            if let Some(v) = ds.volume {
                if !st.dragging_vol {
                    st.volume = Some(v);
                }
            }
            unsafe { InvalidateRect(st.hwnd, ptr::null(), 0); }
        }
    });
}

pub fn set_flyout_optimistic_source(src: SignalSource, addr: &str, takeover_active: bool) {
    FLYOUT_STATE.with(|c| {
        let mut borrow = c.borrow_mut();
        if let Some(st) = borrow.as_mut() {
            st.connected = true;
            st.refreshing = false;
            st.active_source = src;
            st.status_text = if takeover_active {
                format!("键鼠接管中 · 当前：{}", src.label())
            } else {
                format!("已连接 {} · 当前：{}", addr, src.label())
            };
            unsafe { InvalidateRect(st.hwnd, ptr::null(), 0); }
        }
    });
}

fn emit_action(act: FlyoutAction) {
    let cb_ptr: *const dyn Fn(FlyoutAction) = FLYOUT_STATE.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|s| s.on_action.as_deref().map(|f| f as *const _))
            .unwrap_or(ptr::null::<fn(FlyoutAction)>() as *const _)
    });
    if !cb_ptr.is_null() {
        unsafe { (*cb_ptr)(act); }
    }
}

unsafe fn paint_flyout(st: &FlyoutState, hdc: windows_sys::Win32::Graphics::Gdi::HDC) {
    let l = &st.layout;
    let s = l.s;
    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, l.w, l.h);
    let old_bmp = SelectObject(mem_dc, mem_bmp);

    let mut g: *mut windows_sys::Win32::Graphics::GdiPlus::GpGraphics = ptr::null_mut();
    GdipCreateFromHDC(mem_dc, &mut g);
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    GdipSetTextRenderingHint(g, TextRenderingHintClearTypeGridFit);

    // Background + Footer
    fill_rect(g, 0.0, 0.0, l.w as f32, l.h as f32, COL_SURFACE_BG);
    fill_rect(g, 0.0, l.footer_y, l.w as f32, l.h as f32 - l.footer_y, COL_FOOTER_BG);
    draw_line(g, 0.0, l.footer_y, l.w as f32, l.footer_y, COL_DIVIDER, 1.0);
    draw_rect_border(g, 0.0, 0.0, (l.w - 1) as f32, (l.h - 1) as f32, COL_CARD_BORDER, 1.0);

    // Status row
    let dot_col = if st.refreshing {
        COL_STATUS_AMBER
    } else if st.connected {
        COL_STATUS_GREEN
    } else {
        COL_STATUS_RED
    };
    let dot_d = 8.5 * s;
    fill_ellipse(g, l.pad, 26.0 * s - dot_d / 2.0, dot_d, dot_d, dot_col);
    draw_text(
        g,
        &st.status_text,
        l.pad + dot_d + 8.0 * s,
        10.0 * s,
        l.w as f32 - l.pad * 2.0 - dot_d - 8.0 * s,
        32.0 * s,
        12.5 * s,
        false,
        COL_TEXT_PRIMARY,
        StringAlignmentNear,
        StringAlignmentCenter,
    );
    draw_line(g, l.pad, 48.0 * s, l.w as f32 - l.pad, 48.0 * s, COL_DIVIDER, 1.0);

    // Section labels
    draw_text(g, "信号源", l.pad, 56.0 * s, 100.0 * s, 20.0 * s, 12.0 * s, false, COL_TEXT_SECONDARY, StringAlignmentNear, StringAlignmentCenter);
    draw_text(g, "背光", l.pad, 256.0 * s, 100.0 * s, 20.0 * s, 12.0 * s, false, COL_TEXT_SECONDARY, StringAlignmentNear, StringAlignmentCenter);
    draw_text(g, "音量", l.pad, 310.0 * s, 100.0 * s, 20.0 * s, 12.0 * s, false, COL_TEXT_SECONDARY, StringAlignmentNear, StringAlignmentCenter);

    // 2x2 Source tiles
    for (src, label, hit, card) in &l.tiles {
        let active = st.connected && st.active_source == *src;
        let hov = st.hover == HitPart::Tile(*src);
        let prs = st.pressed == HitPart::Tile(*src);
        let fill = if active {
            if hov { COL_ACCENT_HOVER } else { COL_ACCENT_BLUE }
        } else if prs {
            COL_CARD_PRESSED
        } else if hov {
            COL_CARD_HOVER
        } else {
            COL_CARD_BG
        };
        let border = if active { fill } else { COL_CARD_BORDER };
        let icon_col = if active { COL_WHITE } else { COL_TEXT_PRIMARY };

        fill_round_rect(g, card.x, card.y, card.w, card.h, 7.0 * s, fill);
        draw_round_rect(g, card.x, card.y, card.w, card.h, 7.0 * s, border, 1.0);

        let icx = card.x + card.w / 2.0;
        let icy = card.y + card.h / 2.0;
        if *src == SignalSource::TvSystem {
            draw_tv_icon(g, icx, icy, icon_col, s);
        } else {
            draw_monitor_icon(g, icx, icy, icon_col, s);
        }

        draw_text(
            g,
            label,
            hit.x,
            card.y + card.h + 4.0 * s,
            hit.w,
            22.0 * s,
            12.2 * s,
            active,
            if active { COL_ACCENT_BLUE } else { COL_TEXT_PRIMARY },
            StringAlignmentCenter,
            StringAlignmentCenter,
        );
    }

    // Backlight row
    draw_sun_icon(g, l.pad + 12.0 * s, l.bl_slider.y + l.bl_slider.h / 2.0, COL_TEXT_SECONDARY, s);
    paint_slider(
        g,
        &l.bl_slider,
        st.backlight.unwrap_or(50),
        st.hover == HitPart::SliderBacklight,
        st.dragging_bl,
        s,
    );
    let bl_txt = st.backlight.map(|v| v.to_string()).unwrap_or_else(|| "--".to_string());
    draw_text(
        g,
        &bl_txt,
        l.w as f32 - l.pad - 36.0 * s,
        l.bl_slider.y,
        36.0 * s,
        l.bl_slider.h,
        12.5 * s,
        false,
        COL_TEXT_SECONDARY,
        StringAlignmentFar,
        StringAlignmentCenter,
    );

    // Volume row
    draw_speaker_icon(g, l.pad + 12.0 * s, l.vol_slider.y + l.vol_slider.h / 2.0, COL_TEXT_SECONDARY, s);
    paint_slider(
        g,
        &l.vol_slider,
        st.volume.unwrap_or(15),
        st.hover == HitPart::SliderVolume,
        st.dragging_vol,
        s,
    );
    let vol_txt = st.volume.map(|v| v.to_string()).unwrap_or_else(|| "--".to_string());
    draw_text(
        g,
        &vol_txt,
        l.w as f32 - l.pad - 36.0 * s,
        l.vol_slider.y,
        36.0 * s,
        l.vol_slider.h,
        12.5 * s,
        false,
        COL_TEXT_SECONDARY,
        StringAlignmentFar,
        StringAlignmentCenter,
    );

    // Footer buttons
    paint_btn(g, &l.btn_settings, "ADB 设置...", st.hover == HitPart::BtnSettings, st.pressed == HitPart::BtnSettings, s);
    paint_btn(g, &l.btn_refresh, "刷新", st.hover == HitPart::BtnRefresh, st.pressed == HitPart::BtnRefresh, s);
    paint_btn(g, &l.btn_exit, "退出", st.hover == HitPart::BtnExit, st.pressed == HitPart::BtnExit, s);

    GdipDeleteGraphics(g);
    BitBlt(hdc, 0, 0, l.w, l.h, mem_dc, 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    DeleteObject(mem_bmp);
    DeleteDC(mem_dc);
}

unsafe fn paint_slider(
    g: *mut windows_sys::Win32::Graphics::GdiPlus::GpGraphics,
    r: &RectF4,
    value: i32,
    hovered: bool,
    dragging: bool,
    s: f32,
) {
    let pad = 11.0 * s;
    let track_h = 4.0 * s;
    let cy = r.y + r.h / 2.0;
    let track_w = (r.w - pad * 2.0).max(1.0);
    let ratio = (value.clamp(0, 100) as f32) / 100.0;
    let tx = r.x + pad + track_w * ratio;

    fill_round_rect(g, r.x + pad, cy - track_h / 2.0, track_w, track_h, track_h / 2.0, COL_TRACK_BG);
    if tx > r.x + pad + 1.0 {
        fill_round_rect(g, r.x + pad, cy - track_h / 2.0, tx - (r.x + pad), track_h, track_h / 2.0, COL_ACCENT_BLUE);
    }

    let outer_r = 9.0 * s;
    fill_ellipse(g, tx - outer_r, cy - outer_r, outer_r * 2.0, outer_r * 2.0, COL_WHITE);
    draw_ellipse(g, tx - outer_r, cy - outer_r, outer_r * 2.0, outer_r * 2.0, COL_THUMB_BORDER, 1.1);

    let inner_r = if dragging { 4.2 } else if hovered { 5.6 } else { 4.8 } * s;
    fill_ellipse(g, tx - inner_r, cy - inner_r, inner_r * 2.0, inner_r * 2.0, COL_ACCENT_BLUE);
}

unsafe fn paint_btn(
    g: *mut windows_sys::Win32::Graphics::GdiPlus::GpGraphics,
    r: &RectF4,
    text: &str,
    hovered: bool,
    pressed: bool,
    s: f32,
) {
    let fill = if pressed {
        COL_CARD_PRESSED
    } else if hovered {
        COL_CARD_HOVER
    } else {
        COL_WHITE
    };
    fill_round_rect(g, r.x, r.y, r.w, r.h, 5.0 * s, fill);
    draw_round_rect(g, r.x, r.y, r.w, r.h, 5.0 * s, COL_BTN_BORDER, 1.0);
    draw_text(g, text, r.x, r.y, r.w, r.h, 12.0 * s, false, COL_TEXT_PRIMARY, StringAlignmentCenter, StringAlignmentCenter);
}

unsafe extern "system" fn flyout_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            FLYOUT_STATE.with(|c| {
                if let Some(st) = c.borrow().as_ref() {
                    paint_flyout(st, hdc);
                }
            });
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_ACTIVATE => {
            let active = (wp & 0xFFFF) as u32;
            if active == WA_INACTIVE {
                let should_hide = FLYOUT_STATE.with(|c| {
                    c.borrow().as_ref().map(|s| !s.suppress_deactivate).unwrap_or(false)
                });
                if should_hide {
                    hide_flyout();
                }
            }
            0
        }
        WM_KEYDOWN => {
            if wp as u16 == VK_ESCAPE {
                hide_flyout();
            }
            0
        }
        WM_MOUSEMOVE => {
            let px = (lp as i16) as f32;
            let py = ((lp >> 16) as i16) as f32;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);

            let mut need_repaint = false;
            FLYOUT_STATE.with(|c| {
                if let Some(st) = c.borrow_mut().as_mut() {
                    if st.dragging_bl {
                        let v = st.layout.slider_value_from_x(&st.layout.bl_slider, px);
                        if st.backlight != Some(v) {
                            st.backlight = Some(v);
                            SetTimer(hwnd, TIMER_BL_DEBOUNCE, 160, None);
                            need_repaint = true;
                        }
                    } else if st.dragging_vol {
                        let v = st.layout.slider_value_from_x(&st.layout.vol_slider, px);
                        if st.volume != Some(v) {
                            st.volume = Some(v);
                            SetTimer(hwnd, TIMER_VOL_DEBOUNCE, 140, None);
                            need_repaint = true;
                        }
                    } else {
                        let hit = st.layout.hit_test(px, py);
                        if st.hover != hit {
                            st.hover = hit;
                            need_repaint = true;
                        }
                    }
                    let cursor = if st.hover != HitPart::None || st.dragging_bl || st.dragging_vol {
                        IDC_HAND
                    } else {
                        IDC_ARROW
                    };
                    SetCursor(LoadCursorW(ptr::null_mut(), cursor));
                }
            });
            if need_repaint {
                InvalidateRect(hwnd, ptr::null(), 0);
            }
            0
        }
        WM_MOUSELEAVE_MSG => {
            FLYOUT_STATE.with(|c| {
                if let Some(st) = c.borrow_mut().as_mut() {
                    if st.hover != HitPart::None {
                        st.hover = HitPart::None;
                        InvalidateRect(hwnd, ptr::null(), 0);
                    }
                }
            });
            0
        }
        WM_LBUTTONDOWN => {
            let px = (lp as i16) as f32;
            let py = ((lp >> 16) as i16) as f32;
            FLYOUT_STATE.with(|c| {
                if let Some(st) = c.borrow_mut().as_mut() {
                    let hit = st.layout.hit_test(px, py);
                    st.pressed = hit;
                    if hit == HitPart::SliderBacklight {
                        st.dragging_bl = true;
                        SetCapture(hwnd);
                        let v = st.layout.slider_value_from_x(&st.layout.bl_slider, px);
                        st.backlight = Some(v);
                        SetTimer(hwnd, TIMER_BL_DEBOUNCE, 160, None);
                    } else if hit == HitPart::SliderVolume {
                        st.dragging_vol = true;
                        SetCapture(hwnd);
                        let v = st.layout.slider_value_from_x(&st.layout.vol_slider, px);
                        st.volume = Some(v);
                        SetTimer(hwnd, TIMER_VOL_DEBOUNCE, 140, None);
                    }
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
            });
            0
        }
        WM_LBUTTONUP => {
            let px = (lp as i16) as f32;
            let py = ((lp >> 16) as i16) as f32;
            let mut action: Option<FlyoutAction> = None;
            FLYOUT_STATE.with(|c| {
                if let Some(st) = c.borrow_mut().as_mut() {
                    if st.dragging_bl {
                        st.dragging_bl = false;
                        ReleaseCapture();
                        KillTimer(hwnd, TIMER_BL_DEBOUNCE);
                        let v = st.layout.slider_value_from_x(&st.layout.bl_slider, px);
                        st.backlight = Some(v);
                        action = Some(FlyoutAction::SetBacklight(v));
                    } else if st.dragging_vol {
                        st.dragging_vol = false;
                        ReleaseCapture();
                        KillTimer(hwnd, TIMER_VOL_DEBOUNCE);
                        let v = st.layout.slider_value_from_x(&st.layout.vol_slider, px);
                        st.volume = Some(v);
                        action = Some(FlyoutAction::SetVolume(v));
                    } else {
                        let hit = st.layout.hit_test(px, py);
                        if hit == st.pressed {
                            action = match hit {
                                HitPart::Tile(src) => Some(FlyoutAction::SwitchSource(src)),
                                HitPart::BtnSettings => {
                                    hide_flyout();
                                    Some(FlyoutAction::OpenSettings)
                                }
                                HitPart::BtnRefresh => Some(FlyoutAction::Refresh),
                                HitPart::BtnExit => {
                                    hide_flyout();
                                    Some(FlyoutAction::Exit)
                                }
                                _ => None,
                            };
                        }
                    }
                    st.pressed = HitPart::None;
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
            });
            if let Some(a) = action {
                emit_action(a);
            }
            0
        }
        WM_TIMER => {
            let mut action: Option<FlyoutAction> = None;
            if wp == TIMER_BL_DEBOUNCE {
                KillTimer(hwnd, TIMER_BL_DEBOUNCE);
                FLYOUT_STATE.with(|c| {
                    if let Some(st) = c.borrow().as_ref() {
                        if let Some(v) = st.backlight {
                            action = Some(FlyoutAction::SetBacklight(v));
                        }
                    }
                });
            } else if wp == TIMER_VOL_DEBOUNCE {
                KillTimer(hwnd, TIMER_VOL_DEBOUNCE);
                FLYOUT_STATE.with(|c| {
                    if let Some(st) = c.borrow().as_ref() {
                        if let Some(v) = st.volume {
                            action = Some(FlyoutAction::SetVolume(v));
                        }
                    }
                });
            }
            if let Some(a) = action {
                emit_action(a);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
