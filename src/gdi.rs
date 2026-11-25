use crate::config::to_wide;
use std::ffi::c_void;
use std::ptr;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_BORDER_COLOR_ATTR: u32 = 34;

#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub dark: bool,
    pub surface_bg: u32,
    pub footer_bg: u32,
    pub card_bg: u32,
    pub card_hover: u32,
    pub card_pressed: u32,
    pub card_border: u32,
    pub btn_bg: u32,
    pub btn_hover: u32,
    pub btn_pressed: u32,
    pub btn_border: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_text: u32,
    pub active_icon: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub divider: u32,
    pub track_bg: u32,
    pub thumb_outer: u32,
    pub thumb_border: u32,
    pub status_green: u32,
    pub status_amber: u32,
    pub status_red: u32,
    pub reconnect_hover: u32,
    pub reconnect_pressed: u32,
    /// COLORREF (0x00BBGGRR) for DWMWA_BORDER_COLOR
    pub dwm_border_bgr: u32,
    /// COLORREF (0x00BBGGRR) for native Win32 dialog background & text
    pub win32_bg_bgr: u32,
    pub win32_edit_bg_bgr: u32,
    pub win32_text_bgr: u32,
}

impl ThemePalette {
    pub const fn light() -> Self {
        Self {
            dark: false,
            surface_bg: 0xFFF6F7FA,
            footer_bg: 0xFFECEEF3,
            card_bg: 0xFFFFFFFF,
            card_hover: 0xFFF8F9FC,
            card_pressed: 0xFFECEFF4,
            card_border: 0xFFE1E4EA,
            btn_bg: 0xFFFFFFFF,
            btn_hover: 0xFFF9FAFC,
            btn_pressed: 0xFFECEFF4,
            btn_border: 0xFFD7DBE2,
            accent: 0xFF0067C0,
            accent_hover: 0xFF005AA8,
            accent_text: 0xFF0067C0,
            active_icon: 0xFFFFFFFF,
            text_primary: 0xFF1A1E24,
            text_secondary: 0xFF5F6672,
            divider: 0xFFE5E8EE,
            track_bg: 0xFFC4C8CF,
            thumb_outer: 0xFFFFFFFF,
            thumb_border: 0xFFCCD0D6,
            status_green: 0xFF107C10,
            status_amber: 0xFFCA8200,
            status_red: 0xFFC42B1C,
            reconnect_hover: 0xFFE8ECF2,
            reconnect_pressed: 0xFFDDE3EC,
            dwm_border_bgr: 0x00E0D8D4,
            win32_bg_bgr: 0x00FAF7F6,
            win32_edit_bg_bgr: 0x00FFFFFF,
            win32_text_bgr: 0x00241E1A,
        }
    }

    pub const fn dark() -> Self {
        Self {
            dark: true,
            surface_bg: 0xFF202225,
            footer_bg: 0xFF191B1E,
            card_bg: 0xFF2B2E33,
            card_hover: 0xFF34383F,
            card_pressed: 0xFF25282D,
            card_border: 0xFF3A3E46,
            btn_bg: 0xFF2B2E33,
            btn_hover: 0xFF353940,
            btn_pressed: 0xFF24272C,
            btn_border: 0xFF3E434C,
            accent: 0xFF4CC2FF,
            accent_hover: 0xFF66CCFF,
            accent_text: 0xFF60CDFF,
            active_icon: 0xFF11161C,
            text_primary: 0xFFF2F4F8,
            text_secondary: 0xFFA5ADB9,
            divider: 0xFF31353C,
            track_bg: 0xFF525862,
            thumb_outer: 0xFF383C44,
            thumb_border: 0xFF5C626E,
            status_green: 0xFF6CCB5F,
            status_amber: 0xFFFCE100,
            status_red: 0xFFFF99A4,
            reconnect_hover: 0xFF2E3239,
            reconnect_pressed: 0xFF252930,
            dwm_border_bgr: 0x004E4642,
            win32_bg_bgr: 0x00252220,
            win32_edit_bg_bgr: 0x00332E2B,
            win32_text_bgr: 0x00F8F4F2,
        }
    }

