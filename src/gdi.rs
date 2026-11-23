use crate::config::to_wide;
use std::ptr;
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

pub const COL_SURFACE_BG: u32 = 0xFFF5F6F9;
pub const COL_FOOTER_BG: u32 = 0xFFECEEF3;
pub const COL_CARD_BG: u32 = 0xFFFFFFFF;
pub const COL_CARD_HOVER: u32 = 0xFFF9FAFC;
pub const COL_CARD_PRESSED: u32 = 0xFFECEFF4;
pub const COL_CARD_BORDER: u32 = 0xFFE0E3E8;
pub const COL_BTN_BORDER: u32 = 0xFFD7DBE2;
pub const COL_ACCENT_BLUE: u32 = 0xFF0067C0;
pub const COL_ACCENT_HOVER: u32 = 0xFF005AA8;
pub const COL_TEXT_PRIMARY: u32 = 0xFF1A1E24;
pub const COL_TEXT_SECONDARY: u32 = 0xFF5F6672;
pub const COL_DIVIDER: u32 = 0xFFE4E7EC;
pub const COL_TRACK_BG: u32 = 0xFFC4C8CF;
pub const COL_THUMB_BORDER: u32 = 0xFFCCD0D6;
pub const COL_STATUS_GREEN: u32 = 0xFF107C10;
pub const COL_STATUS_AMBER: u32 = 0xFFCA8200;
pub const COL_STATUS_RED: u32 = 0xFFC42B1C;
pub const COL_WHITE: u32 = 0xFFFFFFFF;

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

pub unsafe fn draw_rect_border(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, argb: u32, width: f32) {
    let mut pen: *mut GpPen = ptr::null_mut();
    GdipCreatePen1(argb, width, UnitPixel, &mut pen);
    GdipDrawRectangle(g, pen, x, y, w, h);
    GdipDeletePen(pen);
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

pub fn create_tray_icon(takeover_active: bool) -> HICON {
    unsafe {
        let mut bmp: *mut GpBitmap = ptr::null_mut();
        const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x0026_200A;
        GdipCreateBitmapFromScan0(32, 32, 0, PIXEL_FORMAT_32BPP_ARGB, ptr::null_mut(), &mut bmp);
        let mut g: *mut GpGraphics = ptr::null_mut();
        GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipGraphicsClear(g, 0x00000000);

        let stroke = if takeover_active { 0xFF00825F } else { 0xFF1E242C };
        let fill = if takeover_active { 0xFF00A878 } else { 0xFFF5F8FC };
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
