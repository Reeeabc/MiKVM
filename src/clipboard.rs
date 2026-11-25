use crate::config::{from_wide, to_wide};
use std::ptr;
use std::sync::OnceLock;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT_FMT: u32 = 13;

fn exclude_monitor_format() -> u32 {
    static FMT: OnceLock<u32> = OnceLock::new();
    *FMT.get_or_init(|| unsafe {
        RegisterClipboardFormatW(
            to_wide("ExcludeClipboardContentFromMonitorProcessing").as_ptr(),
        )
    })
}

fn exclude_history_format() -> u32 {
    static FMT: OnceLock<u32> = OnceLock::new();
    *FMT.get_or_init(|| unsafe {
        RegisterClipboardFormatW(to_wide("CanIncludeInClipboardHistory").as_ptr())
    })
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Reads UTF-8 text from Windows clipboard unless marked sensitive by a password manager.
pub fn read_pc_clipboard_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT_FMT) == 0 {
            return None;
        }
        let ex_mon = exclude_monitor_format();
        if ex_mon != 0 && IsClipboardFormatAvailable(ex_mon) != 0 {
            return None;
        }
        let ex_hist = exclude_history_format();
        if OpenClipboard(ptr::null_mut()) == 0 {
            return None;
        }
        let mut sensitive = false;
        if ex_hist != 0 && IsClipboardFormatAvailable(ex_hist) != 0 {
            let h = GetClipboardData(ex_hist);
            if !h.is_null() {
                let p = GlobalLock(h) as *const u32;
                if !p.is_null() {
                    if *p == 0 {
                        sensitive = true;
                    }
                    GlobalUnlock(h);
                }
            }
        }
        let mut result = None;
        if !sensitive {
            let h = GetClipboardData(CF_UNICODETEXT_FMT);
            if !h.is_null() {
                let ptr_u16 = GlobalLock(h) as *const u16;
                if !ptr_u16.is_null() {
                    let mut len = 0usize;
                    while len < 131_072 && *ptr_u16.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr_u16, len);
                    let text = from_wide(slice);
                    if !text.is_empty() {
                        result = Some(text);
                    }
                    GlobalUnlock(h);
                }
            }
        }
        CloseClipboard();
        result
    }
}

/// Writes UTF-8 text to Windows clipboard and returns the new sequence number on success.
pub fn write_pc_clipboard_text(text: &str) -> Option<u32> {
    let wide = to_wide(text);
    let bytes = wide.len() * 2;
    unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            return None;
        }
        EmptyClipboard();
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if hmem.is_null() {
            CloseClipboard();
            return None;
        }
        let dst = GlobalLock(hmem) as *mut u16;
        if dst.is_null() {
            CloseClipboard();
            return None;
        }
        ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        GlobalUnlock(hmem);
        if SetClipboardData(CF_UNICODETEXT_FMT, hmem).is_null() {
            CloseClipboard();
            return None;
        }
        CloseClipboard();
        Some(GetClipboardSequenceNumber())
    }
}
