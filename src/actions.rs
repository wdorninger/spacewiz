//! Shell actions: open, show in Explorer, move to the Recycle Bin, and restarting elevated for the MFT scan.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// Whether the process runs with administrator rights (needed to read the MFT).
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, &mut elevation as *mut _ as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Whether this is an administrator account running with a filtered (UAC) token, so a restart "as
/// administrator" works with a simple consent prompt. False for standard users, who would need someone
/// else's admin password.
pub fn can_elevate() -> bool {
    use windows_sys::Win32::Security::{TOKEN_ELEVATION_TYPE, TokenElevationType, TokenElevationTypeLimited};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut kind: TOKEN_ELEVATION_TYPE = 0;
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevationType, &mut kind as *mut _ as *mut _, std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && kind == TokenElevationTypeLimited
    }
}

fn shell_execute(verb: &str, file: &std::ffi::OsStr, params: &str) -> bool {
    let (verb, file, params) = (wide(verb.as_ref()), wide(file), wide(params.as_ref()));
    let result = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), SW_SHOWNORMAL) };
    result as isize > 32
}

/// Starts this program again as administrator (UAC prompt), opening `path`. False if the user said no.
pub fn restart_elevated(path: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    shell_execute("runas", exe.as_os_str(), &format!("\"{}\"", path.trim_end_matches('\\')))
}

/// Opens a file or folder like a double click in Explorer.
pub fn shell_open(path: &Path) {
    shell_execute("open", path.as_os_str(), "");
}

/// Opens Explorer with the item selected.
pub fn show_in_explorer(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn();
}

/// Moves a file or folder to the Recycle Bin without Windows' own prompt (the app asks first).
pub fn recycle(path: &Path) -> Result<(), String> {
    // pFrom is a double-null-terminated list.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        Err(format!("could not move to the Recycle Bin (error 0x{code:x})"))
    } else if op.fAnyOperationsAborted != 0 {
        Err("cancelled".into())
    } else if path.exists() {
        Err("the item is still there (in use?)".into())
    } else {
        Ok(())
    }
}
