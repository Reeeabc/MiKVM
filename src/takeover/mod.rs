mod dp_watcher;
mod edge;
mod hooks;
mod state;

pub use hooks::handle_raw_input;
pub use state::{
    current_mode, TakeoverMode, WM_CONTROL_EXITED, WM_CONTROL_READY, WM_DP_RETURNED,
    WM_EDGE_ENTER_TV, WM_RECOVERY_FAILED, WM_TRIGGER_RETURN_DP, WM_TRIGGER_TOGGLE_FOCUS,
    WM_TV_CLIPBOARD, WM_TV_EDGE_EXIT,
};

use crate::adb::DeviceController;
use crate::clipboard::{clipboard_sequence, read_pc_clipboard_text, write_pc_clipboard_text};
use crate::config::AppConfig;
use crate::display::{
    pick_visible_pc_window, query_display_topology, rect_contains, window_on_rect,
    DisplayTopology, EdgeSide, SavedDisplayConfig,
};
use crate::keymap::KeyMap;
use crate::scrcpy_proto::ScrcpyControlClient;
use dp_watcher::spawn_dp_return_watcher;
use edge::{
    create_edge_trigger_window, hide_edge_trigger_window, show_edge_trigger_window,
    spawn_tv_cursor_edge_align, TvEdgeSensor,
};
use hooks::{
    install_keyboard_hook, install_mouse_hook, release_pc_modifiers_and_swallow,
    set_raw_input_device, swallow_currently_held_modifiers, uninstall_keyboard_hook,
    uninstall_mouse_hook, StickyKeysGuard,
};
use state::*;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct TakeoverSession {
    scrcpy_dir: PathBuf,
    adb_path: PathBuf,
    cfg: Arc<Mutex<AppConfig>>,
    dev: Arc<DeviceController>,
    host_hwnd: usize,
    edge_hwnd: usize,
    client: Arc<Mutex<Option<Arc<ScrcpyControlClient>>>>,
    edge_sensor: Arc<Mutex<Option<TvEdgeSensor>>>,
    edge_sensor_gen: Arc<AtomicU32>,
    watch_stop: Mutex<Option<Arc<AtomicBool>>>,
    recovery_gen: Arc<AtomicU32>,
    recovering: Arc<AtomicBool>,
    suspended_was_active: bool,
    last_resume_tick: u32,
    lid_open: AtomicBool,
    topo: DisplayTopology,
    saved_display: SavedDisplayConfig,
    sticky_keys: StickyKeysGuard,
    saved_pc_cursor: Option<POINT>,
    last_pc_hwnd: usize,
    raw_input_active: bool,
    fence_active: bool,
    last_clip_seq: u32,
    last_synced_clip: String,
}

impl TakeoverSession {
    pub fn new(
        scrcpy_dir: PathBuf,
        cfg: Arc<Mutex<AppConfig>>,
        dev: Arc<DeviceController>,
        host_hwnd: HWND,
    ) -> Self {
        let adb_path = scrcpy_dir.join("adb.exe");
        HOST_HWND_GLOBAL.store(host_hwnd as usize, Ordering::Release);
        if let Ok(mut g) = GLOBAL_DEV.lock() {
            *g = Some(Arc::clone(&dev));
        }
        let (tv_hint, fallback_side, edge_on, res_pc, res_tv, keymap) = cfg
            .lock()
            .map(|c| {
                (
                    c.tv_monitor_hint.clone(),
                    c.tv_edge_side,
                    c.edge_switch,
                    c.edge_resistance_pc,
                    c.edge_resistance_tv,
                    c.keymap.clone(),
                )
            })
            .unwrap_or_else(|_| {
                ("Mi Monitor".to_string(), EdgeSide::Left, true, 120, 180, KeyMap::default())
            });
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        EDGE_SWITCH_ATOMIC.store(edge_on, Ordering::Relaxed);
        EDGE_RES_PC.store(res_pc.clamp(0, 300), Ordering::Relaxed);
        EDGE_RES_TV.store(res_tv.clamp(0, 300), Ordering::Relaxed);

        let edge_hwnd = create_edge_trigger_window();
        let topo = query_display_topology(&tv_hint, true, fallback_side);
        persist_detected_edge_side(&cfg, &topo);

        Self {
            scrcpy_dir,
            adb_path,
            cfg,
            dev,
            host_hwnd: host_hwnd as usize,
            edge_hwnd: edge_hwnd as usize,
            client: Arc::new(Mutex::new(None)),
            edge_sensor: Arc::new(Mutex::new(None)),
            edge_sensor_gen: Arc::new(AtomicU32::new(0)),
            watch_stop: Mutex::new(None),
            recovery_gen: Arc::new(AtomicU32::new(0)),
            recovering: Arc::new(AtomicBool::new(false)),
            suspended_was_active: false,
            last_resume_tick: 0,
            lid_open: AtomicBool::new(true),
            topo,
            saved_display: SavedDisplayConfig::default(),
            sticky_keys: StickyKeysGuard::default(),
            saved_pc_cursor: None,
            last_pc_hwnd: 0,
            raw_input_active: false,
            fence_active: false,
            last_clip_seq: clipboard_sequence(),
            last_synced_clip: String::new(),
        }
    }