    pub fn current() -> Self {
        if crate::config::is_apps_dark_mode() {
            Self::dark()
        } else {
            Self::light()
        }
    }
}

pub unsafe fn apply_dwm_dark_titlebar(hwnd: HWND, dark: bool) {
    let val: i32 = if dark { 1 } else { 0 };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &val as *const i32 as *const _,
        std::mem::size_of::<i32>() as u32,
    );
}

pub unsafe fn apply_dwm_border_color(hwnd: HWND, bgr: u32) {
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_BORDER_COLOR_ATTR,
        &bgr as *const u32 as *const _,
        std::mem::size_of::<u32>() as u32,
    );
}

type FnSetWindowTheme = unsafe extern "system" fn(HWND, *const u16, *const u16) -> i32;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *const c_void;
}

pub unsafe fn apply_dark_control_theme(ctrl: HWND, dark: bool) {
    static FN_PTR: std::sync::OnceLock<Option<FnSetWindowTheme>> = std::sync::OnceLock::new();
    let f = *FN_PTR.get_or_init(|| {
        let dll = to_wide("uxtheme.dll");
        let hmod = LoadLibraryW(dll.as_ptr());
        if hmod.is_null() {
            return None;
        }
        let proc = GetProcAddress(hmod, b"SetWindowTheme\0".as_ptr());
        if proc.is_null() {
            return None;
        }
        Some(std::mem::transmute::<*const c_void, FnSetWindowTheme>(proc))
    });
    if let Some(set_theme) = f {
        if dark {
            let dark_name = to_wide("DarkMode_Explorer");
            let _ = set_theme(ctrl, dark_name.as_ptr(), ptr::null());
        } else {
            let light_name = to_wide("Explorer");
            let _ = set_theme(ctrl, light_name.as_ptr(), ptr::null());
        }
    }
}

pub struct GdipGuard {
    token: usize,
}

impl GdipGuard {
    pub fn start() -> Self {
        let mut token: usize = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        unsafe {
            GdiplusStartup(&mut token, &input, ptr::null_mut());
        }
        Self { token }
    }
}

impl Drop for GdipGuard {
    fn drop(&mut self) {
        if self.token != 0 {
            unsafe {
                GdiplusShutdown(self.token);
            }
        }
    }
}

unsafe fn make_round_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> *mut GpPath {
    let mut path: *mut GpPath = ptr::null_mut();
    GdipCreatePath(FillModeAlternate, &mut path);
    let d = (radius * 2.0).min(w).min(h);
    GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    GdipClosePathFigure(path);
    path
}

pub unsafe fn fill_rect(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, argb: u32) {
    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(argb, &mut br);
    GdipFillRectangle(g, br as *mut GpBrush, x, y, w, h);
    GdipDeleteBrush(br as *mut GpBrush);
}

pub unsafe fn fill_round_rect(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, r: f32, argb: u32) {
    let path = make_round_rect_path(x, y, w, h, r);
    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(argb, &mut br);
    GdipFillPath(g, br as *mut GpBrush, path);
    GdipDeleteBrush(br as *mut GpBrush);
    GdipDeletePath(path);
}

pub unsafe fn draw_round_rect(
    g: *mut GpGraphics,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    argb: u32,
    pen_w: f32,
) {
    let path = make_round_rect_path(x, y, w, h, r);
    let mut pen: *mut GpPen = ptr::null_mut();
    GdipCreatePen1(argb, pen_w, UnitPixel, &mut pen);
    GdipSetPenLineJoin(pen, LineJoinRound);
    GdipDrawPath(g, pen, path);
    GdipDeletePen(pen);
    GdipDeletePath(path);
}

pub unsafe fn draw_line(g: *mut GpGraphics, x1: f32, y1: f32, x2: f32, y2: f32, argb: u32, pen_w: f32) {
    let mut pen: *mut GpPen = ptr::null_mut();
    GdipCreatePen1(argb, pen_w, UnitPixel, &mut pen);
    GdipSetPenStartCap(pen, LineCapRound);
    GdipSetPenEndCap(pen, LineCapRound);
    GdipDrawLine(g, pen, x1, y1, x2, y2);
    GdipDeletePen(pen);
}

