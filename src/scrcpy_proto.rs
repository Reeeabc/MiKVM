use crate::adb::DeviceController;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const FALLBACK_SCRCPY_VERSION: &str = "3.3.1";
const REMOTE_SERVER_PATH: &str = "/data/local/tmp/scrcpy-server.jar";

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

struct ConnState {
    writer: TcpStream,
    buttons: u8,
    pressed_keys: Vec<i32>,
}

pub struct ScrcpyControlClient {
    adb_path: PathBuf,
    tv_addr: String,
    local_port: u16,
    server_child: Mutex<Option<Child>>,
    conn: Mutex<Option<ConnState>>,
    alive: Arc<AtomicBool>,
    clip_seq: AtomicU64,
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
        FExit: Fn() + Send + 'static,
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

        let (Some(writer), Some(child)) = (stream_opt, child_opt) else {
            let _ = dev.run_adb(&["-s", &tv_addr, "forward", "--remove", &local_spec], 1500);
            return Err("连接 scrcpy-server 控制通道超时".to_string());
        };

        let reader = writer
            .try_clone()
            .map_err(|e| format!("克隆控制通道失败: {}", e))?;

        let alive = Arc::new(AtomicBool::new(true));
        let client = Arc::new(Self {
            adb_path,
            tv_addr,
            local_port,
            server_child: Mutex::new(Some(child)),
            conn: Mutex::new(Some(ConnState {
                writer,
                buttons: 0,
                pressed_keys: Vec::with_capacity(16),
            })),
            alive: Arc::clone(&alive),
            clip_seq: AtomicU64::new(1),
        });

        // 5. Register virtual UHID relative mouse immediately
        if !client.send_uhid_create() {
            client.close();
            return Err("初始化电视虚拟 UHID 鼠标失败".to_string());
        }

        // 6. Spawn background reader thread for device messages (clipboard / disconnect)
        let alive_r = Arc::clone(&alive);
        std::thread::spawn(move || {
            device_reader_loop(reader, alive_r, on_clipboard, on_disconnect);
        });

        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn send_uhid_create(&self) -> bool {
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
        self.write_raw(&buf)
    }

    fn write_raw(&self, data: &[u8]) -> bool {
        if !self.alive.load(Ordering::Relaxed) {
            return false;
        }
        if let Ok(mut g) = self.conn.lock() {
            if let Some(ref mut c) = *g {
                if c.writer.write_all(data).is_ok() {
                    return true;
                }
                self.alive.store(false, Ordering::Release);
            }
        }
        false
    }

