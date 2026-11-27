use super::state::*;
use crate::adb::DeviceController;
use crate::config::to_wide;
use crate::display::{is_foreground_fullscreen_on_pc, DisplayTopology, EdgeSide};
use crate::scrcpy_proto::ScrcpyControlClient;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const EDGE_CLASS: &str = "MiKVMEdgeTriggerWnd";
const TIMER_EDGE_DWELL: usize = 601;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;

pub(super) struct TvEdgeSensor {
    child: Child,
    stdin: ChildStdin,
    stop: Arc<AtomicBool>,
    last_side: String,
}

impl TvEdgeSensor {
    fn spawn_attempt(
        adb_path: &PathBuf,
        addr: &str,
        shell_cmd: &str,
    ) -> Option<(Child, ChildStdin, BufReader<std::process::ChildStdout>)> {
        let mut child = Command::new(adb_path)
            .args(["-s", addr, "shell", shell_cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout_raw = child.stdout.take()?;
        let mut reader = BufReader::new(stdout_raw);
        let mut first = String::new();
        if reader.read_line(&mut first).unwrap_or(0) > 0 && first.trim().starts_with("READY") {
            let mut parts = first.split_whitespace();
            let _ = parts.next();
            if let (Some(Ok(w)), Some(Ok(h))) = (
                parts.next().map(|s| s.parse::<i32>()),
                parts.next().map(|s| s.parse::<i32>()),
            ) {
                if w > 0 && h > 0 {
                    TV_SCREEN_W.store(w, Ordering::Relaxed);
                    TV_SCREEN_H.store(h, Ordering::Relaxed);
                }
            }
            return Some((child, stdin, reader));
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    pub(super) fn start(
        dev: &Arc<DeviceController>,
        adb_path: &PathBuf,
        addr: &str,
        initial_side: &str,
    ) -> Option<Self> {
        dev.ensure_jars_deployed();
        // 1. Try rootless com.android.shell overlay first
        let _ = dev.shell(
            "appops set com.android.shell SYSTEM_ALERT_WINDOW allow >/dev/null 2>&1",
            2000,
        );
        let rootless_cmd = format!(
            "CLASSPATH=/data/local/tmp/MiTVEdge.jar:/data/local/tmp/scrcpy-server.jar /system/bin/app_process /data/local/tmp MiTVEdge {}",
            initial_side
        );
        let started = Self::spawn_attempt(adb_path, addr, &rootless_cmd).or_else(|| {
            // 2. Fallback to rooted uid 1000 system overlay
            let rooted_cmd = format!(
                "su 1000 -c 'CLASSPATH=/data/local/tmp/MiTVEdge.jar:/data/local/tmp/scrcpy-server.jar /system/bin/app_process /data/local/tmp MiTVEdge {}'",
                initial_side
            );
            Self::spawn_attempt(adb_path, addr, &rooted_cmd)
        });

        let (child, stdin, mut reader) = started?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_r = Arc::clone(&stop);

        std::thread::spawn(move || {
            let mut line = String::new();
            while !stop_r.load(Ordering::Relaxed) {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim();
                        if t == "L" {
                            TV_EDGE_ACTIVE.store(false, Ordering::Release);
                            TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                        } else if let Some(rest) = t.strip_prefix("H ") {
                            if let Ok(r10k) = rest.trim().parse::<u32>() {
                                TV_EDGE_RATIO.store(r10k.min(10000), Ordering::Relaxed);
                                TV_EDGE_ACTIVE.store(true, Ordering::Release);
                                TV_EDGE_SEQ.fetch_add(1, Ordering::Release);
                            }
                        }
                    }
                }
            }
            TV_EDGE_ACTIVE.store(false, Ordering::Release);
            TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        });

        Some(Self {
            child,
            stdin,
            stop,
            last_side: initial_side.to_string(),
        })
    }

    pub(super) fn set_side(&mut self, side_cmd: &str) {
        if self.last_side == side_cmd {
            return;
        }
        self.last_side.clear();
        self.last_side.push_str(side_cmd);
        let _ = self.stdin.write_all(format!("S {}\n", side_cmd).as_bytes());
        let _ = self.stdin.flush();
    }

    pub(super) fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.stdin.write_all(b"Q\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            if !matches!(self.child.try_wait(), Ok(None)) {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

/// Computes rounded UHID counts for a primed burst given ratio delta (`-10000..=10000`),
/// TV logical span in pixels, and primed acceleration multiplier x100 (`100..=500`).
pub(super) fn compute_primed_counts(err_r10k: i32, tv_span: i32, accel_x100: i32) -> i32 {
    let accel = accel_x100.clamp(100, 500) as i64;
    let span = tv_span.max(1) as i64;
    let want_px = (err_r10k as i64 * span) / 10000;
    let num = want_px * 100;
    let rounded = if num >= 0 {
        (num + accel / 2) / accel
    } else {
        (num - accel / 2) / accel
    };
    rounded as i32
}

/// Velocity-primed single-frame PC -> TV edge y-alignment!
/// Sends Packet 1 = `(pin_dx, 0)` (pins cursor onto the 3px strip & primes Android VelocityControl)
/// followed immediately by `(0, sy)` packets in ONE single TCP write so all movement runs at
/// exact linear `3.000x` acceleration (+/-1px accuracy across the full 1080p height on Frame 1!).
pub(super) fn spawn_tv_cursor_edge_align(client: Arc<ScrcpyControlClient>, target_r10k: usize) {
    let strip_side = TV_STRIP_SIDE.load(Ordering::Acquire);
    if strip_side == TV_SIDE_NONE {
        return;
    }
    let target = target_r10k.min(10000) as i32;
    let gen = TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    ALIGN_USER_DX.store(0, Ordering::Relaxed);
    ALIGN_USER_DY.store(0, Ordering::Relaxed);
    TV_ALIGNING.store(true, Ordering::Release);

    let was_on_edge = TV_LAST_EXITED_BY_EDGE.swap(false, Ordering::AcqRel);

    std::thread::spawn(move || {
        let aborted = || {
            TV_ALIGN_GEN.load(Ordering::Acquire) != gen
                || current_mode() != TakeoverMode::TvFocused
                || !client.is_alive()
        };

        let wait_edge_seq = |seq_before: u32, max_ms: u32| -> bool {
            let iters = (max_ms / 4).max(1);
            for _ in 0..iters {
                if aborted() {
                    return false;
                }
                if TV_EDGE_SEQ.load(Ordering::Acquire) != seq_before {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            false
        };

        let (dock_dx, dock_dy, pin_dx, pin_dy, nudge_dx, nudge_dy, horizontal_strip) =
            match strip_side {
                TV_SIDE_RIGHT => (800, 0, 40, 0, -16, 0, false),
                TV_SIDE_LEFT => (-800, 0, -40, 0, 16, 0, false),
                TV_SIDE_BOTTOM => (0, 800, 0, 40, 0, -16, true),
                TV_SIDE_TOP => (0, -800, 0, -40, 0, 16, true),
                _ => {
                    TV_ALIGNING.store(false, Ordering::Release);
                    return;
                }
            };

        let tv_span = if horizontal_strip {
            TV_SCREEN_W.load(Ordering::Relaxed).max(480)
        } else {
            TV_SCREEN_H.load(Ordering::Relaxed).max(480)
        };

        let send_primed_axis = |counts: i32| {
            if horizontal_strip {
                client.send_primed_mouse_burst(0, pin_dy, counts, 0);
            } else {
                client.send_primed_mouse_burst(pin_dx, 0, 0, counts);
            }
        };

        let counts_for_err = |err_r10k: i32| -> i32 {
            let accel = TV_PRIMED_ACCEL_X100.load(Ordering::Relaxed);
            compute_primed_counts(err_r10k, tv_span, accel)
        };

        let mut need_dock = !was_on_edge;

        if was_on_edge {
            let start_r10k = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
            let err0 = target - start_r10k;

            // Fast path: cursor is ALREADY sitting on the edge at practically the exact same height!
            if err0.abs() <= 120 {
                TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                TV_ENTER_GRACE_UNTIL
                    .store(get_tick_count().wrapping_add(160), Ordering::Release);
                TV_ALIGNING.store(false, Ordering::Release);
                if !aborted() {
                    let udx = ALIGN_USER_DX.swap(0, Ordering::Relaxed);
                    let udy = ALIGN_USER_DY.swap(0, Ordering::Relaxed);
                    client.send_mouse_burst(nudge_dx + udx, nudge_dy + udy);
                }
                return;
            }

            // Frame 1 (VERY FIRST TCP WRITE): Primed burst teleports straight to target_y (+/-1px!)
            let pred_counts = counts_for_err(err0);
            let seq0 = TV_EDGE_SEQ.load(Ordering::Acquire);
            if pred_counts != 0 {
                send_primed_axis(pred_counts);
            } else {
                client.send_mouse_burst(pin_dx, pin_dy);
            }

            if wait_edge_seq(seq0, 160) {
                let mut landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                // If Packet 1 (prime) emitted an early H before Packet 2's H arrived, wait 1 VSync
                if (target - landed).abs() > 220 && !aborted() {
                    std::thread::sleep(Duration::from_millis(18));
                    landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                }
                // Self-calibrate primed acceleration if non-saturated & moved cleanly in expected direction
                if pred_counts.abs() >= 25 && landed > 250 && landed < 9750 {
                    let moved_px =
                        (((landed - start_r10k) as i64 * tv_span as i64) / 10000) as i32;
                    if moved_px.signum() == pred_counts.signum() && moved_px.abs() >= 45 {
                        let measured = ((moved_px.abs() as i64 * 100) / pred_counts.abs() as i64)
                            .clamp(100, 500) as i32;
                        let prev = TV_PRIMED_ACCEL_X100.load(Ordering::Relaxed).clamp(100, 500);
                        let blended = (prev * 4 + measured * 6) / 10;
                        TV_PRIMED_ACCEL_X100.store(blended.clamp(100, 500), Ordering::Relaxed);
                    }
                }
            } else if !aborted() {
                // Cursor wasn't actually on the strip (drifted inward) -> dock & align cleanly!
                need_dock = true;
            }
        }

        if need_dock && !aborted() {
            let seq_d = TV_EDGE_SEQ.load(Ordering::Acquire);
            client.send_mouse_burst(dock_dx, dock_dy);
            let _ = wait_edge_seq(seq_d, 160);
        }

        // Closed-loop safety net (runs 0 iterations on normal primed hits; up to 2 if residual > 2.2%)
        for _ in 0..2 {
            if aborted() {
                TV_ALIGNING.store(false, Ordering::Release);
                return;
            }
            let cur = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
            let err = target - cur;
            if err.abs() <= 220 {
                break;
            }
            let corr = counts_for_err(err);
            if corr == 0 {
                break;
            }
            let seq_c = TV_EDGE_SEQ.load(Ordering::Acquire);
            send_primed_axis(corr);
            if wait_edge_seq(seq_c, 150) {
                let landed = TV_EDGE_RATIO.load(Ordering::Relaxed) as i32;
                if (target - landed).abs() > 220 && !aborted() {
                    std::thread::sleep(Duration::from_millis(18));
                }
            } else {
                break;
            }
        }

        // Step 3: Release alignment lock & apply inward nudge + buffered user swipe momentum
        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        TV_ENTER_GRACE_UNTIL.store(get_tick_count().wrapping_add(160), Ordering::Release);
        TV_ALIGNING.store(false, Ordering::Release);
        if !aborted() {
            let udx = ALIGN_USER_DX.swap(0, Ordering::Relaxed);
            let udy = ALIGN_USER_DY.swap(0, Ordering::Relaxed);
            client.send_mouse_burst(nudge_dx + udx, nudge_dy + udy);
        }
    });
}

pub(super) fn create_edge_trigger_window() -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let cls_w = to_wide(EDGE_CLASS);
        static REG: std::sync::Once = std::sync::Once::new();
        REG.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(edge_wnd_proc),
                hInstance: hinst,
                hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                lpszClassName: cls_w.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            cls_w.as_ptr(),
            to_wide("MiKVM Edge").as_ptr(),
            WS_POPUP,
            0,
            0,
            2,
            2,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        if !hwnd.is_null() {
            SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
        }
        hwnd
    }
}

pub(super) fn show_edge_trigger_window(hwnd: HWND, topo: &DisplayTopology) {
    if hwnd.is_null() {
        return;
    }
    let Some(rect) = topo.edge_trigger_rect else {
        hide_edge_trigger_window(hwnd);
        return;
    };
    let Some(side) = topo.edge_side else {
        hide_edge_trigger_window(hwnd);
        return;
    };
    let fallback_span = topo
        .pc_fence_rect
        .or_else(|| topo.visible_pc_monitors.first().map(|m| m.rect))
        .map(|r| match side {
            EdgeSide::Left | EdgeSide::Right => (r.top, r.bottom),
            EdgeSide::Top | EdgeSide::Bottom => (r.left, r.right),
        })
        .unwrap_or((0, 1080));
    let seam_span = if topo.seam_span.1 > topo.seam_span.0 {
        topo.seam_span
    } else {
        fallback_span
    };
    if let Ok(mut g) = EDGE_CFG.lock() {
        *g = Some(EdgeRuntimeCfg {
            side,
            seam: topo.seam_coord,
            seam_span,
            is_ghost: topo.edge_trigger_is_ghost,
            tv_ghost_rect: topo.tv_ghost_monitor.as_ref().map(|m| m.rect),
        });
    }
    EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
    PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
    unsafe {
        KillTimer(hwnd, TIMER_EDGE_DWELL);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

pub(super) fn hide_edge_trigger_window(hwnd: HWND) {
    EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
    if !hwnd.is_null() {
        unsafe {
            KillTimer(hwnd, TIMER_EDGE_DWELL);
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn should_suppress_edge_crossing(tv_ghost_rect: Option<RECT>) -> bool {
    unsafe {
        // Don't hijack while user is dragging a window or selecting text with left mouse button
        if (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 {
            return true;
        }
        if is_foreground_fullscreen_on_pc(tv_ghost_rect) {
            return true;
        }
    }
    false
}

pub(super) fn compute_pc_entry_ratio_10k(
    side: EdgeSide,
    seam_span: (i32, i32),
    pt: POINT,
) -> usize {
    let (start, end) = seam_span;
    let coord = match side {
        EdgeSide::Left | EdgeSide::Right => pt.y,
        EdgeSide::Top | EdgeSide::Bottom => pt.x,
    };
    let ratio = ((coord - start) as f32 / (end - start).max(1) as f32).clamp(0.0, 1.0);
    (ratio * 10000.0).round() as usize
}

fn fire_edge_enter(hwnd: HWND, cfg: EdgeRuntimeCfg) {
    EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
    unsafe {
        KillTimer(hwnd, TIMER_EDGE_DWELL);
    }
    let mut pt: POINT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
    }
    let ratio_10k = compute_pc_entry_ratio_10k(cfg.side, cfg.seam_span, pt);
    let host = HOST_HWND_GLOBAL.load(Ordering::Acquire);
    if host != 0 {
        unsafe {
            PostMessageW(host as HWND, WM_EDGE_ENTER_TV, ratio_10k, 0);
        }
    }
}

unsafe extern "system" fn edge_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            if current_mode() != TakeoverMode::TvStandby {
                return 0;
            }
            let cfg = EDGE_CFG.try_lock().ok().and_then(|g| *g);
            let Some(cfg) = cfg else {
                return 0;
            };
            if should_suppress_edge_crossing(cfg.tv_ghost_rect) {
                return 0;
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);

            let res_pc = EDGE_RES_PC.load(Ordering::Relaxed).clamp(0, 300);
            if cfg.is_ghost {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let depth = match cfg.side {
                    EdgeSide::Left => cfg.seam - pt.x,
                    EdgeSide::Right => pt.x - cfg.seam,
                    EdgeSide::Top => cfg.seam - pt.y,
                    EdgeSide::Bottom => pt.y - cfg.seam,
                }
                .max(1);

                let now = get_tick_count();
                let prev_ms = PC_EDGE_PUSH_LAST_MS.swap(now, Ordering::Relaxed);
                if now.wrapping_sub(prev_ms) > PUSH_DECAY_MS {
                    PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                }
                let accum = PC_EDGE_PUSH_ACCUM.fetch_add(depth, Ordering::Relaxed) + depth;
                if accum >= res_pc {
                    PC_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
                    fire_edge_enter(hwnd, cfg);
                    return 0;
                }
                // Elastic pressure barrier: bounce cursor right back onto the visible PC screen border
                // until cumulative push exceeds `res_pc` (cursor never vanishes into ghost screen!)
                match cfg.side {
                    EdgeSide::Left => SetCursorPos(cfg.seam, pt.y),
                    EdgeSide::Right => SetCursorPos(cfg.seam, pt.y),
                    EdgeSide::Top => SetCursorPos(pt.x, cfg.seam),
                    EdgeSide::Bottom => SetCursorPos(pt.x, cfg.seam),
                };
            } else if !EDGE_DWELL_ACTIVE.swap(true, Ordering::AcqRel) {
                let dwell_ms = (40 + res_pc).clamp(40, 340) as u32;
                SetTimer(hwnd, TIMER_EDGE_DWELL, dwell_ms, None);
            }
            0
        }
        WM_MOUSELEAVE_MSG => {
            EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
            KillTimer(hwnd, TIMER_EDGE_DWELL);
            0
        }
        WM_TIMER => {
            if wp == TIMER_EDGE_DWELL {
                EDGE_DWELL_ACTIVE.store(false, Ordering::Release);
                KillTimer(hwnd, TIMER_EDGE_DWELL);
                if current_mode() == TakeoverMode::TvStandby {
                    let cfg = EDGE_CFG.try_lock().ok().and_then(|g| *g);
                    if let Some(c) = cfg {
                        if !should_suppress_edge_crossing(c.tv_ghost_rect) {
                            let mut pt: POINT = std::mem::zeroed();
                            let mut wr: RECT = std::mem::zeroed();
                            if GetCursorPos(&mut pt) != 0
                                && GetWindowRect(hwnd, &mut wr) != 0
                                && pt.x >= wr.left
                                && pt.x < wr.right
                                && pt.y >= wr.top
                                && pt.y < wr.bottom
                            {
                                fire_edge_enter(hwnd, c);
                            }
                        }
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primed_counts_exact_3x_rounding() {
        // 1080px screen, 3.00x acceleration:
        // -50% (-540px) / 3.0 = -180 counts
        assert_eq!(compute_primed_counts(-5000, 1080, 300), -180);
        // +50% (+540px) / 3.0 = +180 counts
        assert_eq!(compute_primed_counts(5000, 1080, 300), 180);
        // -900px / 3.0 = -300 counts
        assert_eq!(compute_primed_counts(-8333, 1080, 300), -300);
        // Small delta -18px / 3.0 = -6 counts
        assert_eq!(compute_primed_counts(-167, 1080, 300), -6);
        assert_eq!(compute_primed_counts(0, 1080, 300), 0);
        // Acceleration clamp bounds (100..=500)
        assert_eq!(compute_primed_counts(5000, 1000, 50), 500);
        assert_eq!(compute_primed_counts(5000, 1000, 900), 100);
    }

    #[test]
    fn pc_entry_ratio_vertical_and_horizontal() {
        // Standard 1600px vertical span [559, 2159]
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Left, (559, 2159), POINT { x: 3820, y: 1359 }),
            5000
        );
        // Staggered overlap span [400, 1600] maps 400 -> 0%, 1000 -> 50%, 1600 -> 100%
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Left, (400, 1600), POINT { x: 3820, y: 400 }),
            0
        );
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Left, (400, 1600), POINT { x: 3820, y: 1000 }),
            5000
        );
        // Clamped above start / below end
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Left, (559, 2159), POINT { x: 3820, y: 0 }),
            0
        );
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Right, (559, 2159), POINT { x: 6420, y: 9999 }),
            10000
        );
        // Horizontal seam with negative coordinates [-2000, 0]
        assert_eq!(
            compute_pc_entry_ratio_10k(EdgeSide::Top, (-2000, 0), POINT { x: -500, y: -5 }),
            7500
        );
    }
}