pub unsafe fn fill_ellipse(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, argb: u32) {
    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(argb, &mut br);
    GdipFillEllipse(g, br as *mut GpBrush, x, y, w, h);
    GdipDeleteBrush(br as *mut GpBrush);
}

pub unsafe fn draw_ellipse(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, argb: u32, pen_w: f32) {
    let mut pen: *mut GpPen = ptr::null_mut();
    GdipCreatePen1(argb, pen_w, UnitPixel, &mut pen);
    GdipDrawEllipse(g, pen, x, y, w, h);
    GdipDeletePen(pen);
}

pub unsafe fn draw_text(
    g: *mut GpGraphics,
    text: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    size_px: f32,
    bold: bool,
    argb: u32,
    align: StringAlignment,
    valign: StringAlignment,
) {
    let fam_w = to_wide("Microsoft YaHei UI");
    let mut family: *mut GpFontFamily = ptr::null_mut();
    GdipCreateFontFamilyFromName(fam_w.as_ptr(), ptr::null_mut(), &mut family);
    if family.is_null() {
        let fb = to_wide("Segoe UI");
        GdipCreateFontFamilyFromName(fb.as_ptr(), ptr::null_mut(), &mut family);
    }
    let mut font: *mut GpFont = ptr::null_mut();
    let style = if bold { FontStyleBold } else { FontStyleRegular };
    GdipCreateFont(family, size_px, style, UnitPixel, &mut font);

    let mut fmt: *mut GpStringFormat = ptr::null_mut();
    GdipCreateStringFormat(StringFormatFlagsNoWrap, 0, &mut fmt);
    GdipSetStringFormatAlign(fmt, align);
    GdipSetStringFormatLineAlign(fmt, valign);
    GdipSetStringFormatTrimming(fmt, StringTrimmingEllipsisCharacter);

    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(argb, &mut br);

    let txt_w = to_wide(text);
    let rect = RectF { X: x, Y: y, Width: w, Height: h };
    GdipDrawString(
        g,
        txt_w.as_ptr(),
        (txt_w.len() - 1) as i32,
        font,
        &rect,
        fmt,
        br as *mut GpBrush,
    );

    GdipDeleteBrush(br as *mut GpBrush);
    GdipDeleteStringFormat(fmt);
    GdipDeleteFont(font);
    GdipDeleteFontFamily(family);
}

pub unsafe fn draw_tv_icon(g: *mut GpGraphics, cx: f32, cy: f32, argb: u32, s: f32) {
    let w = 22.0 * s;
    let h = 15.0 * s;
    let x = cx - w / 2.0;
    let y = cy - h / 2.0 - 1.5 * s;
    let pw = 1.65 * s;
    draw_round_rect(g, x, y, w, h, 2.2 * s, argb, pw);
    draw_line(g, cx - 5.5 * s, y + h + 3.2 * s, cx - 3.5 * s, y + h, argb, pw);
    draw_line(g, cx + 5.5 * s, y + h + 3.2 * s, cx + 3.5 * s, y + h, argb, pw);

    let py = y + h / 2.0;
    let pts = [
        PointF { X: cx - 2.3 * s, Y: py - 3.1 * s },
        PointF { X: cx - 2.3 * s, Y: py + 3.1 * s },
        PointF { X: cx + 3.2 * s, Y: py },
    ];
    let mut br: *mut GpSolidFill = ptr::null_mut();
    GdipCreateSolidFill(argb, &mut br);
    GdipFillPolygon(g, br as *mut GpBrush, pts.as_ptr(), 3, FillModeAlternate);
    GdipDeleteBrush(br as *mut GpBrush);
}

