use std::ffi::c_void;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::UI::Input::Ime::{ImmGetDefaultIMEWnd, IMC_SETOPENSTATUS};
const IMC_GETOPENSTATUS: usize = 0x0005;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, HKL};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, PostMessageW, SendMessageW, WM_IME_CONTROL, WM_INPUTLANGCHANGEREQUEST,
};

const TF_PROFILETYPE_KEYBOARDLAYOUT: u32 = 0x0002;
const TF_IPPMF_FORSESSION: u32 = 0x2000_0000;
const TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE: u32 = 0x0000_0004;

const GUID_TFCAT_TIP_KEYBOARD: GUID = GUID {
    data1: 0x34745C63,
    data2: 0xB2F0,
    data3: 0x4784,
    data4: [0x8B, 0x67, 0x5E, 0x12, 0xC8, 0x70, 0x1A, 0x31],
};

const IID_ITF_INPUT_PROCESSOR_PROFILE_MGR: GUID = GUID {
    data1: 0x71C6E74C,
    data2: 0x0F28,
    data3: 0x11D8,
    data4: [0xA8, 0x2A, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C],
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TfInputProcessorProfile {
    pub profile_type: u32,
    pub langid: u16,
    pub clsid: GUID,
    pub guid_profile: GUID,
    pub catid: GUID,
    pub hkl_substitute: isize,
    pub caps: u32,
    pub hkl: isize,
    pub flags: u32,
}

impl Default for TfInputProcessorProfile {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ITfInputProcessorProfileMgrVtbl {
    base: IUnknownVtbl,
    activate_profile: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u16,
        *const GUID,
        *const GUID,
        isize,
        u32,
    ) -> i32,
    deactivate_profile: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u16,
        *const GUID,
        *const GUID,
        isize,
        u32,
    ) -> i32,
    get_profile: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u16,
        *const GUID,
        *const GUID,
        isize,
        *mut TfInputProcessorProfile,
    ) -> i32,
    enum_profiles: unsafe extern "system" fn(*mut c_void, u16, *mut *mut c_void) -> i32,
    release_input_processor: unsafe extern "system" fn(*mut c_void, *const GUID, u32) -> i32,
    register_profile: *const c_void,
    unregister_profile: *const c_void,
    get_active_profile:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut TfInputProcessorProfile) -> i32,
}

type FnCreateProfiles = unsafe extern "system" fn(*mut *mut c_void) -> i32;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *const c_void;
}

fn load_tf_create_fn() -> Option<FnCreateProfiles> {
    static FN_PTR: std::sync::OnceLock<Option<FnCreateProfiles>> = std::sync::OnceLock::new();
    *FN_PTR.get_or_init(|| unsafe {
        let dll: Vec<u16> = "msctf.dll\0".encode_utf16().collect();
        let hmod = LoadLibraryW(dll.as_ptr());
        if hmod.is_null() {
            return None;
        }
        let proc = GetProcAddress(hmod, b"TF_CreateInputProcessorProfiles\0".as_ptr());
        if proc.is_null() {
            return None;
        }
        Some(std::mem::transmute::<*const c_void, FnCreateProfiles>(proc))
    })
}

unsafe fn with_profile_mgr<F, R>(f: F) -> Option<R>
where
    F: FnOnce(*mut c_void, &ITfInputProcessorProfileMgrVtbl) -> R,
{
    let create_fn = load_tf_create_fn()?;
    let mut raw: *mut c_void = std::ptr::null_mut();
    if create_fn(&mut raw) != 0 || raw.is_null() {
        return None;
    }
    let unk_vtbl = &*(*(raw as *const *const IUnknownVtbl));
    let mut mgr: *mut c_void = std::ptr::null_mut();
    let hr = (unk_vtbl.query_interface)(raw, &IID_ITF_INPUT_PROCESSOR_PROFILE_MGR, &mut mgr);
    (unk_vtbl.release)(raw);
    if hr != 0 || mgr.is_null() {
        return None;
    }
    let vtbl = &*(*(mgr as *const *const ITfInputProcessorProfileMgrVtbl));
    let result = f(mgr, vtbl);
    (vtbl.base.release)(mgr);
    Some(result)
}

