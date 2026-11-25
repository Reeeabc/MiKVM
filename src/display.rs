use crate::config::from_wide;
use std::ptr;
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS, SDC_ALLOW_CHANGES, SDC_APPLY,
    SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_INTERNAL,
    SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetDC, GetDeviceCaps, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, ReleaseDC,
    HMONITOR, LOGPIXELSX, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Power::{
    RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    POWERBROADCAST_SETTING,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetCursorPos, GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible,
    SystemParametersInfoW, DEVICE_NOTIFY_WINDOW_HANDLE, GWL_EXSTYLE, PBT_POWERSETTINGCHANGE,
    SPI_GETWORKAREA, WM_POWERBROADCAST, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

// GUID_LIDSWITCH_STATE_CHANGE: {BA3E0F4D-B817-4094-A2D1-D56379E6A0F3}
const GUID_LIDSWITCH_STATE_CHANGE: GUID = GUID {
    data1: 0xBA3E0F4D,
    data2: 0xB817,
    data3: 0x4094,
    data4: [0xA2, 0xD1, 0xD5, 0x63, 0x79, 0xE6, 0xA0, 0xF3],
};

// Xiaomi / Redmi monitor EDID manufacturer ID ("XMI" in big-endian EDID)
const EDID_MFG_XIAOMI: u16 = 0xA961;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl EdgeSide {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeSide::Left => "Left",
            EdgeSide::Right => "Right",
            EdgeSide::Top => "Top",
            EdgeSide::Bottom => "Bottom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Some(EdgeSide::Left),
            "right" => Some(EdgeSide::Right),
            "top" | "up" => Some(EdgeSide::Top),
            "bottom" | "down" => Some(EdgeSide::Bottom),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct MonitorDesc {
    pub is_internal: bool,
    pub rect: RECT,
    pub work_area: RECT,
    pub dpi_scale: f32,
}

#[derive(Clone)]
pub struct DisplayTopology {
    pub visible_pc_monitors: Vec<MonitorDesc>,
    pub tv_ghost_monitor: Option<MonitorDesc>,
    pub edge_side: Option<EdgeSide>,
    pub edge_trigger_rect: Option<RECT>,
    pub edge_trigger_is_ghost: bool,
    pub seam_coord: i32,
    pub return_cursor_pt: POINT,
    pub pc_fence_rect: Option<RECT>,
}

impl DisplayTopology {
    pub fn has_visible_pc(&self) -> bool {
        !self.visible_pc_monitors.is_empty()
    }
}

fn is_internal_tech(tech: i32) -> bool {
    let u = tech as u32;
    // DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL = 0x80000000
    // DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS = 6
    // DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED = 11
    // DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED = 13
    (u & 0x8000_0000) != 0 || u == 6 || u == 11 || u == 13
}

fn monitor_work_and_dpi(rect: RECT) -> (RECT, f32) {
    unsafe {
        let center = POINT {
            x: rect.left + (rect.right - rect.left) / 2,
            y: rect.top + (rect.bottom - rect.top) / 2,
        };
        let hmon = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
        let mut wa = rect;
        let mut scale = 1.0f32;
        if !hmon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut mi) != 0 {
                wa = mi.rcWork;
            }
            scale = dpi_scale_for_monitor(hmon);
        }
        (wa, scale)
    }
}

pub fn dpi_scale_for_monitor(hmon: HMONITOR) -> f32 {
    unsafe {
        if !hmon.is_null() {
            let mut dpix: u32 = 96;
            let mut dpiy: u32 = 96;
            if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy) == 0 && dpix >= 96 {
                return (dpix as f32 / 96.0).max(1.0);
            }
        }
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

struct RawTarget {
    source_key: (u32, i32, u32),
    friendly_name: String,
    edid_mfg: u16,
    is_internal: bool,
    rect: RECT,
}

fn query_active_targets() -> Vec<RawTarget> {
    unsafe {
        let mut path_count: u32 = 0;
        let mut mode_count: u32 = 0;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) != 0
            || path_count == 0
        {
            return Vec::new();
        }
        let mut paths = vec![std::mem::zeroed::<DISPLAYCONFIG_PATH_INFO>(); path_count as usize];
        let mut modes = vec![std::mem::zeroed::<DISPLAYCONFIG_MODE_INFO>(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            ptr::null_mut(),
        ) != 0
        {
            return Vec::new();
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);

        let mut out = Vec::with_capacity(paths.len());
        for p in &paths {
            let mut tgt_name: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
            tgt_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt_name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt_name.header.adapterId = p.targetInfo.adapterId;
            tgt_name.header.id = p.targetInfo.id;
            let _ = DisplayConfigGetDeviceInfo(&mut tgt_name.header);
            let friendly_name = from_wide(&tgt_name.monitorFriendlyDeviceName);

            let mode_idx = p.sourceInfo.Anonymous.modeInfoIdx as usize;
            let mut rect = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
            if mode_idx < modes.len() && modes[mode_idx].infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                let sm = modes[mode_idx].Anonymous.sourceMode;
                rect = RECT {
                    left: sm.position.x,
                    top: sm.position.y,
                    right: sm.position.x + sm.width as i32,
                    bottom: sm.position.y + sm.height as i32,
                };
            }

            let source_key = (
                p.sourceInfo.adapterId.LowPart,
                p.sourceInfo.adapterId.HighPart,
                p.sourceInfo.id,
            );
            out.push(RawTarget {
                source_key,
                friendly_name,
                edid_mfg: tgt_name.edidManufactureId,
                is_internal: is_internal_tech(p.targetInfo.outputTechnology),
                rect,
            });
        }
        out
    }
}

