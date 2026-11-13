//! Drive letters with label, file system and used/free space for the drive list.

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};

#[derive(Clone, Debug)]
pub struct Drive {
    /// `C:\`
    pub root: String,
    pub label: String,
    pub filesystem: String,
    pub total: u64,
    pub free: u64,
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Label and file system name of a volume root (`C:\`).
fn volume_info(root: &str) -> Option<(String, String)> {
    let (mut label, mut fs) = ([0u16; 261], [0u16; 261]);
    let ok = unsafe {
        GetVolumeInformationW(
            wide(root).as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    (ok != 0).then(|| (from_wide(&label), from_wide(&fs)))
}

pub fn filesystem(root: &str) -> Option<String> {
    volume_info(root).map(|(_, fs)| fs)
}

/// (total, free) bytes of the volume holding `path`.
pub fn space(path: &str) -> Option<(u64, u64)> {
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide(path).as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}

/// Fixed, removable and network drives that are ready (an empty card reader is left out).
pub fn list() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        // 2 removable, 3 fixed, 4 network, 6 RAM disk; skip CD-ROM (5) and unknown.
        let kind = unsafe { GetDriveTypeW(wide(&root).as_ptr()) };
        if !matches!(kind, 2 | 3 | 4 | 6) {
            continue;
        }
        let Some((total, free)) = space(&root) else { continue };
        let (label, filesystem) = volume_info(&root).unwrap_or_default();
        out.push(Drive { root, label, filesystem, total, free });
    }
    out
}