    /// Sends relative mouse movement; splits large deltas > 127 cleanly without clamping loss.
    pub fn send_mouse_move(&self, mut dx: i32, mut dy: i32) {
        if (dx == 0 && dy == 0) || !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut g) = self.conn.try_lock() else {
            return;
        };
        let Some(ref mut c) = *g else {
            return;
        };
        while dx != 0 || dy != 0 {
            let sx = dx.clamp(-127, 127) as i8;
            let sy = dy.clamp(-127, 127) as i8;
            dx -= sx as i32;
            dy -= sy as i32;
            let pkt = build_uhid_mouse_packet(c.buttons, sx, sy, 0, 0);
            if c.writer.write_all(&pkt).is_err() {
                self.alive.store(false, Ordering::Release);
                return;
            }
        }
    }

    /// Sends an entire multi-packet relative mouse move in ONE single TCP write so Android
    /// InputReader processes the whole displacement inside a single 16ms display frame!
    pub fn send_mouse_burst(&self, mut dx: i32, mut dy: i32) {
        if (dx == 0 && dy == 0) || !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut g) = self.conn.lock() else {
            return;
        };
        let Some(ref mut c) = *g else {
            return;
        };
        let steps = ((dx.abs().max(dy.abs()) + 126) / 127).max(1) as usize;
        let mut buf = Vec::with_capacity(steps * 10);
        while dx != 0 || dy != 0 {
            let sx = dx.clamp(-127, 127) as i8;
            let sy = dy.clamp(-127, 127) as i8;
            dx -= sx as i32;
            dy -= sy as i32;
            buf.extend_from_slice(&build_uhid_mouse_packet(c.buttons, sx, sy, 0, 0));
        }
        if c.writer.write_all(&buf).is_err() {
            self.alive.store(false, Ordering::Release);
        }
    }

    /// Sends a velocity-primed single-frame burst in ONE TCP write:
    /// Packet 1 = `(prime_dx, prime_dy)` pins the cursor to the edge AND primes Android's
    /// VelocityControl (eliminating the 1x->3x non-linear cliff after >200ms idle!),
    /// followed immediately by `(dx, dy)` packets that scale linearly at exact `acceleration` (3.0x),
    /// achieving +/-1px accuracy across the entire TV screen in a single VSync frame!
    pub fn send_primed_mouse_burst(
        &self,
        prime_dx: i32,
        prime_dy: i32,
        mut dx: i32,
        mut dy: i32,
    ) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut g) = self.conn.lock() else {
            return;
        };
        let Some(ref mut c) = *g else {
            return;
        };
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
            buf.extend_from_slice(&build_uhid_mouse_packet(c.buttons, sx, sy, 0, 0));
        }
        while dx != 0 || dy != 0 {
            let sx = dx.clamp(-127, 127) as i8;
            let sy = dy.clamp(-127, 127) as i8;
            dx -= sx as i32;
            dy -= sy as i32;
            buf.extend_from_slice(&build_uhid_mouse_packet(c.buttons, sx, sy, 0, 0));
        }
        if !buf.is_empty() && c.writer.write_all(&buf).is_err() {
            self.alive.store(false, Ordering::Release);
        }
    }

    pub fn send_mouse_button(&self, mask: u8, down: bool) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut g) = self.conn.lock() {
            if let Some(ref mut c) = *g {
                if down {
                    c.buttons |= mask;
                } else {
                    c.buttons &= !mask;
                }
                let pkt = build_uhid_mouse_packet(c.buttons, 0, 0, 0, 0);
                if c.writer.write_all(&pkt).is_err() {
                    self.alive.store(false, Ordering::Release);
                }
            }
        }
    }

    pub fn send_keycode(&self, action: u8, keycode: i32, repeat: i32, metastate: i32) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut g) = self.conn.lock() {
            if let Some(ref mut c) = *g {
                if action == ACTION_DOWN {
                    if !c.pressed_keys.contains(&keycode) {
                        c.pressed_keys.push(keycode);
                    }
                } else {
                    c.pressed_keys.retain(|&k| k != keycode);
                }
                let pkt = build_inject_keycode_packet(action, keycode, repeat, metastate);
                if c.writer.write_all(&pkt).is_err() {
                    self.alive.store(false, Ordering::Release);
                }
            }
        }
    }

    /// Sends a quick down+up tap for a single Android keycode (e.g. Back / Home / Menu / Vol).
    pub fn tap_keycode(&self, keycode: i32) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut g) = self.conn.lock() {
            if let Some(ref mut c) = *g {
                let down = build_inject_keycode_packet(ACTION_DOWN, keycode, 0, 0);
                let up = build_inject_keycode_packet(ACTION_UP, keycode, 0, 0);
                let mut both = [0u8; 28];
                both[..14].copy_from_slice(&down);
                both[14..].copy_from_slice(&up);
                if c.writer.write_all(&both).is_err() {
                    self.alive.store(false, Ordering::Release);
                }
            }
        }
    }

    /// Releases all currently held mouse buttons and Android keys so leaving TV focus never causes stuck keys!
    pub fn release_all_inputs(&self) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut g) = self.conn.lock() {
            if let Some(ref mut c) = *g {
                if c.buttons != 0 {
                    c.buttons = 0;
                    let pkt = build_uhid_mouse_packet(0, 0, 0, 0, 0);
                    let _ = c.writer.write_all(&pkt);
                }
                let keys: Vec<i32> = c.pressed_keys.drain(..).collect();
                for kc in keys {
                    let pkt = build_inject_keycode_packet(ACTION_UP, kc, 0, 0);
                    let _ = c.writer.write_all(&pkt);
                }
            }
        }
    }

    pub fn set_tv_clipboard(&self, text: &str, paste: bool) {
        if !self.alive.load(Ordering::Relaxed) || text.is_empty() {
            return;
        }
        let mut cut = text.len().min(CLIPBOARD_MAX_BYTES);
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let bytes = &text.as_bytes()[..cut];
        let seq = self.clip_seq.fetch_add(1, Ordering::Relaxed);
        let mut buf = Vec::with_capacity(1 + 8 + 1 + 4 + bytes.len());
        buf.push(TYPE_SET_CLIPBOARD);
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.push(if paste { 1 } else { 0 });
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
        let _ = self.write_raw(&buf);
    }

    pub fn close(&self) {
        let was_alive = self.alive.swap(false, Ordering::AcqRel);
        if let Ok(mut g) = self.conn.lock() {
            if let Some(mut c) = g.take() {
                if was_alive {
                    if c.buttons != 0 {
                        let pkt = build_uhid_mouse_packet(0, 0, 0, 0, 0);
                        let _ = c.writer.write_all(&pkt);
                    }
                    for kc in c.pressed_keys.drain(..) {
                        let pkt = build_inject_keycode_packet(ACTION_UP, kc, 0, 0);
                        let _ = c.writer.write_all(&pkt);
                    }
                    let mut destroy = [0u8; 3];
                    destroy[0] = TYPE_UHID_DESTROY;
                    destroy[1..3].copy_from_slice(&UHID_MOUSE_ID.to_be_bytes());
                    let _ = c.writer.write_all(&destroy);
                    let _ = c.writer.flush();
                    std::thread::sleep(Duration::from_millis(15));
                }
                let _ = c.writer.shutdown(Shutdown::Both);
            }
        }
        if let Ok(mut cg) = self.server_child.lock() {
            if let Some(mut ch) = cg.take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
        let local_spec = format!("tcp:{}", self.local_port);
        let _ = Command::new(&self.adb_path)
            .args(["-s", &self.tv_addr, "forward", "--remove", &local_spec])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
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

fn device_reader_loop<FClip, FExit>(
    mut reader: TcpStream,
    alive: Arc<AtomicBool>,
    on_clipboard: FClip,
    on_disconnect: FExit,
) where
    FClip: Fn(String),
    FExit: Fn(),
{
    let mut hdr = [0u8; 1];
    while alive.load(Ordering::Acquire) {
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        match hdr[0] {
            DEV_TYPE_CLIPBOARD => {
                let mut len_buf = [0u8; 4];
                if reader.read_exact(&mut len_buf).is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > 512 * 1024 {
                    break;
                }
                let mut data = vec![0u8; len];
                if reader.read_exact(&mut data).is_err() {
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
                    break;
                }
            }
            DEV_TYPE_UHID_OUTPUT => {
                let mut info = [0u8; 4];
                if reader.read_exact(&mut info).is_err() {
                    break;
                }
                let data_len = u16::from_be_bytes([info[2], info[3]]) as usize;
                let mut skip = vec![0u8; data_len];
                if reader.read_exact(&mut skip).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    if alive.swap(false, Ordering::AcqRel) {
        on_disconnect();
    }
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
}