pub unsafe fn draw_monitor_icon(g: *mut GpGraphics, cx: f32, cy: f32, argb: u32, s: f32) {
    let w = 22.0 * s;
    let h = 14.5 * s;
    let x = cx - w / 2.0;
    let y = cy - h / 2.0 - 2.0 * s;
    let pw = 1.65 * s;
    draw_round_rect(g, x, y, w, h, 2.0 * s, argb, pw);
    draw_line(g, cx, y + h, cx, y + h + 3.8 * s, argb, pw);
    draw_line(g, cx - 5.5 * s, y + h + 3.8 * s, cx + 5.5 * s, y + h + 3.8 * s, argb, pw);
}

pub unsafe fn draw_sun_icon(g: *mut GpGraphics, cx: f32, cy: f32, argb: u32, s: f32) {
    let r = 3.8 * s;
    let pw = 1.45 * s;
    draw_ellipse(g, cx - r, cy - r, r * 2.0, r * 2.0, argb, pw);
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::FRAC_PI_4;
        let r1 = 6.2 * s;
        let r2 = 8.2 * s;
        draw_line(g, cx + a.cos() * r1, cy + a.sin() * r1, cx + a.cos() * r2, cy + a.sin() * r2, argb, pw);
    }
}

pub unsafe fn draw_speaker_icon(g: *mut GpGraphics, cx: f32, cy: f32, argb: u32, s: f32) {
    let pw = 1.45 * s;
    let pts = [
        PointF { X: cx - 7.5 * s, Y: cy - 2.6 * s },
        PointF { X: cx - 4.2 * s, Y: cy - 2.6 * s },
        PointF { X: cx - 0.5 * s, Y: cy - 6.0 * s },
        PointF { X: cx - 0.5 * s, Y: cy + 6.0 * s },
        PointF { X: cx - 4.2 * s, Y: cy + 2.6 * s },
        PointF { X: cx - 7.5 * s, Y: cy + 2.6 * s },
    ];
    let mut pen: *mut GpPen = ptr::null_mut();
    GdipCreatePen1(argb, pw, UnitPixel, &mut pen);
    GdipSetPenLineJoin(pen, LineJoinRound);
    GdipSetPenStartCap(pen, LineCapRound);
    GdipSetPenEndCap(pen, LineCapRound);
    GdipDrawPolygon(g, pen, pts.as_ptr(), pts.len() as i32);
    GdipDrawArc(g, pen, cx - 1.5 * s, cy - 3.6 * s, 6.2 * s, 7.2 * s, -50.0, 100.0);
    GdipDrawArc(g, pen, cx - 1.5 * s, cy - 6.2 * s, 10.2 * s, 12.4 * s, -50.0, 100.0);
    GdipDeletePen(pen);
}

pub fn create_tray_icon(takeover_active: bool, dark_taskbar: bool) -> HICON {
    unsafe {
        let mut bmp: *mut GpBitmap = ptr::null_mut();
        const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x0026_200A;
        GdipCreateBitmapFromScan0(32, 32, 0, PIXEL_FORMAT_32BPP_ARGB, ptr::null_mut(), &mut bmp);
        let mut g: *mut GpGraphics = ptr::null_mut();
        GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipGraphicsClear(g, 0x00000000);

        let (stroke, fill) = if takeover_active {
            if dark_taskbar {
                (0xFF4BE2A8, 0xFF0E7A55)
            } else {
                (0xFF00825F, 0xFF00A878)
            }
        } else if dark_taskbar {
            (0xFFF0F4FA, 0x33FFFFFF)
        } else {
            (0xFF1E242C, 0xFFF5F8FC)
        };
        fill_round_rect(g, 4.0, 5.0, 24.0, 16.0, 2.5, fill);
        draw_round_rect(g, 4.0, 5.0, 24.0, 16.0, 2.5, stroke, 2.5);
        draw_line(g, 16.0, 21.0, 16.0, 27.0, stroke, 2.5);
        draw_line(g, 10.0, 27.0, 22.0, 27.0, stroke, 2.5);

        let mut hicon: HICON = ptr::null_mut();
        GdipCreateHICONFromBitmap(bmp, &mut hicon);
        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as *mut GpImage);
        hicon
    }
}
