use crate::adb::DeviceController;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const FALLBACK_SCRCPY_VERSION: &str = "3.3.1";
const REMOTE_SERVER_PATH: &str = "/data/local/tmp/scrcpy-server.jar";
const TCP_WRITE_TIMEOUT_MS: u64 = 1200;
const MAX_WRITE_QUEUE_LEN: usize = 256;

// Cached `(server_jar_path, file_size, detected_version)` so repeated connects have 0ms overhead.
static CACHED_SERVER_VER: Mutex<Option<(PathBuf, u64, String)>> = Mutex::new(None);

fn is_valid_version_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

fn parse_scrcpy_cli_version(out: &str) -> Option<String> {
    let first = out.lines().next()?.trim();
    let mut parts = first.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("scrcpy") {
        return None;
    }
    let ver = parts.next()?.trim();
    if is_valid_version_token(ver) {
        Some(ver.to_string())
    } else {
        None
    }
}

fn parse_server_mismatch_version(out: &str) -> Option<String> {
    let marker = "The server version (";
    let start = out.find(marker)? + marker.len();
    let rest = &out[start..];
    let end = rest.find(')')?;
    let ver = rest[..end].trim();
    if is_valid_version_token(ver) {
        Some(ver.to_string())
    } else {
        None
    }
}

fn probe_scrcpy_version_on_device(dev: &Arc<DeviceController>) -> Option<String> {
    let cmd = format!(
        "CLASSPATH={} app_process / com.genymobile.scrcpy.Server 0.0.0 2>&1",
        REMOTE_SERVER_PATH
    );
    let (_, out, err) = dev.shell(&cmd, 2500);
    parse_server_mismatch_version(&out).or_else(|| parse_server_mismatch_version(&err))
}

fn detect_scrcpy_server_version(
    dev: &Arc<DeviceController>,
    server_jar_path: &PathBuf,
    local_size: u64,
    force_device_probe: bool,
) -> String {
    if !force_device_probe {
        if let Ok(g) = CACHED_SERVER_VER.lock() {
            if let Some((ref p, sz, ref v)) = *g {
                if p == server_jar_path && sz == local_size {
                    return v.clone();
                }
            }
        }
        // Fast local detection via sibling `scrcpy.exe --version` (~15ms, works for any scrcpy release)
        if let Some(dir) = server_jar_path.parent() {
            let cli = dir.join("scrcpy.exe");
            if cli.exists() {
                if let Ok(out) = Command::new(&cli)
                    .arg("--version")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if let Some(ver) = parse_scrcpy_cli_version(&text) {
                        if let Ok(mut g) = CACHED_SERVER_VER.lock() {
                            *g = Some((server_jar_path.clone(), local_size, ver.clone()));
                        }
                        return ver;
                    }
                }
            }
        }
    }

    // Universal fallback: ask deployed `scrcpy-server.jar` directly (works even without scrcpy.exe!)
    if let Some(ver) = probe_scrcpy_version_on_device(dev) {
        if let Ok(mut g) = CACHED_SERVER_VER.lock() {
            *g = Some((server_jar_path.clone(), local_size, ver.clone()));
        }
        return ver;
    }

    FALLBACK_SCRCPY_VERSION.to_string()
}

// ControlMessage types (verified from scrcpy-server 5.0.1 classes.dex)
const TYPE_INJECT_KEYCODE: u8 = 0;
const TYPE_SET_CLIPBOARD: u8 = 9;
const TYPE_UHID_CREATE: u8 = 12;
const TYPE_UHID_INPUT: u8 = 13;
const TYPE_UHID_DESTROY: u8 = 14;

// DeviceMessage types (verified from scrcpy-server 5.0.1 classes.dex)
const DEV_TYPE_CLIPBOARD: u8 = 0;
const DEV_TYPE_ACK_CLIPBOARD: u8 = 1;
const DEV_TYPE_UHID_OUTPUT: u8 = 2;

const UHID_MOUSE_ID: u16 = 1;
const CLIPBOARD_MAX_BYTES: usize = 262_130;

// Official 67-byte HID relative mouse descriptor extracted from scrcpy 5.0.1:
// 5 buttons (1 byte) + X (i8) + Y (i8) + VWheel (i8) + HWheel (i8) = 5-byte HID report
const HID_MOUSE_REPORT_DESC: [u8; 67] = [
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81,
    0x06, 0xC0, 0xC0,
];

pub const ACTION_DOWN: u8 = 0;
pub const ACTION_UP: u8 = 1;

pub const MOUSE_BTN_LEFT: u8 = 1 << 0;
pub const MOUSE_BTN_RIGHT: u8 = 1 << 1;

// Physical modifier bitmask tracked inside WH_KEYBOARD_LL (needed because swallowing
// modifier keys in a low-level hook prevents Windows from updating GetAsyncKeyState!)
pub const HMOD_LSHIFT: u16 = 1 << 0;
pub const HMOD_RSHIFT: u16 = 1 << 1;
pub const HMOD_LCTRL: u16 = 1 << 2;
pub const HMOD_RCTRL: u16 = 1 << 3;
pub const HMOD_LALT: u16 = 1 << 4;
pub const HMOD_RALT: u16 = 1 << 5;
pub const HMOD_LWIN: u16 = 1 << 6;
pub const HMOD_RWIN: u16 = 1 << 7;

