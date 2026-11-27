use crate::adb::DeviceController;
use crate::config::{to_wide, AppConfig};
use std::ffi::c_void;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const WORKER_COUNT: usize = 32;
const CONNECT_TIMEOUT_MS: u64 = 220;
const MAX_SUBNETS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub addr: String,
    pub label: String,
    pub is_xiaomi: bool,
    pub is_tv_model: bool,
    pub adb_ready: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MibIpAddrRow {
    dw_addr: u32,
    dw_index: u32,
    dw_mask: u32,
    dw_bcast_addr: u32,
    dw_reasm_size: u32,
    unused1: u16,
    w_type: u16,
}

type FnGetIpAddrTable = unsafe extern "system" fn(*mut u8, *mut u32, i32) -> u32;

static GET_IP_ADDR_TABLE: OnceLock<Option<FnGetIpAddrTable>> = OnceLock::new();

fn load_get_ip_addr_table() -> Option<FnGetIpAddrTable> {
    *GET_IP_ADDR_TABLE.get_or_init(|| unsafe {
        let hmod = LoadLibraryW(to_wide("iphlpapi.dll").as_ptr());
        if hmod.is_null() {
            return None;
        }
        let proc = GetProcAddress(hmod, b"GetIpAddrTable\0".as_ptr())?;
        Some(std::mem::transmute::<*const c_void, FnGetIpAddrTable>(
            proc as *const c_void,
        ))
    })
}

pub(crate) fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
}

pub(crate) fn extract_hint_ipv4_and_port(hint: &str) -> (Option<Ipv4Addr>, u16) {
    let trimmed = hint.trim();
    if let Some((h, p)) = trimmed.rsplit_once(':') {
        let ip = h.trim().parse::<Ipv4Addr>().ok();
        let port = p.trim().parse::<u16>().ok().filter(|&v| v > 0).unwrap_or(5555);
        (ip, port)
    } else {
        (trimmed.parse::<Ipv4Addr>().ok(), 5555)
    }
}