    pub fn mode(&self) -> TakeoverMode {
        current_mode()
    }

    pub fn is_running(&self) -> bool {
        if let Ok(g) = self.client.lock() {
            if let Some(ref c) = *g {
                return c.is_alive();
            }
        }
        false
    }

    pub fn is_lid_open(&self) -> bool {
        self.lid_open.load(Ordering::Acquire)
    }

    pub fn is_recovering(&self) -> bool {
        self.recovering.load(Ordering::Acquire)
    }

    pub fn sync_config(&mut self) {
        let (edge_on, res_pc, res_tv, keymap) = self
            .cfg
            .lock()
            .map(|c| (c.edge_switch, c.edge_resistance_pc, c.edge_resistance_tv, c.keymap.clone()))
            .unwrap_or_else(|_| (true, 120, 180, KeyMap::default()));
        if let Ok(mut km) = ACTIVE_KEYMAP.lock() {
            *km = Some(keymap);
        }
        EDGE_SWITCH_ATOMIC.store(edge_on, Ordering::Relaxed);
        EDGE_RES_PC.store(res_pc.clamp(0, 300), Ordering::Relaxed);
        EDGE_RES_TV.store(res_tv.clamp(0, 300), Ordering::Relaxed);
        if self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        }
    }

    fn refresh_topology(&mut self) {
        let (hint, fallback) = self
            .cfg
            .lock()
            .map(|c| (c.tv_monitor_hint.clone(), c.tv_edge_side))
            .unwrap_or_else(|_| ("Mi Monitor".to_string(), EdgeSide::Left));
        let lid = self.lid_open.load(Ordering::Acquire);
        self.topo = query_display_topology(&hint, lid, fallback);
        // Remember & persist auto-detected physical side whenever Extend mode gives us a real seam
        persist_detected_edge_side(&self.cfg, &self.topo);
    }

    /// Blocking connect called on background worker thread; posts WM_CONTROL_READY on success.
    pub fn start_control_blocking(&self) -> Result<(), String> {
        self.recovery_gen.fetch_add(1, Ordering::AcqRel);
        self.recovering.store(false, Ordering::Release);
        if self.is_running() {
            unsafe {
                PostMessageW(self.host_hwnd as HWND, WM_CONTROL_READY, 0, 0);
            }
            return Ok(());
        }
        let server_path = self.scrcpy_dir.join("scrcpy-server");
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();

        connect_and_install_client(
            &self.dev,
            &self.adb_path,
            server_path,
            addr,
            self.host_hwnd,
            &self.client,
            false,
        )
    }

    pub fn on_control_ready_ui(&mut self, prefer_standby: bool) {
        self.recovering.store(false, Ordering::Release);
        self.suspended_was_active = false;
        self.sync_config();
        // Capture & persist the real Extend seam side BEFORE AutoInternalOnTv collapses to single-screen!
        self.refresh_topology();
        let (auto_internal, tv_hint) = self
            .cfg
            .lock()
            .map(|c| (c.auto_internal_on_tv, c.tv_monitor_hint.clone()))
            .unwrap_or((false, "Mi Monitor".to_string()));
        if auto_internal
            && self
                .saved_display
                .switch_to_internal_if_extended(&tv_hint, self.is_lid_open())
        {
            self.refresh_topology();
        }
        self.capture_last_pc_window();
        self.capture_pc_cursor_if_visible();

        self.dev.start_persistent_shell();
        self.dev.set_keep_warm(true);

        if self.topo.has_visible_pc() {
            if prefer_standby {
                self.enter_mode(TakeoverMode::TvStandby);
            } else {
                self.enter_mode(TakeoverMode::TvFocused);
            }
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
        self.start_edge_sensor_async();
        if prefer_standby {
            self.start_dp_return_watcher();
        }
    }

    /// Unexpected control channel disconnect (e.g. Wi-Fi drop):
    /// immediately releases PC hooks so Windows never freezes, then runs bounded silent recovery.
    pub fn on_control_exited_ui(&mut self) {
        if !self.mode().is_session_active() || self.recovering.load(Ordering::Acquire) {
            return;
        }
        diag_log!("RESIL", "control channel exited unexpectedly; starting backoff recovery");
        self.teardown_transport_keep_display();
        self.spawn_recovery_worker(&[1000, 2000, 4000], true);
    }

    /// PC entering sleep/hibernation: cleanly release hooks and close live sockets so wake never hangs.
    pub fn on_power_suspend_ui(&mut self) {
        let was_active = self.mode().is_session_active() || self.recovering.load(Ordering::Acquire);
        diag_log!("POWER", "suspend was_active={}", was_active);
        self.suspended_was_active = was_active;
        self.recovery_gen.fetch_add(1, Ordering::AcqRel);
        self.recovering.store(false, Ordering::Release);
        if was_active {
            self.teardown_transport_keep_display();
        }
        let dev = Arc::clone(&self.dev);
        std::thread::spawn(move || dev.reset_stale_connections());
    }

    /// PC resumed from sleep: debounce duplicate resume events, reset stale ADB pipes,
    /// and silently recover active TV sessions once Wi-Fi re-associates.
    /// Returns `true` if a non-session silent status refresh should be triggered.
    pub fn on_power_resume_ui(&mut self) -> bool {
        let now = get_tick_count();
        if self.last_resume_tick != 0 && now.wrapping_sub(self.last_resume_tick) < 3000 {
            return false;
        }
        self.last_resume_tick = now;
        self.refresh_topology();
        diag_log!("POWER", "resume suspended_was_active={}", self.suspended_was_active);

        if self.suspended_was_active {
            self.suspended_was_active = false;
            self.spawn_recovery_worker(&[2000, 3000, 4000], false);
            return false;
        }
        let dev = Arc::clone(&self.dev);
        std::thread::spawn(move || dev.reset_stale_connections());
        true
    }

    fn teardown_transport_keep_display(&mut self) {
        self.enter_mode(TakeoverMode::Idle);
        self.stop_edge_sensor();
        self.stop_dp_return_watcher();
        self.dev.stop_persistent_shell();
        self.dev.set_keep_warm(false);

        if let Ok(mut gg) = GLOBAL_CLIENT.lock() {
            *gg = None;
        }
        if let Ok(mut g) = self.client.lock() {
            if let Some(c) = g.take() {
                c.close();
            }
        }
    }

    fn spawn_recovery_worker(&self, backoffs_ms: &[u64], notify_on_fail: bool) {
        let gen = self.recovery_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.recovering.store(true, Ordering::Release);

        let gen_ref = Arc::clone(&self.recovery_gen);
        let recovering_ref = Arc::clone(&self.recovering);
        let dev = Arc::clone(&self.dev);
        let adb_path = self.adb_path.clone();
        let server_path = self.scrcpy_dir.join("scrcpy-server");
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();
        let host = self.host_hwnd;
        let client_slot = Arc::clone(&self.client);
        let schedule = backoffs_ms.to_vec();

        std::thread::spawn(move || {
            let cancelled = || gen_ref.load(Ordering::Acquire) != gen;

            for (attempt, &delay_ms) in schedule.iter().enumerate() {
                let mut slept = 0u64;
                while slept < delay_ms {
                    if cancelled() {
                        return;
                    }
                    let step = (delay_ms - slept).min(150);
                    std::thread::sleep(Duration::from_millis(step));
                    slept += step;
                }
                if cancelled() {
                    return;
                }
                if attempt == 0 {
                    dev.reset_stale_connections();
                }
                if !dev.reconnect_force() || cancelled() {
                    continue;
                }
                match dev.is_tv_on_tv_system() {
                    Some(false) => {
                        // Display already switched back to DP/HDMI while offline
                        recovering_ref.store(false, Ordering::Release);
                        if !cancelled() {
                            unsafe {
                                PostMessageW(host as HWND, WM_DP_RETURNED, 0, 0);
                            }
                        }
                        return;
                    }
                    Some(true) => {
                        if connect_and_install_client(
                            &dev,
                            &adb_path,
                            server_path.clone(),
                            addr.clone(),
                            host,
                            &client_slot,
                            true,
                        )
                        .is_ok()
                        {
                            return;
                        }
                    }
                    None => {}
                }
            }

            if !cancelled() {
                recovering_ref.store(false, Ordering::Release);
                unsafe {
                    PostMessageW(
                        host as HWND,
                        WM_RECOVERY_FAILED,
                        if notify_on_fail { 1 } else { 0 },
                        0,
                    );
                }
            }
        });
    }

    fn capture_pc_cursor_if_visible(&mut self) {
        unsafe {
            let mut pt: POINT = std::mem::zeroed();
            if GetCursorPos(&mut pt) != 0 {
                if self.topo.visible_pc_monitors.iter().any(|m| rect_contains(&m.rect, pt.x, pt.y)) {
                    self.saved_pc_cursor = Some(pt);
                }
            }
        }
    }

    /// Parks the Windows cursor out of sight while controlling the TV:
    /// - Extend mode with ghost TV monitor: inside the invisible 27U ghost rect (100% hidden on laptop!)
    /// - Single-screen / Duplicate mode: extreme bottom-right pixel of PC monitor (arrow off-screen!)
    fn park_pc_cursor_hidden(&self) {
        unsafe {
            if let Some(ref ghost) = self.topo.tv_ghost_monitor {
                let gx = ghost.rect.left + ((ghost.rect.right - ghost.rect.left) / 4).min(160);
                let gy = ghost.rect.top + ((ghost.rect.bottom - ghost.rect.top) / 4).min(160);
                SetCursorPos(gx, gy);
            } else if let Some(pc) = self.topo.visible_pc_monitors.first() {
                SetCursorPos(pc.rect.right - 1, pc.rect.bottom - 1);
            }
        }
    }

    fn point_on_pc_edge(&self, ratio_10k: usize) -> POINT {
        self.topo.point_on_pc_edge(ratio_10k)
    }

    fn desired_tv_edge_cmd(&self) -> (u8, &'static str) {
        let edge_on = EDGE_SWITCH_ATOMIC.load(Ordering::Relaxed);
        // Keep the 3px transparent TV strip warm/VISIBLE during both TvFocused AND TvStandby
        // (zero interference while user is on PC, and 0ms WindowManager redraw latency on re-entry!).
        // Only hide it in TvLocked (single-screen) or Idle or when edge_switch is disabled.
        let mode = self.mode();
        if (mode == TakeoverMode::TvFocused || mode == TakeoverMode::TvStandby) && edge_on {
            if let Some(pc_side) = self.topo.edge_side {
                return tv_strip_for_pc_edge(pc_side);
            }
        }
        (TV_SIDE_NONE, "HIDE")
    }

    fn sync_tv_edge_sensor_side(&self) {
        let (side_code, side_cmd) = self.desired_tv_edge_cmd();
        TV_STRIP_SIDE.store(side_code, Ordering::Release);
        TV_EDGE_ACTIVE.store(false, Ordering::Release);
        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        if let Ok(mut g) = self.edge_sensor.try_lock() {
            if let Some(ref mut sensor) = *g {
                sensor.set_side(side_cmd);
            }
        }
    }

    fn start_edge_sensor_async(&self) {
        let gen = self.edge_sensor_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let gen_ref = Arc::clone(&self.edge_sensor_gen);
        let slot = Arc::clone(&self.edge_sensor);
        let dev = Arc::clone(&self.dev);
        let adb_path = self.adb_path.clone();
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();
        let (_, init_cmd) = self.desired_tv_edge_cmd();
        let init_side = init_cmd.to_string();

        std::thread::spawn(move || {
            // Stop any prior instance first
            if let Ok(mut g) = slot.lock() {
                if let Some(old) = g.take() {
                    old.shutdown();
                }
            }
            if gen_ref.load(Ordering::Acquire) != gen || !current_mode().is_session_active() {
                return;
            }
            if let Some(mut sensor) = TvEdgeSensor::start(&dev, &adb_path, &addr, &init_side) {
                if gen_ref.load(Ordering::Acquire) != gen || !current_mode().is_session_active() {
                    sensor.shutdown();
                    return;
                }
                // Re-sync side in case mode changed while sensor was starting up
                let side_code = TV_STRIP_SIDE.load(Ordering::Acquire);
                let live_cmd = match side_code {
                    TV_SIDE_LEFT => "LEFT",
                    TV_SIDE_RIGHT => "RIGHT",
                    TV_SIDE_TOP => "TOP",
                    TV_SIDE_BOTTOM => "BOTTOM",
                    _ => "HIDE",
                };
                if live_cmd != init_side {
                    sensor.set_side(live_cmd);
                }
                if let Ok(mut g) = slot.lock() {
                    *g = Some(sensor);
                } else {
                    sensor.shutdown();
                }
            }
        });
    }

    fn stop_edge_sensor(&self) {
        self.edge_sensor_gen.fetch_add(1, Ordering::AcqRel);
        TV_STRIP_SIDE.store(TV_SIDE_NONE, Ordering::Release);
        TV_EDGE_ACTIVE.store(false, Ordering::Release);
        TV_EDGE_PUSH_ACCUM.store(0, Ordering::Relaxed);
        let old = self.edge_sensor.lock().ok().and_then(|mut g| g.take());
        if let Some(sensor) = old {
            std::thread::spawn(move || sensor.shutdown());
        }
    }

    pub fn toggle_focus_ui(&mut self) -> TakeoverMode {
        match self.mode() {
            TakeoverMode::TvFocused => {
                self.refresh_topology();
                if self.topo.has_visible_pc() {
                    TV_LAST_EXITED_BY_EDGE.store(false, Ordering::Release);
                    self.enter_mode(TakeoverMode::TvStandby);
                } else {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvStandby => {
                self.refresh_topology();
                self.capture_last_pc_window();
                self.capture_pc_cursor_if_visible();
                if self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvFocused);
                } else {
                    self.enter_mode(TakeoverMode::TvLocked);
                }
            }
            TakeoverMode::TvLocked => {
                // Re-check if user opened lid or plugged a screen
                self.refresh_topology();
                if self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvStandby);
                }
            }
            TakeoverMode::Idle => {}
        }
        self.mode()
    }

    pub fn on_edge_enter_tv_ui(&mut self, entry_ratio_10k: usize) {
        if self.mode() != TakeoverMode::TvStandby {
            return;
        }
        diag_log!("EDGE", "PC -> TV ratio_10k={}", entry_ratio_10k);
        self.refresh_topology();
        self.capture_last_pc_window();
        // Save matching PC edge point in case the user later returns via hotkey
        self.saved_pc_cursor = Some(self.point_on_pc_edge(entry_ratio_10k));
        if self.topo.has_visible_pc() {
            self.enter_mode(TakeoverMode::TvFocused);
            if let Some(client) = self.client.lock().ok().and_then(|g| g.clone()) {
                spawn_tv_cursor_edge_align(client, entry_ratio_10k);
            }
        } else {
            self.enter_mode(TakeoverMode::TvLocked);
        }
    }

    /// Phase 2 entry point: called when TV-side 3px edge overlay + outward push exits back to PC.
    pub fn on_tv_edge_exit_ui(&mut self, y_ratio_10k: usize) {
        if self.mode() != TakeoverMode::TvFocused {
            return;
        }
        diag_log!("EDGE", "TV -> PC ratio_10k={}", y_ratio_10k);
        self.refresh_topology();
        if !self.topo.has_visible_pc() {
            return;
        }
        TV_LAST_EXITED_BY_EDGE.store(true, Ordering::Release);
        let pt = self.point_on_pc_edge(y_ratio_10k);
        self.saved_pc_cursor = Some(pt);
        self.enter_mode(TakeoverMode::TvStandby);
    }

    pub fn on_lid_state_changed(&mut self, is_open: bool) {
        let prev = self.lid_open.swap(is_open, Ordering::AcqRel);
        if prev != is_open && self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        }
    }

    pub fn on_display_changed(&mut self) {
        if self.mode().is_session_active() {
            self.refresh_topology_and_reconcile();
        } else {
            self.refresh_topology();
        }
    }

    fn refresh_topology_and_reconcile(&mut self) {
        self.refresh_topology();
        let cur = self.mode();
        match cur {
            TakeoverMode::TvLocked => {
                if self.topo.has_visible_pc() {
                    // User opened laptop lid or switched to Extend -> unlock multi-screen!
                    self.enter_mode(TakeoverMode::TvFocused);
                }
            }
            TakeoverMode::TvFocused => {
                if !self.topo.has_visible_pc() {
                    self.enter_mode(TakeoverMode::TvLocked);
                } else {
                    self.sync_tv_edge_sensor_side();
                }
            }
            TakeoverMode::TvStandby => {
                if !self.topo.has_visible_pc() {
                    // User closed laptop lid while working on PC -> pull focus cleanly to TV!
                    self.enter_mode(TakeoverMode::TvLocked);
                } else {
                    self.apply_standby_edge_or_fence();
                }
            }
            TakeoverMode::Idle => {}
        }
    }

    fn enter_mode(&mut self, next: TakeoverMode) {
        let prev = self.mode();
        if prev != next {
            diag_log!("MODE", "{:?} -> {:?} visible_pc={}", prev, next, self.topo.visible_pc_monitors.len());
        }

        // When entering TV capture from PC (Idle/TvStandby), flush any held Windows modifiers
        // BEFORE switching MODE_ATOMIC so Windows never gets stuck with Ctrl/Shift/Alt held!
        if next.is_tv_capturing() && !prev.is_tv_capturing() {
            release_pc_modifiers_and_swallow(true);
        }

        MODE_ATOMIC.store(next as u8, Ordering::Release);

        match next {
            TakeoverMode::Idle => {
                TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel);
                TV_ALIGNING.store(false, Ordering::Release);
                HELD_MODS.store(0, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                self.sticky_keys.restore();
                self.release_fence();
                self.hide_edge_trigger();
                self.set_raw_input(false);
                uninstall_mouse_hook();
                uninstall_keyboard_hook();
                self.sync_tv_edge_sensor_side();
                // Restore remembered PC cursor if it was parked hidden
                if prev.is_tv_capturing() {
                    if let Some(pt) = self.saved_pc_cursor.take() {
                        unsafe {
                            SetCursorPos(pt.x, pt.y);
                        }
                    }
                }
            }
            TakeoverMode::TvLocked | TakeoverMode::TvFocused => {
                HELD_MODS.store(0, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                let now = get_tick_count();
                TV_ENTER_GRACE_UNTIL.store(now.wrapping_add(TV_ENTER_GRACE_MS), Ordering::Release);
                self.sticky_keys.suppress();
                self.release_fence();
                self.hide_edge_trigger();
                // Hide Windows cursor off the visible laptop screen while controlling TV
                self.park_pc_cursor_hidden();
                install_keyboard_hook();
                install_mouse_hook();
                self.set_raw_input(true);
                self.sync_tv_edge_sensor_side();
                self.sync_pc_clipboard_to_tv_if_changed();
            }
            TakeoverMode::TvStandby => {
                TV_ALIGN_GEN.fetch_add(1, Ordering::AcqRel);
                TV_ALIGNING.store(false, Ordering::Release);
                TV_LBUTTON_DOWN.store(false, Ordering::Release);
                self.sticky_keys.restore();
                self.sync_tv_edge_sensor_side();
                // Release any held keys/buttons on TV first so zero stuck keys!
                if prev.is_tv_capturing() {
                    swallow_currently_held_modifiers();
                    if let Ok(g) = self.client.lock() {
                        if let Some(ref c) = *g {
                            c.release_all_inputs();
                        }
                    }
                } else {
                    HELD_MODS.store(0, Ordering::Release);
                }
                self.set_raw_input(false);
                uninstall_mouse_hook();
                install_keyboard_hook(); // Keeps only ToggleFocus / ReturnDp active!
                // Restore remembered PC cursor position (for hotkey) or matched edge point (for edge exit)
                let pt = self
                    .saved_pc_cursor
                    .filter(|p| {
                        self.topo
                            .visible_pc_monitors
                            .iter()
                            .any(|m| rect_contains(&m.rect, p.x, p.y))
                    })
                    .unwrap_or(self.topo.return_cursor_pt);
                unsafe {
                    SetCursorPos(pt.x, pt.y);
                }
                self.restore_visible_pc_focus();
                self.apply_standby_edge_or_fence();
            }
        }
    }

    fn apply_standby_edge_or_fence(&mut self) {
        let edge_on = self
            .cfg
            .lock()
            .map(|c| c.edge_switch)
            .unwrap_or(true);
        if edge_on {
            self.release_fence();
            self.show_edge_trigger();
        } else {
            self.hide_edge_trigger();
            if let Some(r) = self.topo.pc_fence_rect {
                unsafe {
                    ClipCursor(&r);
                }
                self.fence_active = true;
            } else {
                self.release_fence();
            }
        }
    }

    fn release_fence(&mut self) {
        if self.fence_active {
            unsafe {
                ClipCursor(ptr::null());
            }
            self.fence_active = false;
        }
    }

    fn show_edge_trigger(&self) {
        show_edge_trigger_window(self.edge_hwnd as HWND, &self.topo);
    }

    fn hide_edge_trigger(&self) {
        hide_edge_trigger_window(self.edge_hwnd as HWND);
    }

    fn capture_last_pc_window(&mut self) {
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() || fg == self.host_hwnd as HWND || fg == self.edge_hwnd as HWND {
                return;
            }
            let on_ghost = self
                .topo
                .tv_ghost_monitor
                .as_ref()
                .map(|m| window_on_rect(fg, &m.rect))
                .unwrap_or(false);
            if !on_ghost {
                self.last_pc_hwnd = fg as usize;
            }
        }
    }

    fn restore_visible_pc_focus(&self) {
        let ghost = self.topo.tv_ghost_monitor.as_ref().map(|m| m.rect);
        unsafe {
            let mut target: HWND = self.last_pc_hwnd as HWND;
            let valid = !target.is_null()
                && IsWindow(target) != 0
                && IsWindowVisible(target) != 0
                && !ghost.map(|g| window_on_rect(target, &g)).unwrap_or(false);
            if !valid {
                target = pick_visible_pc_window(self.edge_hwnd as HWND, ghost)
                    .unwrap_or(ptr::null_mut());
            }
            if !target.is_null() {
                SetForegroundWindow(target);
            }
        }
    }

    fn sync_pc_clipboard_to_tv_if_changed(&mut self) {
        let enabled = self
            .cfg
            .lock()
            .map(|c| c.clipboard_sync)
            .unwrap_or(true);
        if !enabled {
            return;
        }
        let seq = clipboard_sequence();
        if seq == self.last_clip_seq {
            return;
        }
        self.last_clip_seq = seq;
        if let Some(text) = read_pc_clipboard_text() {
            if text != self.last_synced_clip {
                self.last_synced_clip = text.clone();
                if let Ok(g) = self.client.lock() {
                    if let Some(ref c) = *g {
                        c.set_tv_clipboard(&text, false);
                    }
                }
            }
        }
    }

    pub fn on_tv_clipboard_ui(&mut self, text: String) {
        let enabled = self
            .cfg
            .lock()
            .map(|c| c.clipboard_sync)
            .unwrap_or(true);
        if !enabled || text.is_empty() || text == self.last_synced_clip {
            return;
        }
        if let Some(new_seq) = write_pc_clipboard_text(&text) {
            self.last_clip_seq = new_seq;
            self.last_synced_clip = text;
        }
    }

    pub fn start_dp_return_watcher(&self) {
        self.stop_dp_return_watcher();
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut g) = self.watch_stop.lock() {
            *g = Some(Arc::clone(&stop));
        }

        let dev = Arc::clone(&self.dev);
        let adb_path = self.adb_path.clone();
        let addr = self
            .cfg
            .lock()
            .map(|c| c.tv_address.clone())
            .unwrap_or_default();
        spawn_dp_return_watcher(dev, adb_path, addr, self.host_hwnd, stop);
    }

    pub fn stop_dp_return_watcher(&self) {
        if let Ok(mut g) = self.watch_stop.lock() {
            if let Some(flag) = g.take() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn stop(&mut self) {
        self.recovery_gen.fetch_add(1, Ordering::AcqRel);
        self.recovering.store(false, Ordering::Release);
        self.suspended_was_active = false;
        self.teardown_transport_keep_display();
        self.saved_display.restore_if_needed();
    }

    fn set_raw_input(&mut self, enable: bool) {
        if self.raw_input_active == enable {
            return;
        }
        if set_raw_input_device(self.host_hwnd as HWND, enable) {
            self.raw_input_active = enable;
        }
    }
}

fn connect_and_install_client(
    dev: &Arc<DeviceController>,
    adb_path: &PathBuf,
    server_path: PathBuf,
    addr: String,
    host_hwnd: usize,
    client_slot: &Arc<Mutex<Option<Arc<ScrcpyControlClient>>>>,
    prefer_standby: bool,
) -> Result<(), String> {
    let host_clip = host_hwnd;
    let host_exit = host_hwnd;

    let client = ScrcpyControlClient::connect(
        dev,
        adb_path.clone(),
        server_path,
        addr,
        move |text| {
            let boxed = Box::into_raw(Box::new(text)) as LPARAM;
            unsafe {
                PostMessageW(host_clip as HWND, WM_TV_CLIPBOARD, 0, boxed);
            }
        },
        move || unsafe {
            PostMessageW(host_exit as HWND, WM_CONTROL_EXITED, 0, 0);
        },
    )?;

    if let Ok(mut g) = client_slot.lock() {
        *g = Some(Arc::clone(&client));
    }
    if let Ok(mut gg) = GLOBAL_CLIENT.lock() {
        *gg = Some(client);
    }

    unsafe {
        PostMessageW(
            host_hwnd as HWND,
            WM_CONTROL_READY,
            if prefer_standby { 1 } else { 0 } as WPARAM,
            0,
        );
    }
    Ok(())
}

/// Persists an Extend-mode auto-detected physical TV direction into `config.ini` whenever it
/// changes, while leaving single-screen / duplicate fallbacks untouched (0 disk writes when unchanged).
fn persist_detected_edge_side(cfg: &Arc<Mutex<AppConfig>>, topo: &DisplayTopology) -> bool {
    if !topo.edge_trigger_is_ghost {
        return false;
    }
    let Some(side) = topo.edge_side else {
        return false;
    };
    if let Ok(mut c) = cfg.lock() {
        if c.tv_edge_side != side {
            c.tv_edge_side = side;
            c.save();
            diag_log!("TOPO", "auto-saved TvEdgeSide={}", side.as_str());
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::MonitorDesc;
    use windows_sys::Win32::Foundation::RECT;

    fn dummy_topo(is_ghost: bool, side: Option<EdgeSide>) -> DisplayTopology {
        let r = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        DisplayTopology {
            visible_pc_monitors: vec![MonitorDesc {
                is_internal: true,
                rect: r,
                work_area: r,
                dpi_scale: 1.0,
            }],
            tv_ghost_monitor: None,
            edge_side: side,
            edge_trigger_rect: Some(r),
            edge_trigger_is_ghost: is_ghost,
            seam_coord: 0,
            seam_span: (0, 1080),
            return_cursor_pt: POINT { x: 100, y: 100 },
            pc_fence_rect: None,
        }
    }

    #[test]
    fn persist_detected_edge_side_saves_only_on_real_extend_change() {
        let tmp = std::env::temp_dir().join(format!(
            "mikvm_edge_side_test_{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let cfg = Arc::new(Mutex::new(AppConfig::load(tmp.clone())));

        // Single-screen fallback must NOT overwrite config
        assert!(!persist_detected_edge_side(
            &cfg,
            &dummy_topo(false, Some(EdgeSide::Right))
        ));
        assert_eq!(cfg.lock().unwrap().tv_edge_side, EdgeSide::Left);

        // Extend-mode ghost seam detection updates memory AND persists to disk
        assert!(persist_detected_edge_side(
            &cfg,
            &dummy_topo(true, Some(EdgeSide::Right))
        ));
        assert_eq!(cfg.lock().unwrap().tv_edge_side, EdgeSide::Right);
        let reloaded = AppConfig::load(tmp.clone());
        assert_eq!(reloaded.tv_edge_side, EdgeSide::Right);

        // Repeating the same side is a no-op (no unnecessary disk write)
        assert!(!persist_detected_edge_side(
            &cfg,
            &dummy_topo(true, Some(EdgeSide::Right))
        ));

        let _ = std::fs::remove_file(&tmp);
    }
}