// Android KeyEvent.META_* masks
pub const AMETA_SHIFT_ON: i32 = 0x0000_0001;
pub const AMETA_ALT_ON: i32 = 0x0000_0002;
pub const AMETA_ALT_LEFT_ON: i32 = 0x0000_0010;
pub const AMETA_ALT_RIGHT_ON: i32 = 0x0000_0020;
pub const AMETA_SHIFT_LEFT_ON: i32 = 0x0000_0040;
pub const AMETA_SHIFT_RIGHT_ON: i32 = 0x0000_0080;
pub const AMETA_CTRL_ON: i32 = 0x0000_1000;
pub const AMETA_CTRL_LEFT_ON: i32 = 0x0000_2000;
pub const AMETA_CTRL_RIGHT_ON: i32 = 0x0000_4000;
pub const AMETA_META_ON: i32 = 0x0001_0000;
pub const AMETA_META_LEFT_ON: i32 = 0x0002_0000;
pub const AMETA_META_RIGHT_ON: i32 = 0x0004_0000;
pub const AMETA_CAPS_LOCK_ON: i32 = 0x0010_0000;
pub const AMETA_NUM_LOCK_ON: i32 = 0x0020_0000;

enum WriteCmd {
    MouseMove(i32, i32),
    MouseBurst(i32, i32),
    PrimedMouseBurst {
        prime_dx: i32,
        prime_dy: i32,
        dx: i32,
        dy: i32,
    },
    MouseButton {
        mask: u8,
        down: bool,
    },
    Keycode {
        action: u8,
        keycode: i32,
        repeat: i32,
        metastate: i32,
    },
    TapKeycode(i32),
    ReleaseAllInputs,
    Clipboard {
        text: String,
        paste: bool,
    },
    Shutdown,
}

#[derive(Default)]
struct WriteQueueState {
    items: VecDeque<WriteCmd>,
    closed: bool,
}

struct WriteQueue {
    state: Mutex<WriteQueueState>,
    cv: Condvar,
}

impl WriteQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(WriteQueueState {
                items: VecDeque::with_capacity(64),
                closed: false,
            }),
            cv: Condvar::new(),
        }
    }

    /// Enqueues a write command without EVER blocking on TCP I/O.
    /// Consecutive `MouseMove` deltas coalesce in place to keep latency ultra-low.
    fn push(&self, cmd: WriteCmd) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        if st.closed {
            return false;
        }
        if let WriteCmd::MouseMove(dx, dy) = cmd {
            if let Some(WriteCmd::MouseMove(ref mut pdx, ref mut pdy)) = st.items.back_mut() {
                *pdx = pdx.saturating_add(dx);
                *pdy = pdy.saturating_add(dy);
                return true;
            }
        }
        if st.items.len() >= MAX_WRITE_QUEUE_LEN {
            return false;
        }
        st.items.push_back(cmd);
        self.cv.notify_one();
        true
    }

    fn request_shutdown(&self) {
        if let Ok(mut st) = self.state.lock() {
            if !st.closed {
                st.closed = true;
                st.items.clear();
                st.items.push_back(WriteCmd::Shutdown);
                self.cv.notify_one();
            }
        }
    }

    fn pop_blocking(&self) -> Option<WriteCmd> {
        let mut st = self.state.lock().ok()?;
        loop {
            if let Some(cmd) = st.items.pop_front() {
                return Some(cmd);
            }
            if st.closed {
                return None;
            }
            st = self.cv.wait(st).ok()?;
        }
    }
}

/// Coordinates exact-once disconnect notifications across both the writer and reader threads,
/// preventing write errors from silently swallowing `on_disconnect()` (`WM_CONTROL_EXITED`).
struct DisconnectNotifier {
    alive: AtomicBool,
    closed_by_user: AtomicBool,
    on_disconnect: Box<dyn Fn() + Send + Sync>,
}