fn enumerate_local_private_ipv4s() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    if let Some(get_table) = load_get_ip_addr_table() {
        unsafe {
            let mut size: u32 = 0;
            let _ = get_table(std::ptr::null_mut(), &mut size, 0);
            if size >= 4 && size <= 1024 * 1024 {
                let mut buf = vec![0u8; size as usize];
                if get_table(buf.as_mut_ptr(), &mut size, 0) == 0 {
                    let count = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                    let row_sz = std::mem::size_of::<MibIpAddrRow>();
                    let max_rows = (buf.len().saturating_sub(4)) / row_sz;
                    let n = count.min(max_rows);
                    let base = buf.as_ptr().add(4) as *const MibIpAddrRow;
                    for i in 0..n {
                        let row = *base.add(i);
                        let ip = Ipv4Addr::from(row.dw_addr.to_ne_bytes());
                        if is_private_ipv4(ip) && !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                }
            }
        }
    }
    if out.is_empty() {
        // Fallback: ask OS routing table which local IP reaches LAN/WAN
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            for target in ["192.168.1.1:80", "10.0.0.1:80", "8.8.8.8:80"] {
                if sock.connect(target).is_ok() {
                    if let Ok(SocketAddr::V4(v4)) = sock.local_addr() {
                        let ip = *v4.ip();
                        if is_private_ipv4(ip) && !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn build_subnet_prefixes(
    local_ips: &[Ipv4Addr],
    hint_ip: Option<Ipv4Addr>,
) -> Vec<[u8; 3]> {
    let mut prefixes: Vec<[u8; 3]> = Vec::new();

    // Prioritize the subnet of existing TvAddress first if private
    if let Some(hip) = hint_ip {
        if is_private_ipv4(hip) {
            let o = hip.octets();
            prefixes.push([o[0], o[1], o[2]]);
        }
    }
    // Prefer 192.168.* home LAN subnets ahead of virtual adapters (172.x / 10.x)
    for &ip in local_ips {
        let o = ip.octets();
        if o[0] == 192 && o[1] == 168 {
            let p = [o[0], o[1], o[2]];
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
        }
    }
    for &ip in local_ips {
        let o = ip.octets();
        let p = [o[0], o[1], o[2]];
        if !prefixes.contains(&p) {
            prefixes.push(p);
        }
    }
    prefixes.truncate(MAX_SUBNETS);
    prefixes
}

pub(crate) fn build_scan_targets(
    local_ips: &[Ipv4Addr],
    hint_ip: Option<Ipv4Addr>,
) -> Vec<Ipv4Addr> {
    let mut self_ips: Vec<Ipv4Addr> = local_ips.to_vec();
    self_ips.sort();
    self_ips.dedup();

    let prefixes = build_subnet_prefixes(local_ips, hint_ip);
    let mut targets = Vec::with_capacity(prefixes.len() * 254);
    for p in prefixes {
        for host in 1u8..=254u8 {
            let ip = Ipv4Addr::new(p[0], p[1], p[2], host);
            if !self_ips.contains(&ip) {
                targets.push(ip);
            }
        }
    }
    targets
}

pub(crate) fn classify_adb_props(raw: &str) -> (String, bool, bool) {
    let mut parts = raw.split('|').map(|s| s.trim());
    let model = parts.next().unwrap_or("");
    let mfg = parts.next().unwrap_or("");
    let device = parts.next().unwrap_or("");
    let combined = format!("{} {} {}", model, mfg, device).to_ascii_lowercase();
    let is_xiaomi = combined.contains("xiaomi")
        || combined.contains("redmi")
        || combined.contains("mitv")
        || combined.contains("beekeeper")
        || combined.contains("fengmi");
    let is_tv_model = is_xiaomi
        && (combined.contains("mitv")
            || combined.contains("beekeeper")
            || combined.contains("monitor")
            || combined.contains("fengmi")
            || combined.contains("tv"));

    let label = if !model.is_empty() {
        model.to_string()
    } else if !device.is_empty() {
        device.to_string()
    } else if !mfg.is_empty() {
        mfg.to_string()
    } else {
        "Android ADB".to_string()
    };
    (label, is_xiaomi, is_tv_model)
}

pub(crate) fn sort_discovered_devices(devs: &mut [DiscoveredDevice]) {
    devs.sort_by(|a, b| {
        b.is_tv_model
            .cmp(&a.is_tv_model)
            .then_with(|| b.is_xiaomi.cmp(&a.is_xiaomi))
            .then_with(|| b.adb_ready.cmp(&a.adb_ready))
            .then_with(|| a.addr.cmp(&b.addr))
    });
}

/// Strictly selects the best rediscovered Xiaomi/Redmi TV/monitor at a NEW address
/// (never auto-adopts non-Xiaomi Android devices or the stale failed address).
pub(crate) fn select_rediscovered_tv<'a>(
    candidates: &'a [DiscoveredDevice],
    old_addr: &str,
) -> Option<&'a DiscoveredDevice> {
    let old = old_addr.trim();
    candidates
        .iter()
        .filter(|d| d.is_xiaomi && d.adb_ready && !d.addr.eq_ignore_ascii_case(old))
        .max_by(|a, b| {
            a.is_tv_model
                .cmp(&b.is_tv_model)
                .then_with(|| b.addr.cmp(&a.addr))
        })
}

fn probe_open_ports(
    targets: Vec<Ipv4Addr>,
    port: u16,
    is_cancelled: &Arc<dyn Fn() -> bool + Send + Sync>,
) -> Vec<Ipv4Addr> {
    if targets.is_empty() || is_cancelled() {
        return Vec::new();
    }
    let targets = Arc::new(targets);
    let next_idx = Arc::new(AtomicUsize::new(0));
    let open_ips = Arc::new(Mutex::new(Vec::<Ipv4Addr>::new()));
    let workers = WORKER_COUNT.min(targets.len());

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let t = Arc::clone(&targets);
        let idx = Arc::clone(&next_idx);
        let found = Arc::clone(&open_ips);
        let cancel = Arc::clone(is_cancelled);
        handles.push(std::thread::spawn(move || loop {
            if cancel() {
                break;
            }
            let i = idx.fetch_add(1, Ordering::Relaxed);
            if i >= t.len() {
                break;
            }
            let ip = t[i];
            let sa = SocketAddr::V4(SocketAddrV4::new(ip, port));
            if TcpStream::connect_timeout(&sa, Duration::from_millis(CONNECT_TIMEOUT_MS)).is_ok() {
                if let Ok(mut g) = found.lock() {
                    g.push(ip);
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    open_ips.lock().map(|g| g.clone()).unwrap_or_default()
}

fn identify_adb_host(dev: &Arc<DeviceController>, ip: Ipv4Addr, port: u16) -> DiscoveredDevice {
    let addr = format!("{}:{}", ip, port);
    let adb_ready = dev.test_connect(&addr);
    let (label, is_xiaomi, is_tv_model) = if adb_ready {
        let (_, out, _) = dev.run_adb(
            &[
                "-s",
                &addr,
                "shell",
                "echo \"$(getprop ro.product.model)|$(getprop ro.product.manufacturer)|$(getprop ro.product.device)\"",
            ],
            2000,
        );
        classify_adb_props(&out)
    } else {
        ("端口开放 (待确认 ADB)".to_string(), false, false)
    };
    diag_log!(
        "SCAN",
        "candidate addr={} ready={} xiaomi={} tv={} label={}",
        addr,
        adb_ready,
        is_xiaomi,
        is_tv_model,
        label
    );
    DiscoveredDevice {
        addr,
        label,
        is_xiaomi,
        is_tv_model,
        adb_ready,
    }
}

/// Scans local private `/24` LAN subnets for hosts with wireless ADB port (default 5555) open,
/// verifies ADB connectivity, and ranks Xiaomi/Redmi TV/monitors first.
pub fn scan_lan_adb_devices(dev: &Arc<DeviceController>, hint_addr: &str) -> Vec<DiscoveredDevice> {
    let (hint_ip, port) = extract_hint_ipv4_and_port(hint_addr);
    let locals = enumerate_local_private_ipv4s();
    let targets = build_scan_targets(&locals, hint_ip);
    diag_log!(
        "SCAN",
        "start port={} local_ifaces={} targets={}",
        port,
        locals.len(),
        targets.len()
    );
    let never_cancel: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| false);
    let ips = probe_open_ports(targets, port, &never_cancel);

    let mut results = Vec::with_capacity(ips.len());
    for ip in ips {
        results.push(identify_adb_host(dev, ip, port));
    }

    sort_discovered_devices(&mut results);
    results
}

/// Fast subnet-by-subnet auto-rediscovery when router DHCP reassigns the TV's IP.
/// Probes the old address's `/24` subnet first (~1.7s) and stops as soon as a verified
/// Xiaomi/Redmi TV/monitor is found at a new IP.
pub fn rediscover_xiaomi_tv<F>(
    dev: &Arc<DeviceController>,
    old_addr: &str,
    is_cancelled: F,
) -> Option<DiscoveredDevice>
where
    F: Fn() -> bool + Send + Sync + 'static,
{
    let (old_ip, port) = extract_hint_ipv4_and_port(old_addr);
    let locals = enumerate_local_private_ipv4s();
    let prefixes = build_subnet_prefixes(&locals, old_ip);
    if prefixes.is_empty() {
        return None;
    }

    let cancel: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(is_cancelled);
    diag_log!(
        "SCAN",
        "rediscover start old_addr={} subnets={}",
        old_addr,
        prefixes.len()
    );

    for p in prefixes {
        if cancel() {
            return None;
        }
        let mut targets = Vec::with_capacity(254);
        for host in 1u8..=254u8 {
            let ip = Ipv4Addr::new(p[0], p[1], p[2], host);
            if Some(ip) == old_ip || locals.contains(&ip) {
                continue;
            }
            targets.push(ip);
        }
        let open_ips = probe_open_ports(targets, port, &cancel);
        if cancel() {
            return None;
        }
        let mut found_in_subnet = Vec::with_capacity(open_ips.len());
        for ip in open_ips {
            if cancel() {
                return None;
            }
            found_in_subnet.push(identify_adb_host(dev, ip, port));
        }
        if let Some(best) = select_rediscovered_tv(&found_in_subnet, old_addr) {
            diag_log!(
                "SCAN",
                "rediscovered {} ({}) replacing {}",
                best.addr,
                best.label,
                old_addr
            );
            return Some(best.clone());
        }
    }
    None
}

/// Persists a newly rediscovered TV address into `config.ini` and clears stale ADB/daemon caches.
pub fn apply_rediscovered_address(
    cfg: &Arc<Mutex<AppConfig>>,
    dev: &Arc<DeviceController>,
    found: &DiscoveredDevice,
) -> bool {
    let updated = if let Ok(mut c) = cfg.lock() {
        if c.tv_address != found.addr {
            c.tv_address = found.addr.clone();
            c.save();
            true
        } else {
            false
        }
    } else {
        false
    };
    if updated {
        dev.reset_stale_connections();
        diag_log!(
            "SCAN",
            "auto-updated TvAddress={} ({})",
            found.addr,
            found.label
        );
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ipv4_filtering() {
        assert!(is_private_ipv4(Ipv4Addr::new(192, 168, 1, 200)));
        assert!(is_private_ipv4(Ipv4Addr::new(10, 0, 0, 5)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 31, 255, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn hint_ip_and_port_parsing() {
        assert_eq!(
            extract_hint_ipv4_and_port("192.168.1.200:5555"),
            (Some(Ipv4Addr::new(192, 168, 1, 200)), 5555)
        );
        assert_eq!(
            extract_hint_ipv4_and_port("192.168.1.50:5566"),
            (Some(Ipv4Addr::new(192, 168, 1, 50)), 5566)
        );
        assert_eq!(
            extract_hint_ipv4_and_port("10.0.0.12"),
            (Some(Ipv4Addr::new(10, 0, 0, 12)), 5555)
        );
        assert_eq!(extract_hint_ipv4_and_port("invalid"), (None, 5555));
    }

    #[test]
    fn scan_targets_skip_self_and_prioritize_hint_subnet() {
        let locals = [Ipv4Addr::new(172, 20, 10, 2), Ipv4Addr::new(192, 168, 1, 10)];
        let targets = build_scan_targets(&locals, Some(Ipv4Addr::new(192, 168, 1, 200)));
        assert_eq!(targets.len(), 253 + 253);
        // Hint subnet 192.168.1.x comes first
        assert_eq!(targets[0], Ipv4Addr::new(192, 168, 1, 1));
        assert!(!targets.contains(&Ipv4Addr::new(192, 168, 1, 10)));
        assert!(!targets.contains(&Ipv4Addr::new(172, 20, 10, 2)));
        assert!(targets.contains(&Ipv4Addr::new(192, 168, 1, 200)));
    }

    #[test]
    fn adb_prop_classification_and_ranking() {
        let (lbl, mi, tv) = classify_adb_props("MiTV-MFFU1|Xiaomi|beekeeper\n");
        assert_eq!(lbl, "MiTV-MFFU1");
        assert!(mi);
        assert!(tv);

        let (lbl_phone, mi_phone, tv_phone) = classify_adb_props("24031PN0DC|Xiaomi|aurora\n");
        assert_eq!(lbl_phone, "24031PN0DC");
        assert!(mi_phone);
        assert!(!tv_phone);

        let (lbl2, mi2, tv2) = classify_adb_props("Pixel 8|Google|shiba\n");
        assert_eq!(lbl2, "Pixel 8");
        assert!(!mi2);
        assert!(!tv2);

        let mut devs = vec![
            DiscoveredDevice {
                addr: "192.168.1.50:5555".into(),
                label: "Pixel 8".into(),
                is_xiaomi: false,
                is_tv_model: false,
                adb_ready: true,
            },
            DiscoveredDevice {
                addr: "192.168.1.120:5555".into(),
                label: "Xiaomi 14 Ultra".into(),
                is_xiaomi: true,
                is_tv_model: false,
                adb_ready: true,
            },
            DiscoveredDevice {
                addr: "192.168.1.205:5555".into(),
                label: "MiTV-MFFU1".into(),
                is_xiaomi: true,
                is_tv_model: true,
                adb_ready: true,
            },
        ];
        sort_discovered_devices(&mut devs);
        assert_eq!(devs[0].addr, "192.168.1.205:5555");
        assert_eq!(devs[1].addr, "192.168.1.120:5555");
    }

    #[test]
    fn rediscovery_selector_prefers_xiaomi_tv_and_rejects_foreign_or_stale() {
        let foreign_only = vec![DiscoveredDevice {
            addr: "192.168.1.50:5555".into(),
            label: "Pixel 8".into(),
            is_xiaomi: false,
            is_tv_model: false,
            adb_ready: true,
        }];
        assert_eq!(select_rediscovered_tv(&foreign_only, "192.168.1.200:5555"), None);

        let mixed = vec![
            DiscoveredDevice {
                addr: "192.168.1.200:5555".into(),
                label: "MiTV-MFFU1".into(),
                is_xiaomi: true,
                is_tv_model: true,
                adb_ready: true,
            },
            DiscoveredDevice {
                addr: "192.168.1.110:5555".into(),
                label: "Xiaomi 14".into(),
                is_xiaomi: true,
                is_tv_model: false,
                adb_ready: true,
            },
            DiscoveredDevice {
                addr: "192.168.1.208:5555".into(),
                label: "MiTV-MFFU1".into(),
                is_xiaomi: true,
                is_tv_model: true,
                adb_ready: true,
            },
        ];
        // Skips old .200 and chooses TV model .208 ahead of Xiaomi phone .110
        let picked = select_rediscovered_tv(&mixed, "192.168.1.200:5555").unwrap();
        assert_eq!(picked.addr, "192.168.1.208:5555");
    }
}