fn target_matches_tv(t: &RawTarget, hint_lower: &str) -> bool {
    let fname_lower = t.friendly_name.to_ascii_lowercase();
    if !hint_lower.is_empty() && !fname_lower.is_empty() && fname_lower.contains(hint_lower) {
        return true;
    }
    if t.edid_mfg == EDID_MFG_XIAOMI {
        return true;
    }
    if fname_lower.contains("mi monitor")
        || fname_lower.contains("redmi")
        || fname_lower.contains("xiaomi")
        || fname_lower.contains("mitv")
    {
        return true;
    }
    false
}

pub fn query_display_topology(
    tv_hint: &str,
    lid_open: bool,
    fallback_side: EdgeSide,
) -> DisplayTopology {
    let raw = query_active_targets();
    let hint_lower = tv_hint.trim().to_ascii_lowercase();

    // Determine which targets belong to the TV monitor
    let mut is_tv_flags: Vec<bool> = raw.iter().map(|t| target_matches_tv(t, &hint_lower)).collect();
    if !is_tv_flags.iter().any(|&b| b) {
        // Fallback: if no explicit match and there is an external monitor, mark the first external monitor as TV
        let ext_indices: Vec<usize> = raw
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_internal)
            .map(|(i, _)| i)
            .collect();
        if ext_indices.len() == 1 {
            is_tv_flags[ext_indices[0]] = true;
        }
    }

    // Group targets by unique desktop surface (source_key) so Duplicate/Clone mode is handled accurately
    let mut source_keys: Vec<(u32, i32, u32)> = Vec::new();
    for t in &raw {
        if !source_keys.contains(&t.source_key) {
            source_keys.push(t.source_key);
        }
    }

    let mut visible_pc_monitors: Vec<MonitorDesc> = Vec::new();
    let mut tv_ghost_monitor: Option<MonitorDesc> = None;

    for sk in source_keys {
        let members: Vec<(usize, &RawTarget)> = raw
            .iter()
            .enumerate()
            .filter(|(_, t)| t.source_key == sk)
            .collect();
        if members.is_empty() {
            continue;
        }

        let has_visible_non_tv = members.iter().any(|(idx, t)| {
            !is_tv_flags[*idx] && !(t.is_internal && !lid_open)
        });
        let has_tv = members.iter().any(|(idx, _)| is_tv_flags[*idx]);

        if has_visible_non_tv {
            // Pick the visible non-TV target as representative
            let (_, rep) = members
                .iter()
                .find(|(idx, t)| !is_tv_flags[*idx] && !(t.is_internal && !lid_open))
                .copied()
                .unwrap_or(members[0]);
            let (work_area, dpi_scale) = monitor_work_and_dpi(rep.rect);
            visible_pc_monitors.push(MonitorDesc {
                is_internal: rep.is_internal,
                rect: rep.rect,
                work_area,
                dpi_scale,
            });
        } else if has_tv {
            let (_, rep) = members
                .iter()
                .find(|(idx, _)| is_tv_flags[*idx])
                .copied()
                .unwrap_or(members[0]);
            let (work_area, dpi_scale) = monitor_work_and_dpi(rep.rect);
            tv_ghost_monitor = Some(MonitorDesc {
                is_internal: false,
                rect: rep.rect,
                work_area,
                dpi_scale,
            });
        }
    }

    // Compute edge trigger geometry & safe return cursor position
    let mut edge_side = None;
    let mut edge_trigger_rect = None;
    let mut edge_trigger_is_ghost = false;
    let mut seam_coord = 0;
    let mut return_cursor_pt = POINT { x: 100, y: 100 };
    let mut pc_fence_rect = None;

    if !visible_pc_monitors.is_empty() {
        if let Some(ref tv) = tv_ghost_monitor {
            // Find the visible PC monitor adjacent to the TV ghost screen
            let mut best: Option<(&MonitorDesc, EdgeSide, i32)> = None;
            for pc in &visible_pc_monitors {
                let v_ov = (pc.rect.bottom.min(tv.rect.bottom) - pc.rect.top.max(tv.rect.top)).max(0);
                let h_ov = (pc.rect.right.min(tv.rect.right) - pc.rect.left.max(tv.rect.left)).max(0);
                if (tv.rect.right - pc.rect.left).abs() <= 4 && v_ov > 0 {
                    if best.map(|b| v_ov > b.2).unwrap_or(true) {
                        best = Some((pc, EdgeSide::Left, v_ov));
                    }
                } else if (pc.rect.right - tv.rect.left).abs() <= 4 && v_ov > 0 {
                    if best.map(|b| v_ov > b.2).unwrap_or(true) {
                        best = Some((pc, EdgeSide::Right, v_ov));
                    }
                } else if (tv.rect.bottom - pc.rect.top).abs() <= 4 && h_ov > 0 {
                    if best.map(|b| h_ov > b.2).unwrap_or(true) {
                        best = Some((pc, EdgeSide::Top, h_ov));
                    }
                } else if (pc.rect.bottom - tv.rect.top).abs() <= 4 && h_ov > 0 {
                    if best.map(|b| h_ov > b.2).unwrap_or(true) {
                        best = Some((pc, EdgeSide::Bottom, h_ov));
                    }
                }
            }

            let (adj_pc, side) = if let Some((pc, s, _)) = best {
                (pc, s)
            } else {
                // Fallback relative center comparison
                let pc = &visible_pc_monitors[0];
                let pc_cx = (pc.rect.left + pc.rect.right) / 2;
                let tv_cx = (tv.rect.left + tv.rect.right) / 2;
                let s = if tv_cx < pc_cx { EdgeSide::Left } else { EdgeSide::Right };
                (pc, s)
            };

            edge_side = Some(side);
            // Cover the ENTIRE invisible ghost TV monitor so even ultra-fast mouse flicks can't jump over it!
            edge_trigger_rect = Some(tv.rect);
            edge_trigger_is_ghost = true;
            pc_fence_rect = Some(adj_pc.rect);

            let (seam, ret_pt) = match side {
                EdgeSide::Left => (
                    adj_pc.rect.left,
                    POINT {
                        x: adj_pc.rect.left + 12,
                        y: (adj_pc.rect.top + adj_pc.rect.bottom) / 2,
                    },
                ),
                EdgeSide::Right => (
                    adj_pc.rect.right - 1,
                    POINT {
                        x: adj_pc.rect.right - 13,
                        y: (adj_pc.rect.top + adj_pc.rect.bottom) / 2,
                    },
                ),
                EdgeSide::Top => (
                    adj_pc.rect.top,
                    POINT {
                        x: (adj_pc.rect.left + adj_pc.rect.right) / 2,
                        y: adj_pc.rect.top + 12,
                    },
                ),
                EdgeSide::Bottom => (
                    adj_pc.rect.bottom - 1,
                    POINT {
                        x: (adj_pc.rect.left + adj_pc.rect.right) / 2,
                        y: adj_pc.rect.bottom - 13,
                    },
                ),
            };
            seam_coord = seam;
            return_cursor_pt = ret_pt;
        } else {
            // PC screen only / Duplicate mode: no ghost screen exists, place a 2px strip on the outer border
            let pc = &visible_pc_monitors[0];
            edge_side = Some(fallback_side);
            edge_trigger_is_ghost = false;
            pc_fence_rect = None; // No ghost monitor to fall into!

            let (trig, seam, ret_pt) = match fallback_side {
                EdgeSide::Left => (
                    RECT { left: pc.rect.left, top: pc.rect.top, right: pc.rect.left + 2, bottom: pc.rect.bottom },
                    pc.rect.left,
                    POINT { x: pc.rect.left + 12, y: (pc.rect.top + pc.rect.bottom) / 2 },
                ),
                EdgeSide::Right => (
                    RECT { left: pc.rect.right - 2, top: pc.rect.top, right: pc.rect.right, bottom: pc.rect.bottom },
                    pc.rect.right - 1,
                    POINT { x: pc.rect.right - 13, y: (pc.rect.top + pc.rect.bottom) / 2 },
                ),
                EdgeSide::Top => (
                    RECT { left: pc.rect.left, top: pc.rect.top, right: pc.rect.right, bottom: pc.rect.top + 2 },
                    pc.rect.top,
                    POINT { x: (pc.rect.left + pc.rect.right) / 2, y: pc.rect.top + 12 },
                ),
                EdgeSide::Bottom => (
                    RECT { left: pc.rect.left, top: pc.rect.bottom - 2, right: pc.rect.right, bottom: pc.rect.bottom },
                    pc.rect.bottom - 1,
                    POINT { x: (pc.rect.left + pc.rect.right) / 2, y: pc.rect.bottom - 13 },
                ),
            };
            edge_trigger_rect = Some(trig);
            seam_coord = seam;
            return_cursor_pt = ret_pt;
        }
    }

    DisplayTopology {
        visible_pc_monitors,
        tv_ghost_monitor,
        edge_side,
        edge_trigger_rect,
        edge_trigger_is_ghost,
        seam_coord,
        return_cursor_pt,
        pc_fence_rect,
    }
}