impl DisconnectNotifier {
    fn new<F: Fn() + Send + Sync + 'static>(on_disconnect: F) -> Self {
        Self {
            alive: AtomicBool::new(true),
            closed_by_user: AtomicBool::new(false),
            on_disconnect: Box::new(on_disconnect),
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn notify_unexpected_disconnect(&self, reason: &str) {
        if self.alive.swap(false, Ordering::AcqRel) {
            let user_closed = self.closed_by_user.load(Ordering::Acquire);
            diag_log!(
                "SCRCPY",
                "channel disconnected reason={} user_closed={}",
                reason,
                user_closed
            );
            if !user_closed {
                (self.on_disconnect)();
            }
        }
    }

    fn mark_user_closed(&self) -> bool {
        self.closed_by_user.store(true, Ordering::Release);
        self.alive.swap(false, Ordering::AcqRel)
    }
}

pub struct ScrcpyControlClient {
    adb_path: PathBuf,
    tv_addr: String,
    local_port: u16,
    server_child: Arc<Mutex<Option<Child>>>,
    socket: Arc<TcpStream>,
    queue: Arc<WriteQueue>,
    notifier: Arc<DisconnectNotifier>,
}

impl ScrcpyControlClient {
    pub fn connect<FClip, FExit>(
        dev: &Arc<DeviceController>,
        adb_path: PathBuf,
        server_jar_path: PathBuf,
        tv_addr: String,
        on_clipboard: FClip,
        on_disconnect: FExit,
    ) -> Result<Arc<Self>, String>
    where
        FClip: Fn(String) + Send + 'static,
        FExit: Fn() + Send + Sync + 'static,
    {
        if !server_jar_path.exists() {
            return Err(
                "未找到 scrcpy-server：请确保同级 scrcpy/ 目录下存在 scrcpy-server 文件"
                    .to_string(),
            );
        }
        if !adb_path.exists() {
            return Err("未找到 adb.exe：请检查 scrcpy/ 目录".to_string());
        }

        // 1. Deploy scrcpy-server.jar only if missing or size differs
        let local_size = std::fs::metadata(&server_jar_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let (_, size_out, _) = dev.shell(
            &format!("stat -c %s {} 2>/dev/null || wc -c < {} 2>/dev/null", REMOTE_SERVER_PATH, REMOTE_SERVER_PATH),
            2500,
        );
        let remote_size = size_out.trim().parse::<u64>().unwrap_or(0);
        if remote_size != local_size || local_size == 0 {
            if let Some(p_str) = server_jar_path.to_str() {
                let (rc, _, err) = dev.run_adb(&["-s", &tv_addr, "push", p_str, REMOTE_SERVER_PATH], 8000);
                if rc != 0 {
                    return Err(format!("推送 scrcpy-server 失败: {}", err.trim()));
                }
            }
        }

        // 2. Pick a free ephemeral local TCP port and set up adb forward
        let local_port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("无法分配本地端口: {}", e))?;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(12345);
        let scid = (nanos ^ std::process::id().wrapping_mul(2654435761)) & 0x7FFF_FFFF;
        let scid_hex = format!("{:08x}", scid);
        let local_spec = format!("tcp:{}", local_port);
        let remote_spec = format!("localabstract:scrcpy_{}", scid_hex);

        let (fwd_rc, _, fwd_err) = dev.run_adb(&["-s", &tv_addr, "forward", &local_spec, &remote_spec], 3000);
        if fwd_rc != 0 {
            return Err(format!("adb forward 失败: {}", fwd_err.trim()));
        }

        // 3. Auto-detect exact scrcpy-server version and launch control-only server (retry once via
        //    on-device version probe if sibling scrcpy.exe version ever disagrees with scrcpy-server)
        let mut server_ver = detect_scrcpy_server_version(dev, &server_jar_path, local_size, false);
        diag_log!("SCRCPY", "connect addr={} ver={} port={}", tv_addr, server_ver, local_port);
        let mut stream_opt: Option<TcpStream> = None;
        let mut child_opt: Option<Child> = None;

        for attempt in 0..2 {
            let shell_cmd = format!(
                "CLASSPATH={} app_process / com.genymobile.scrcpy.Server {} scid={} log_level=warn video=false audio=false control=true tunnel_forward=true send_device_meta=false send_dummy_byte=true cleanup=false clipboard_autosync=true",
                REMOTE_SERVER_PATH, server_ver, scid_hex
            );
            let mut child = Command::new(&adb_path)
                .args(["-s", &tv_addr, "shell", &shell_cmd])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| format!("无法启动 scrcpy-server: {}", e))?;

            // 4. Handshake: retry connecting until dummy byte 0x00 arrives from server
            let deadline = Instant::now() + Duration::from_millis(4500);
            let mut exited_early = false;

            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    exited_early = true;
                    break;
                }
                if let Ok(mut st) = TcpStream::connect(("127.0.0.1", local_port)) {
                    let _ = st.set_nodelay(true);
                    let _ = st.set_read_timeout(Some(Duration::from_millis(250)));
                    let mut dummy = [0u8; 1];
                    match st.read(&mut dummy) {
                        Ok(1) => {
                            let _ = st.set_read_timeout(None);
                            stream_opt = Some(st);
                            break;
                        }
                        _ => {
                            drop(st);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(40));
            }

            if stream_opt.is_some() {
                child_opt = Some(child);
                break;
            }

            let _ = child.kill();
            let _ = child.wait();

            if exited_early && attempt == 0 {
                let probed = detect_scrcpy_server_version(dev, &server_jar_path, local_size, true);
                if probed != server_ver {
                    server_ver = probed;
                    continue;
                }
                let _ = dev.run_adb(&["-s", &tv_addr, "forward", "--remove", &local_spec], 1500);
                return Err("scrcpy-server 异常退出，请确认电视 ADB 正常连接".to_string());
            }
            break;
        }

        let (Some(mut stream), Some(child)) = (stream_opt, child_opt) else {
            let _ = dev.run_adb(&["-s", &tv_addr, "forward", "--remove", &local_spec], 1500);
            return Err("连接 scrcpy-server 控制通道超时".to_string());
        };

        // 5. Set bounded write timeout BEFORE registering UHID mouse so stalled ADB loopback
        //    sockets fail fast (1.2s) instead of hanging in TCP Zero-Window Probing for minutes!
        let _ = stream.set_write_timeout(Some(Duration::from_millis(TCP_WRITE_TIMEOUT_MS)));
        let uhid_pkt = build_uhid_create_packet();
        if stream.write_all(&uhid_pkt).is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            let mut ch = child;
            let _ = ch.kill();
            let _ = ch.wait();
            let _ = dev.run_adb(&["-s", &tv_addr, "forward", "--remove", &local_spec], 1500);
            return Err("初始化电视虚拟 UHID 鼠标失败".to_string());
        }