pub fn get_active_tsf_profile() -> Option<TfInputProcessorProfile> {
    unsafe {
        with_profile_mgr(|mgr, vtbl| {
            let mut prof = TfInputProcessorProfile::default();
            if (vtbl.get_active_profile)(mgr, &GUID_TFCAT_TIP_KEYBOARD, &mut prof) == 0 {
                Some(prof)
            } else {
                None
            }
        })
        .flatten()
    }
}

pub fn activate_tsf_profile(prof: &TfInputProcessorProfile) -> bool {
    unsafe {
        with_profile_mgr(|mgr, vtbl| {
            (vtbl.activate_profile)(
                mgr,
                prof.profile_type,
                prof.langid,
                &prof.clsid,
                &prof.guid_profile,
                prof.hkl,
                TF_IPPMF_FORSESSION | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
            ) == 0
        })
        .unwrap_or(false)
    }
}

pub fn find_english_hkl() -> HKL {
    unsafe {
        let count = GetKeyboardLayoutList(0, std::ptr::null_mut());
        if count <= 0 {
            return std::ptr::null_mut();
        }
        let mut list: Vec<HKL> = vec![std::ptr::null_mut(); count as usize];
        let got = GetKeyboardLayoutList(count, list.as_mut_ptr());
        for &hkl in list.iter().take(got.max(0) as usize) {
            if ((hkl as usize) & 0xFFFF) == 0x0409 {
                return hkl;
            }
        }
        std::ptr::null_mut()
    }
}

#[derive(Clone, Copy)]
pub struct SavedImeState {
    pub profile: TfInputProcessorProfile,
    pub ime_open: isize,
}

pub fn save_current_ime() -> Option<SavedImeState> {
    let profile = get_active_tsf_profile()?;
    let ime_open = unsafe {
        let fg = GetForegroundWindow();
        if !fg.is_null() {
            let ime_wnd = ImmGetDefaultIMEWnd(fg);
            if !ime_wnd.is_null() {
                SendMessageW(ime_wnd, WM_IME_CONTROL, IMC_GETOPENSTATUS as WPARAM, 0)
            } else {
                0
            }
        } else {
            0
        }
    };
    Some(SavedImeState { profile, ime_open })
}

pub fn restore_saved_ime(saved: &SavedImeState) {
    let _ = activate_tsf_profile(&saved.profile);
    if saved.ime_open != 0 {
        unsafe {
            let fg = GetForegroundWindow();
            if !fg.is_null() {
                let ime_wnd = ImmGetDefaultIMEWnd(fg);
                if !ime_wnd.is_null() {
                    SendMessageW(
                        ime_wnd,
                        WM_IME_CONTROL,
                        IMC_SETOPENSTATUS as WPARAM,
                        saved.ime_open as LPARAM,
                    );
                }
            }
        }
    }
}

pub fn enforce_scrcpy_english_ime(scrcpy_hwnd: HWND, english_hkl: HKL, switch_layout: bool) {
    if scrcpy_hwnd.is_null() {
        return;
    }
    if switch_layout && !english_hkl.is_null() {
        let eng = TfInputProcessorProfile {
            profile_type: TF_PROFILETYPE_KEYBOARDLAYOUT,
            langid: 0x0409,
            hkl: english_hkl as isize,
            ..Default::default()
        };
        if !activate_tsf_profile(&eng) {
            unsafe {
                PostMessageW(
                    scrcpy_hwnd,
                    WM_INPUTLANGCHANGEREQUEST,
                    0,
                    english_hkl as LPARAM,
                );
            }
        }
    }
    unsafe {
        let ime_wnd = ImmGetDefaultIMEWnd(scrcpy_hwnd);
        if !ime_wnd.is_null() {
            let st = SendMessageW(ime_wnd, WM_IME_CONTROL, IMC_GETOPENSTATUS as WPARAM, 0);
            if st != 0 {
                SendMessageW(ime_wnd, WM_IME_CONTROL, IMC_SETOPENSTATUS as WPARAM, 0);
            }
        }
    }
}