pub fn rect_contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Returns `(work_area, dpi_scale)` for the best visible PC monitor near the cursor.
/// When `avoid_tv_ghost` is true, ensures popups/flyouts NEVER land on the invisible 27U ghost primary monitor!
pub fn visible_work_area_for_cursor(
    avoid_tv_ghost: bool,
    tv_hint: &str,
    lid_open: bool,
) -> (RECT, f32) {
    unsafe {
        let mut pt: POINT = std::mem::zeroed();
        GetCursorPos(&mut pt);

        if avoid_tv_ghost {
            let topo = query_display_topology(tv_hint, lid_open, EdgeSide::Left);
            if !topo.visible_pc_monitors.is_empty() {
                if let Some(m) = topo
                    .visible_pc_monitors
                    .iter()
                    .find(|m| rect_contains(&m.rect, pt.x, pt.y))
                {
                    return (m.work_area, m.dpi_scale);
                }
                let m = &topo.visible_pc_monitors[0];
                return (m.work_area, m.dpi_scale);
            }
        }

        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        if !hmon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut mi) != 0 {
                return (mi.rcWork, dpi_scale_for_monitor(hmon));
            }
        }

        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
        (wa, dpi_scale_for_monitor(ptr::null_mut()))
    }
}

/// Picks the topmost visible normal PC window that is NOT stranded on the invisible TV ghost monitor.
pub fn pick_visible_pc_window(
    exclude_hwnd: HWND,
    tv_ghost: Option<RECT>,
) -> Option<HWND> {
    struct Search {
        exclude: HWND,
        tv_ghost: Option<RECT>,
        found: HWND,
    }
    unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp as *mut Search);
        if hwnd == s.exclude || IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return 1;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)) != 0 {
            return 1;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 || (r.right - r.left) < 40 || (r.bottom - r.top) < 30 {
            return 1;
        }
        if let Some(tv) = s.tv_ghost {
            let cx = r.left + (r.right - r.left) / 2;
            let cy = r.top + (r.bottom - r.top) / 2;
            if rect_contains(&tv, cx, cy) {
                return 1;
            }
        }
        s.found = hwnd;
        0
    }
    let mut s = Search {
        exclude: exclude_hwnd,
        tv_ghost,
        found: ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(enum_cb), &mut s as *mut _ as LPARAM);
    }
    if s.found.is_null() {
        None
    } else {
        Some(s.found)
    }
}