        let socket = Arc::new(stream);
        let queue = Arc::new(WriteQueue::new());
        let notifier = Arc::new(DisconnectNotifier::new(on_disconnect));
        let server_child = Arc::new(Mutex::new(Some(child)));

        let client = Arc::new(Self {
            adb_path,
            tv_addr,
            local_port,
            server_child,
            socket: Arc::clone(&socket),
            queue: Arc::clone(&queue),
            notifier: Arc::clone(&notifier),
        });

        // 6. Spawn dedicated non-blocking TCP writer thread (keeps WH_KEYBOARD_LL / WH_MOUSE_LL 100% I/O-free!)
        {
            let sock_w = Arc::clone(&socket);
            let q_w = Arc::clone(&queue);
            let notif_w = Arc::clone(&notifier);
            std::thread::spawn(move || {
                device_writer_loop(sock_w, q_w, notif_w);
            });
        }

        // 7. Spawn background reader thread for device messages (clipboard / disconnect)
        {
            let sock_r = Arc::clone(&socket);
            let notif_r = Arc::clone(&notifier);
            let q_r = Arc::clone(&queue);
            std::thread::spawn(move || {
                device_reader_loop(sock_r, q_r, notif_r, on_clipboard);
            });
        }

        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.notifier.is_alive()
    }

    /// Sends relative mouse movement asynchronously (100% non-blocking; coalesces bursts).
    pub fn send_mouse_move(&self, dx: i32, dy: i32) {
        if (dx == 0 && dy == 0) || !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::MouseMove(dx, dy));
    }

    /// Sends an entire multi-packet relative mouse move in ONE single TCP write on the writer thread.
    pub fn send_mouse_burst(&self, dx: i32, dy: i32) {
        if (dx == 0 && dy == 0) || !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::MouseBurst(dx, dy));
    }

    /// Sends a velocity-primed single-frame burst in ONE TCP write on the writer thread:
    /// Packet 1 = `(prime_dx, prime_dy)` pins the cursor to the edge AND primes Android's
    /// VelocityControl (eliminating the 1x->3x non-linear cliff after >200ms idle!),
    /// followed immediately by `(dx, dy)` packets that scale linearly at exact `acceleration` (3.0x),
    /// achieving +/-1px accuracy across the entire TV screen in a single VSync frame!
    pub fn send_primed_mouse_burst(
        &self,
        prime_dx: i32,
        prime_dy: i32,
        dx: i32,
        dy: i32,
    ) {
        if !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::PrimedMouseBurst {
            prime_dx,
            prime_dy,
            dx,
            dy,
        });
    }

    pub fn send_mouse_button(&self, mask: u8, down: bool) {
        if !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::MouseButton { mask, down });
    }

    pub fn send_keycode(&self, action: u8, keycode: i32, repeat: i32, metastate: i32) {
        if !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::Keycode {
            action,
            keycode,
            repeat,
            metastate,
        });
    }

    /// Sends a quick down+up tap for a single Android keycode (e.g. Back / Home / Menu / Vol).
    pub fn tap_keycode(&self, keycode: i32) {
        if !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::TapKeycode(keycode));
    }

    /// Releases all currently held mouse buttons and Android keys so leaving TV focus never causes stuck keys!
    pub fn release_all_inputs(&self) {
        if !self.notifier.is_alive() {
            return;
        }
        let _ = self.queue.push(WriteCmd::ReleaseAllInputs);
    }

    pub fn set_tv_clipboard(&self, text: &str, paste: bool) {
        if !self.notifier.is_alive() || text.is_empty() {
            return;
        }
        let _ = self.queue.push(WriteCmd::Clipboard {
            text: text.to_string(),
            paste,
        });
    }

    /// Non-blocking teardown safe to call directly on the UI thread (<0.1ms):
    /// signals the writer thread to release inputs & destroy UHID, then finishes socket shutdown,
    /// child kill, and `adb forward --remove` asynchronously on a short-lived background thread.
    pub fn close(&self) {
        let was_alive = self.notifier.mark_user_closed();
        self.queue.request_shutdown();

        let socket = Arc::clone(&self.socket);
        let child_slot = Arc::clone(&self.server_child);
        let adb_path = self.adb_path.clone();
        let tv_addr = self.tv_addr.clone();
        let local_port = self.local_port;

        std::thread::spawn(move || {
            if was_alive {
                // Give the writer thread a brief window to flush release + UHID_DESTROY cleanly
                std::thread::sleep(Duration::from_millis(45));
            }
            let _ = socket.shutdown(Shutdown::Both);

            if let Ok(mut cg) = child_slot.lock() {
                if let Some(mut ch) = cg.take() {
                    let _ = ch.kill();
                    let _ = ch.wait();
                }
            }

            let local_spec = format!("tcp:{}", local_port);
            if let Ok(mut fwd) = Command::new(&adb_path)
                .args(["-s", &tv_addr, "forward", "--remove", &local_spec])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                let deadline = Instant::now() + Duration::from_millis(2000);
                loop {
                    match fwd.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => {
                            if Instant::now() >= deadline {
                                let _ = fwd.kill();
                                let _ = fwd.wait();
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
            }
        });
    }
}

fn build_uhid_create_packet() -> Vec<u8> {
    let name = b"MiKVM Mouse";
    let mut buf = Vec::with_capacity(1 + 2 + 2 + 2 + 1 + name.len() + 2 + HID_MOUSE_REPORT_DESC.len());
    buf.push(TYPE_UHID_CREATE);
    buf.extend_from_slice(&UHID_MOUSE_ID.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes()); // vendor_id
    buf.extend_from_slice(&0u16.to_be_bytes()); // product_id
    buf.push(name.len() as u8);
    buf.extend_from_slice(name);
    buf.extend_from_slice(&(HID_MOUSE_REPORT_DESC.len() as u16).to_be_bytes());
    buf.extend_from_slice(&HID_MOUSE_REPORT_DESC);
    buf
}

fn device_writer_loop(
    socket: Arc<TcpStream>,
    queue: Arc<WriteQueue>,
    notifier: Arc<DisconnectNotifier>,
) {
    let mut buttons: u8 = 0;
    let mut pressed_keys: Vec<i32> = Vec::with_capacity(16);
    let clip_seq = AtomicU64::new(1);

    let write_bytes = |data: &[u8]| -> bool {
        if data.is_empty() {
            return true;
        }
        let mut s = &*socket;
        match s.write_all(data) {
            Ok(()) => true,
            Err(e) => {
                let reason = format!("write_err:{:?}", e.kind());
                let _ = socket.shutdown(Shutdown::Both);
                queue.request_shutdown();
                notifier.notify_unexpected_disconnect(&reason);
                false
            }
        }
    };

    while let Some(cmd) = queue.pop_blocking() {
        match cmd {
            WriteCmd::MouseMove(mut dx, mut dy) => {
                if !notifier.is_alive() {
                    continue;
                }
                let steps = ((dx.abs().max(dy.abs()) + 126) / 127).max(1) as usize;
                let mut buf = Vec::with_capacity(steps * 10);
                while dx != 0 || dy != 0 {
                    let sx = dx.clamp(-127, 127) as i8;
                    let sy = dy.clamp(-127, 127) as i8;
                    dx -= sx as i32;
                    dy -= sy as i32;
                    buf.extend_from_slice(&build_uhid_mouse_packet(buttons, sx, sy, 0, 0));
                }
                if !write_bytes(&buf) {
                    break;
                }
            }
            WriteCmd::MouseBurst(mut dx, mut dy) => {
                if !notifier.is_alive() {
                    continue;
                }
                let steps = ((dx.abs().max(dy.abs()) + 126) / 127).max(1) as usize;
                let mut buf = Vec::with_capacity(steps * 10);
                while dx != 0 || dy != 0 {
                    let sx = dx.clamp(-127, 127) as i8;
                    let sy = dy.clamp(-127, 127) as i8;
                    dx -= sx as i32;
                    dy -= sy as i32;
                    buf.extend_from_slice(&build_uhid_mouse_packet(buttons, sx, sy, 0, 0));
                }
                if !write_bytes(&buf) {
                    break;
                }
            }
            WriteCmd::PrimedMouseBurst {
                prime_dx,
                prime_dy,
                mut dx,
                mut dy,
            } => {
                if !notifier.is_alive() {
                    continue;
                }
                let p_steps = ((prime_dx.abs().max(prime_dy.abs()) + 126) / 127).max(1) as usize;
                let m_steps = ((dx.abs().max(dy.abs()) + 126) / 127) as usize;
                let mut buf = Vec::with_capacity((p_steps + m_steps) * 10);
                let mut pdx = prime_dx;
                let mut pdy = prime_dy;
                while pdx != 0 || pdy != 0 {
                    let sx = pdx.clamp(-127, 127) as i8;
                    let sy = pdy.clamp(-127, 127) as i8;
                    pdx -= sx as i32;
                    pdy -= sy as i32;
                    buf.extend_from_slice(&build_uhid_mouse_packet(buttons, sx, sy, 0, 0));
                }
                while dx != 0 || dy != 0 {
                    let sx = dx.clamp(-127, 127) as i8;
                    let sy = dy.clamp(-127, 127) as i8;
                    dx -= sx as i32;
                    dy -= sy as i32;
                    buf.extend_from_slice(&build_uhid_mouse_packet(buttons, sx, sy, 0, 0));
                }
                if !write_bytes(&buf) {
                    break;
                }
            }
            WriteCmd::MouseButton { mask, down } => {
                if !notifier.is_alive() {
                    continue;
                }
                if down {
                    buttons |= mask;
                } else {
                    buttons &= !mask;
                }
                let pkt = build_uhid_mouse_packet(buttons, 0, 0, 0, 0);
                if !write_bytes(&pkt) {
                    break;
                }
            }
            WriteCmd::Keycode {
                action,
                keycode,
                repeat,
                metastate,
            } => {
                if !notifier.is_alive() {
                    continue;
                }
                if action == ACTION_DOWN {
                    if !pressed_keys.contains(&keycode) {
                        pressed_keys.push(keycode);
                    }
                } else {
                    pressed_keys.retain(|&k| k != keycode);
                }
                let pkt = build_inject_keycode_packet(action, keycode, repeat, metastate);
                if !write_bytes(&pkt) {
                    break;
                }
            }
            WriteCmd::TapKeycode(keycode) => {
                if !notifier.is_alive() {
                    continue;
                }
                let down = build_inject_keycode_packet(ACTION_DOWN, keycode, 0, 0);
                let up = build_inject_keycode_packet(ACTION_UP, keycode, 0, 0);
                let mut both = [0u8; 28];
                both[..14].copy_from_slice(&down);
                both[14..].copy_from_slice(&up);
                if !write_bytes(&both) {
                    break;
                }
            }
            WriteCmd::ReleaseAllInputs => {
                if !notifier.is_alive() {
                    continue;
                }
                let mut buf = Vec::with_capacity(10 + pressed_keys.len() * 14);
                if buttons != 0 {
                    buttons = 0;
                    buf.extend_from_slice(&build_uhid_mouse_packet(0, 0, 0, 0, 0));
                }
                for kc in pressed_keys.drain(..) {
                    buf.extend_from_slice(&build_inject_keycode_packet(ACTION_UP, kc, 0, 0));
                }
                if !write_bytes(&buf) {
                    break;
                }
            }
            WriteCmd::Clipboard { text, paste } => {
                if !notifier.is_alive() || text.is_empty() {
                    continue;
                }
                let mut cut = text.len().min(CLIPBOARD_MAX_BYTES);
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                let bytes = &text.as_bytes()[..cut];
                let seq = clip_seq.fetch_add(1, Ordering::Relaxed);
                let mut buf = Vec::with_capacity(1 + 8 + 1 + 4 + bytes.len());
                buf.push(TYPE_SET_CLIPBOARD);
                buf.extend_from_slice(&seq.to_be_bytes());
                buf.push(if paste { 1 } else { 0 });
                buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                buf.extend_from_slice(bytes);
                if !write_bytes(&buf) {
                    break;
                }
            }
            WriteCmd::Shutdown => {
                let _ = socket.set_write_timeout(Some(Duration::from_millis(150)));
                let mut buf = Vec::with_capacity(13 + pressed_keys.len() * 14);
                if buttons != 0 {
                    buf.extend_from_slice(&build_uhid_mouse_packet(0, 0, 0, 0, 0));
                }
                for kc in pressed_keys.drain(..) {
                    buf.extend_from_slice(&build_inject_keycode_packet(ACTION_UP, kc, 0, 0));
                }
                buf.push(TYPE_UHID_DESTROY);
                buf.extend_from_slice(&UHID_MOUSE_ID.to_be_bytes());
                let mut s = &*socket;
                let _ = s.write_all(&buf);
                let _ = s.flush();
                let _ = socket.shutdown(Shutdown::Both);
                break;
            }
        }
    }
}

fn build_uhid_mouse_packet(buttons: u8, dx: i8, dy: i8, vwheel: i8, hwheel: i8) -> [u8; 10] {
    let mut pkt = [0u8; 10];
    pkt[0] = TYPE_UHID_INPUT;
    pkt[1..3].copy_from_slice(&UHID_MOUSE_ID.to_be_bytes());
    pkt[3..5].copy_from_slice(&5u16.to_be_bytes());
    pkt[5] = buttons;
    pkt[6] = dx as u8;
    pkt[7] = dy as u8;
    pkt[8] = vwheel as u8;
    pkt[9] = hwheel as u8;
    pkt
}

fn build_inject_keycode_packet(action: u8, keycode: i32, repeat: i32, metastate: i32) -> [u8; 14] {
    let mut pkt = [0u8; 14];
    pkt[0] = TYPE_INJECT_KEYCODE;
    pkt[1] = action;
    pkt[2..6].copy_from_slice(&keycode.to_be_bytes());
    pkt[6..10].copy_from_slice(&repeat.to_be_bytes());
    pkt[10..14].copy_from_slice(&metastate.to_be_bytes());
    pkt
}

fn device_reader_loop<FClip>(
    socket: Arc<TcpStream>,
    queue: Arc<WriteQueue>,
    notifier: Arc<DisconnectNotifier>,
    on_clipboard: FClip,
) where
    FClip: Fn(String),
{
    let mut reader = &*socket;
    let mut hdr = [0u8; 1];
    let mut reason = "read_eof";
    while notifier.is_alive() {
        if let Err(e) = reader.read_exact(&mut hdr) {
            reason = if e.kind() == std::io::ErrorKind::UnexpectedEof {
                "read_eof"
            } else {
                "read_err"
            };
            break;
        }
        match hdr[0] {
            DEV_TYPE_CLIPBOARD => {
                let mut len_buf = [0u8; 4];
                if reader.read_exact(&mut len_buf).is_err() {
                    reason = "read_clip_len_err";
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > 512 * 1024 {
                    reason = "read_clip_oversize";
                    break;
                }
                let mut data = vec![0u8; len];
                if reader.read_exact(&mut data).is_err() {
                    reason = "read_clip_body_err";
                    break;
                }
                if let Ok(text) = String::from_utf8(data) {
                    if !text.is_empty() {
                        on_clipboard(text);
                    }
                }
            }
            DEV_TYPE_ACK_CLIPBOARD => {
                let mut seq_buf = [0u8; 8];
                if reader.read_exact(&mut seq_buf).is_err() {
                    reason = "read_ack_err";
                    break;
                }
            }
            DEV_TYPE_UHID_OUTPUT => {
                let mut info = [0u8; 4];
                if reader.read_exact(&mut info).is_err() {
                    reason = "read_uhid_hdr_err";
                    break;
                }
                let data_len = u16::from_be_bytes([info[2], info[3]]) as usize;
                let mut skip = vec![0u8; data_len];
                if reader.read_exact(&mut skip).is_err() {
                    reason = "read_uhid_body_err";
                    break;
                }
            }
            _ => {
                reason = "read_unknown_msg";
                break;
            }
        }
    }
    let _ = socket.shutdown(Shutdown::Both);
    queue.request_shutdown();
    notifier.notify_unexpected_disconnect(reason);
}

/// Converts a Win32 Virtual-Key code + extended flag to Android KeyEvent keycode.
pub fn vk_to_android_keycode(vk: u16, extended: bool) -> Option<i32> {
    if (b'A' as u16..=b'Z' as u16).contains(&vk) {
        return Some(29 + (vk - b'A' as u16) as i32); // KEYCODE_A = 29
    }
    if (b'0' as u16..=b'9' as u16).contains(&vk) {
        return Some(7 + (vk - b'0' as u16) as i32); // KEYCODE_0 = 7
    }
    if (VK_NUMPAD0..=VK_NUMPAD9).contains(&vk) {
        return Some(144 + (vk - VK_NUMPAD0) as i32); // KEYCODE_NUMPAD_0 = 144
    }
    if (VK_F1..=VK_F12).contains(&vk) {
        return Some(131 + (vk - VK_F1) as i32); // KEYCODE_F1 = 131
    }
    let kc = match vk {
        VK_RETURN => if extended { 160 } else { 66 }, // NUMPAD_ENTER : ENTER
        VK_ESCAPE => 111,
        VK_BACK => 67,      // KEYCODE_DEL
        VK_DELETE => 112,   // KEYCODE_FORWARD_DEL
        VK_TAB => 61,
        VK_SPACE => 62,
        VK_UP => 19,
        VK_DOWN => 20,
        VK_LEFT => 21,
        VK_RIGHT => 22,
        VK_HOME => 122,     // KEYCODE_MOVE_HOME
        VK_END => 123,      // KEYCODE_MOVE_END
        VK_PRIOR => 92,     // KEYCODE_PAGE_UP
        VK_NEXT => 93,      // KEYCODE_PAGE_DOWN
        VK_INSERT => 124,
        VK_CAPITAL => 115,
        VK_NUMLOCK => 143,
        VK_SCROLL => 116,
        VK_PAUSE => 121,
        VK_SNAPSHOT => 120,
        VK_APPS => 82,
        VK_LSHIFT | VK_SHIFT => 59,
        VK_RSHIFT => 60,
        VK_LCONTROL | VK_CONTROL => 113,
        VK_RCONTROL => 114,
        VK_LMENU | VK_MENU => 57,
        VK_RMENU => 58,
        VK_LWIN => 117,
        VK_RWIN => 118,
        VK_OEM_MINUS => 69,
        VK_OEM_PLUS => 70,
        VK_OEM_4 => 71,     // [
        VK_OEM_6 => 72,     // ]
        VK_OEM_5 => 73,     // \
        VK_OEM_1 => 74,     // ;
        VK_OEM_7 => 75,     // '
        VK_OEM_COMMA => 55, // ,
        VK_OEM_PERIOD => 56,// .
        VK_OEM_2 => 76,     // /
        VK_OEM_3 => 68,     // `
        VK_MULTIPLY => 155,
        VK_ADD => 157,
        VK_SUBTRACT => 156,
        VK_DECIMAL => 158,
        VK_DIVIDE => 154,
        VK_VOLUME_UP => 24,
        VK_VOLUME_DOWN => 25,
        VK_VOLUME_MUTE => 164,
        VK_MEDIA_PLAY_PAUSE => 85,
        VK_MEDIA_STOP => 86,
        VK_MEDIA_NEXT_TRACK => 87,
        VK_MEDIA_PREV_TRACK => 88,
        _ => return None,
    };
    Some(kc)
}

/// Computes Android KeyEvent metastate bitmask strictly from our own tracked held-modifier bits
/// (isolated from Windows GetAsyncKeyState so a modifier held during PC->TV switch never leaks!).
pub unsafe fn android_metastate_from_held(held: u16) -> i32 {
    let mut m = 0i32;
    if (held & HMOD_LSHIFT) != 0 {
        m |= AMETA_SHIFT_ON | AMETA_SHIFT_LEFT_ON;
    }
    if (held & HMOD_RSHIFT) != 0 {
        m |= AMETA_SHIFT_ON | AMETA_SHIFT_RIGHT_ON;
    }
    if (held & HMOD_LCTRL) != 0 {
        m |= AMETA_CTRL_ON | AMETA_CTRL_LEFT_ON;
    }
    if (held & HMOD_RCTRL) != 0 {
        m |= AMETA_CTRL_ON | AMETA_CTRL_RIGHT_ON;
    }
    if (held & HMOD_LALT) != 0 {
        m |= AMETA_ALT_ON | AMETA_ALT_LEFT_ON;
    }
    if (held & HMOD_RALT) != 0 {
        m |= AMETA_ALT_ON | AMETA_ALT_RIGHT_ON;
    }
    if (held & HMOD_LWIN) != 0 {
        m |= AMETA_META_ON | AMETA_META_LEFT_ON;
    }
    if (held & HMOD_RWIN) != 0 {
        m |= AMETA_META_ON | AMETA_META_RIGHT_ON;
    }
    if (GetKeyState(VK_CAPITAL as i32) & 1) != 0 {
        m |= AMETA_CAPS_LOCK_ON;
    }
    if (GetKeyState(VK_NUMLOCK as i32) & 1) != 0 {
        m |= AMETA_NUM_LOCK_ON;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_token_validation() {
        assert!(is_valid_version_token("3.3.1"));
        assert!(is_valid_version_token("5.0.1"));
        assert!(is_valid_version_token("3.2-rc1"));
        assert!(!is_valid_version_token(""));
        assert!(!is_valid_version_token("v3.3.1"));
        assert!(!is_valid_version_token("3.3 1"));
    }

    #[test]
    fn parse_cli_and_mismatch_versions() {
        let cli = "scrcpy 5.0.1 <https://github.com/Genymobile/scrcpy>\n\nDependencies:";
        assert_eq!(parse_scrcpy_cli_version(cli).as_deref(), Some("5.0.1"));
        assert_eq!(parse_scrcpy_cli_version("scrcpy 3.3.1\n").as_deref(), Some("3.3.1"));
        assert_eq!(parse_scrcpy_cli_version("not scrcpy"), None);

        let mismatch = "ERROR: The server version (3.3.1) does not match the client (0.0.0)";
        assert_eq!(parse_server_mismatch_version(mismatch).as_deref(), Some("3.3.1"));
        assert_eq!(parse_server_mismatch_version("unrelated stderr"), None);
    }

    #[test]
    fn uhid_mouse_and_keycode_packets() {
        let m = build_uhid_mouse_packet(MOUSE_BTN_LEFT | MOUSE_BTN_RIGHT, -127, -1, 1, -2);
        assert_eq!(m[0], TYPE_UHID_INPUT);
        assert_eq!(&m[1..3], &UHID_MOUSE_ID.to_be_bytes());
        assert_eq!(&m[3..5], &5u16.to_be_bytes());
        assert_eq!(m[5], 0x03);
        assert_eq!(m[6], 0x81); // -127i8
        assert_eq!(m[7], 0xFF); // -1i8
        assert_eq!(m[8], 0x01);
        assert_eq!(m[9], 0xFE); // -2i8

        let k = build_inject_keycode_packet(ACTION_DOWN, 66, 0, AMETA_CTRL_ON | AMETA_CTRL_LEFT_ON);
        assert_eq!(k[0], TYPE_INJECT_KEYCODE);
        assert_eq!(k[1], ACTION_DOWN);
        assert_eq!(i32::from_be_bytes([k[2], k[3], k[4], k[5]]), 66);
        assert_eq!(i32::from_be_bytes([k[6], k[7], k[8], k[9]]), 0);
        assert_eq!(
            i32::from_be_bytes([k[10], k[11], k[12], k[13]]),
            AMETA_CTRL_ON | AMETA_CTRL_LEFT_ON
        );
    }

    #[test]
    fn vk_to_android_mapping_essentials() {
        assert_eq!(vk_to_android_keycode(b'A' as u16, false), Some(29));
        assert_eq!(vk_to_android_keycode(b'Z' as u16, false), Some(54));
        assert_eq!(vk_to_android_keycode(b'0' as u16, false), Some(7));
        assert_eq!(vk_to_android_keycode(VK_F12, false), Some(142));
        assert_eq!(vk_to_android_keycode(VK_RETURN, false), Some(66));
        assert_eq!(vk_to_android_keycode(VK_RETURN, true), Some(160));
        assert_eq!(vk_to_android_keycode(VK_ESCAPE, false), Some(111));
        assert_eq!(vk_to_android_keycode(VK_BACK, false), Some(67));
        assert_eq!(vk_to_android_keycode(0xFFFF, false), None);
    }

    #[test]
    fn disconnect_notifier_fires_once_and_respects_user_close() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c1 = Arc::clone(&count);
        let n = DisconnectNotifier::new(move || {
            c1.fetch_add(1, Ordering::SeqCst);
        });
        assert!(n.is_alive());
        n.notify_unexpected_disconnect("write_err");
        assert!(!n.is_alive());
        n.notify_unexpected_disconnect("read_eof");
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let count2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = Arc::clone(&count2);
        let n2 = DisconnectNotifier::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
        });
        assert!(n2.mark_user_closed());
        assert!(!n2.is_alive());
        n2.notify_unexpected_disconnect("read_eof");
        assert_eq!(count2.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn write_queue_coalesces_mouse_moves_and_bounds_depth() {
        let q = WriteQueue::new();
        assert!(q.push(WriteCmd::MouseMove(10, -5)));
        assert!(q.push(WriteCmd::MouseMove(7, 12)));
        assert!(q.push(WriteCmd::TapKeycode(4)));

        match q.pop_blocking() {
            Some(WriteCmd::MouseMove(dx, dy)) => {
                assert_eq!((dx, dy), (17, 7));
            }
            _ => panic!("expected coalesced MouseMove"),
        }
        assert!(matches!(q.pop_blocking(), Some(WriteCmd::TapKeycode(4))));

        // Pushing beyond MAX_WRITE_QUEUE_LEN must never block
        for _ in 0..MAX_WRITE_QUEUE_LEN {
            assert!(q.push(WriteCmd::TapKeycode(3)));
        }
        assert!(!q.push(WriteCmd::TapKeycode(3)));
        q.request_shutdown();
        assert!(matches!(q.pop_blocking(), Some(WriteCmd::Shutdown)));
        assert!(q.pop_blocking().is_none());
    }
}
