use crate::config::to_wide;
use crate::display::find_tv_monitor_rect;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{BOOL, HANDLE, POINT};
use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// MCCS VCP code 0x10 = Luminance / Backlight Brightness
const VCP_BRIGHTNESS: u8 = 0x10;

#[repr(C)]
struct PhysicalMonitor {
    h_physical_monitor: HANDLE,
    sz_physical_monitor_description: [u16; 128],
}

type FnGetNumPhys = unsafe extern "system" fn(HMONITOR, *mut u32) -> BOOL;
type FnGetPhys = unsafe extern "system" fn(HMONITOR, u32, *mut PhysicalMonitor) -> BOOL;
type FnDestroyPhys = unsafe extern "system" fn(u32, *const PhysicalMonitor) -> BOOL;
type FnGetVcp = unsafe extern "system" fn(HANDLE, u8, *mut u32, *mut u32, *mut u32) -> BOOL;
type FnSetVcp = unsafe extern "system" fn(HANDLE, u8, u32) -> BOOL;

struct Dxva2Api {
    get_num: FnGetNumPhys,
    get_phys: FnGetPhys,
    destroy_phys: FnDestroyPhys,
    get_vcp: FnGetVcp,
    set_vcp: FnSetVcp,
}

static DXVA2: OnceLock<Option<Dxva2Api>> = OnceLock::new();
static CACHED_MAX_VCP: AtomicU32 = AtomicU32::new(100);

fn load_dxva2() -> Option<&'static Dxva2Api> {
    DXVA2
        .get_or_init(|| unsafe {
            let hmod = LoadLibraryW(to_wide("dxva2.dll").as_ptr());
            if hmod.is_null() {
                return None;
            }
            let get_num = GetProcAddress(hmod, b"GetNumberOfPhysicalMonitorsFromHMONITOR\0".as_ptr())?;
            let get_phys = GetProcAddress(hmod, b"GetPhysicalMonitorsFromHMONITOR\0".as_ptr())?;
            let destroy_phys = GetProcAddress(hmod, b"DestroyPhysicalMonitors\0".as_ptr())?;
            let get_vcp = GetProcAddress(hmod, b"GetVCPFeatureAndVCPFeatureReply\0".as_ptr())?;
            let set_vcp = GetProcAddress(hmod, b"SetVCPFeature\0".as_ptr())?;

            Some(Dxva2Api {
                get_num: std::mem::transmute::<*const c_void, FnGetNumPhys>(get_num as *const c_void),
                get_phys: std::mem::transmute::<*const c_void, FnGetPhys>(get_phys as *const c_void),
                destroy_phys: std::mem::transmute::<*const c_void, FnDestroyPhys>(destroy_phys as *const c_void),
                get_vcp: std::mem::transmute::<*const c_void, FnGetVcp>(get_vcp as *const c_void),
                set_vcp: std::mem::transmute::<*const c_void, FnSetVcp>(set_vcp as *const c_void),
            })
        })
        .as_ref()
}

pub(crate) fn vcp_to_percent(cur: u32, max: u32) -> Option<i32> {
    if max == 0 {
        return None;
    }
    let clamped = cur.min(max) as u64;
    let m = max as u64;
    Some(((clamped * 100 + m / 2) / m).clamp(0, 100) as i32)
}

pub(crate) fn percent_to_vcp(pct: i32, max: u32) -> u32 {
    let p = pct.clamp(0, 100) as u64;
    let m = if max == 0 { 100 } else { max as u64 };
    ((p * m + 50) / 100).min(m) as u32
}

struct PhysGuard<'a> {
    api: &'a Dxva2Api,
    mons: Vec<PhysicalMonitor>,
}

impl Drop for PhysGuard<'_> {
    fn drop(&mut self) {
        if !self.mons.is_empty() {
            unsafe {
                (self.api.destroy_phys)(self.mons.len() as u32, self.mons.as_ptr());
            }
        }
    }
}

fn open_tv_physical_monitors<'a>(api: &'a Dxva2Api, tv_hint: &str) -> Option<PhysGuard<'a>> {
    let rect = find_tv_monitor_rect(tv_hint)?;
    let center = POINT {
        x: rect.left + (rect.right - rect.left) / 2,
        y: rect.top + (rect.bottom - rect.top) / 2,
    };
    unsafe {
        let hmon = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() {
            return None;
        }
        let mut count: u32 = 0;
        if (api.get_num)(hmon, &mut count) == 0 || count == 0 || count > 8 {
            return None;
        }
        let mut mons = Vec::with_capacity(count as usize);
        for _ in 0..count {
            mons.push(std::mem::zeroed::<PhysicalMonitor>());
        }
        if (api.get_phys)(hmon, count, mons.as_mut_ptr()) == 0 {
            return None;
        }
        Some(PhysGuard { api, mons })
    }
}

/// Reads physical monitor brightness (0..=100) via rootless DDC/CI VCP 0x10.
pub fn get_brightness(tv_hint: &str) -> Option<i32> {
    let api = load_dxva2()?;
    let guard = open_tv_physical_monitors(api, tv_hint)?;
    for pm in &guard.mons {
        let mut vcp_type: u32 = 0;
        let mut cur: u32 = 0;
        let mut max: u32 = 0;
        let ok = unsafe {
            (api.get_vcp)(
                pm.h_physical_monitor,
                VCP_BRIGHTNESS,
                &mut vcp_type,
                &mut cur,
                &mut max,
            )
        };
        if ok != 0 && max > 0 {
            CACHED_MAX_VCP.store(max, Ordering::Relaxed);
            if let Some(pct) = vcp_to_percent(cur, max) {
                diag_log!("DDC", "get_brightness cur={} max={} pct={}", cur, max, pct);
                return Some(pct);
            }
        }
    }
    None
}

/// Sets physical monitor brightness (0..=100) via rootless DDC/CI VCP 0x10.
pub fn set_brightness(tv_hint: &str, percent: i32) -> bool {
    let Some(api) = load_dxva2() else {
        return false;
    };
    let Some(guard) = open_tv_physical_monitors(api, tv_hint) else {
        return false;
    };
    let max = CACHED_MAX_VCP.load(Ordering::Relaxed);
    let val = percent_to_vcp(percent, max);
    for pm in &guard.mons {
        let ok = unsafe { (api.set_vcp)(pm.h_physical_monitor, VCP_BRIGHTNESS, val) };
        if ok != 0 {
            diag_log!("DDC", "set_brightness pct={} vcp={} max={}", percent, val, max);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcp_percent_conversion_standard_and_custom_ranges() {
        assert_eq!(vcp_to_percent(0, 100), Some(0));
        assert_eq!(vcp_to_percent(50, 100), Some(50));
        assert_eq!(vcp_to_percent(100, 100), Some(100));
        assert_eq!(vcp_to_percent(120, 100), Some(100));
        assert_eq!(vcp_to_percent(10, 0), None);

        // Non-100 VCP max (e.g. 255)
        assert_eq!(vcp_to_percent(128, 255), Some(50));
        assert_eq!(percent_to_vcp(50, 255), 128);
        assert_eq!(percent_to_vcp(100, 255), 255);
        assert_eq!(percent_to_vcp(-10, 100), 0);
        assert_eq!(percent_to_vcp(150, 100), 100);
        assert_eq!(percent_to_vcp(42, 0), 42);
    }
}