pub fn window_on_rect(hwnd: HWND, r: &RECT) -> bool {
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        let mut wr: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut wr) == 0 {
            return false;
        }
        let cx = wr.left + (wr.right - wr.left) / 2;
        let cy = wr.top + (wr.bottom - wr.top) / 2;
        rect_contains(r, cx, cy)
    }
}

/// Checks whether the foreground window is running fullscreen on a visible PC monitor (e.g. a game).
pub fn is_foreground_fullscreen_on_pc(tv_ghost: Option<RECT>) -> bool {
    unsafe {
        let fg = windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if fg.is_null() {
            return false;
        }
        let mut wr: RECT = std::mem::zeroed();
        if GetWindowRect(fg, &mut wr) == 0 {
            return false;
        }
        if let Some(tv) = tv_ghost {
            let cx = wr.left + (wr.right - wr.left) / 2;
            let cy = wr.top + (wr.bottom - wr.top) / 2;
            if rect_contains(&tv, cx, cy) {
                return false;
            }
        }
        let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() {
            return false;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut mi) == 0 {
            return false;
        }
        let mr = mi.rcMonitor;
        // Ignore Windows desktop / shell windows
        let mut cls = [0u16; 64];
        let n = windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(fg, cls.as_mut_ptr(), 64);
        if n > 0 {
            let name = from_wide(&cls[..n as usize]);
            if name == "Progman" || name == "WorkerW" || name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd" {
                return false;
            }
        }
        wr.left <= mr.left && wr.top <= mr.top && wr.right >= mr.right && wr.bottom >= mr.bottom
    }
}

pub fn register_lid_notification(hwnd: HWND) -> HPOWERNOTIFY {
    unsafe {
        RegisterPowerSettingNotification(
            hwnd as *mut _,
            &GUID_LIDSWITCH_STATE_CHANGE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    }
}

pub fn unregister_lid_notification(handle: HPOWERNOTIFY) {
    if handle != 0 {
        unsafe {
            let _ = UnregisterPowerSettingNotification(handle);
        }
    }
}

pub fn parse_lid_power_broadcast(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<bool> {
    if msg != WM_POWERBROADCAST || wp as u32 != PBT_POWERSETTINGCHANGE || lp == 0 {
        return None;
    }
    unsafe {
        let pbs = &*(lp as *const POWERBROADCAST_SETTING);
        let g = pbs.PowerSetting;
        if g.data1 == GUID_LIDSWITCH_STATE_CHANGE.data1
            && g.data2 == GUID_LIDSWITCH_STATE_CHANGE.data2
            && g.data3 == GUID_LIDSWITCH_STATE_CHANGE.data3
            && g.data4 == GUID_LIDSWITCH_STATE_CHANGE.data4
            && pbs.DataLength >= 1
        {
            let val = pbs.Data[0];
            return Some(val != 0);
        }
    }
    None
}

/// Optional helper: temporarily switch Windows to "PC screen only" when entering TV mode,
/// and restore the exact prior Extend display config when returning to DP.
#[derive(Default)]
pub struct SavedDisplayConfig {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
    switched_to_internal: bool,
}

impl SavedDisplayConfig {
    pub fn switch_to_internal_if_extended(&mut self, tv_hint: &str, lid_open: bool) -> bool {
        if self.switched_to_internal || !lid_open {
            return false;
        }
        let topo = query_display_topology(tv_hint, lid_open, EdgeSide::Left);
        if topo.tv_ghost_monitor.is_none() || topo.visible_pc_monitors.is_empty() {
            return false;
        }
        if !topo.visible_pc_monitors.iter().any(|m| m.is_internal) {
            return false;
        }
        unsafe {
            let mut pc: u32 = 0;
            let mut mc: u32 = 0;
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut pc, &mut mc) != 0 || pc == 0 {
                return false;
            }
            let mut paths = vec![std::mem::zeroed::<DISPLAYCONFIG_PATH_INFO>(); pc as usize];
            let mut modes = vec![std::mem::zeroed::<DISPLAYCONFIG_MODE_INFO>(); mc as usize];
            if QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut pc,
                paths.as_mut_ptr(),
                &mut mc,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            ) != 0
            {
                return false;
            }
            paths.truncate(pc as usize);
            modes.truncate(mc as usize);

            let rc = SetDisplayConfig(
                0,
                ptr::null(),
                0,
                ptr::null(),
                SDC_APPLY | SDC_TOPOLOGY_INTERNAL,
            );
            if rc == 0 {
                self.paths = paths;
                self.modes = modes;
                self.switched_to_internal = true;
                return true;
            }
        }
        false
    }

    pub fn restore_if_needed(&mut self) {
        if !self.switched_to_internal {
            return;
        }
        self.switched_to_internal = false;
        let paths = std::mem::take(&mut self.paths);
        let modes = std::mem::take(&mut self.modes);
        unsafe {
            let rc = if !paths.is_empty() && !modes.is_empty() {
                SetDisplayConfig(
                    paths.len() as u32,
                    paths.as_ptr(),
                    modes.len() as u32,
                    modes.as_ptr(),
                    SDC_APPLY
                        | SDC_USE_SUPPLIED_DISPLAY_CONFIG
                        | SDC_ALLOW_CHANGES
                        | SDC_SAVE_TO_DATABASE,
                )
            } else {
                -1
            };
            if rc != 0 {
                let _ = SetDisplayConfig(
                    0,
                    ptr::null(),
                    0,
                    ptr::null(),
                    SDC_APPLY | SDC_TOPOLOGY_EXTEND,
                );
            }
        }
    }
}
