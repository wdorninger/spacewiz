//! Parallel folder walk with `FindFirstFileExW` (basic info, large fetch): works without administrator rights
//! and on any file system. Junctions and symbolic links to folders are not followed, so nothing counts twice.
//! Entries go straight into a shared tree (one lock per folder) with running totals up the parent chain, so
//! the window can draw the chart while the scan is still going.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use rayon::prelude::*;
use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDiskFreeSpaceW, WIN32_FIND_DATAW,
};

use super::{Exclude, Progress};
use crate::tree::{self, NONE, Tree};

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
}

/// `\\?\` form so paths longer than 260 characters work.
fn long_path(path: &str) -> String {
    if path.starts_with("\\\\?\\") {
        path.to_string()
    } else if let Some(unc) = path.strip_prefix("\\\\") {
        format!("\\\\?\\UNC\\{unc}")
    } else {
        format!("\\\\?\\{path}")
    }
}

fn filetime(t: FILETIME) -> u64 {
    (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64
}

fn cluster_size(path: &str) -> u64 {
    let Some(root) = super::volume_root(path) else { return 4096 };
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    let ok = unsafe { GetDiskFreeSpaceW(wide(&root).as_ptr(), &mut spc, &mut bps, &mut free, &mut total) };
    if ok != 0 && spc > 0 && bps > 0 { spc as u64 * bps as u64 } else { 4096 }
}

struct Item {
    name: String,
    is_dir: bool,
    size: u64,
    alloc: u64,
    mtime: u64,
    flags: u8,
    descend: bool,
}

struct Walker<'a> {
    progress: &'a Progress,
    tree: &'a Mutex<Tree>,
    exclude: &'a Exclude,
    cluster: u64,
    denied: AtomicU64,
    skipped: AtomicU64,
}

impl Walker<'_> {
    /// Lists one folder into the tree, then walks its subfolders in parallel.
    fn list(&self, dir: &str, id: u32) {
        if self.progress.cancelled() {
            return;
        }
        let items = self.read_dir(dir);
        let (mut bytes, mut alloc, mut files, mut dirs, mut newest) = (0u64, 0u64, 0u32, 0u32, 0u64);
        let mut subdirs = Vec::new();
        {
            let mut t = self.tree.lock().unwrap();
            for it in &items {
                let child = t.add(id, &it.name, it.is_dir, it.size, it.alloc, it.mtime);
                if it.flags != 0 {
                    t.set_flag(child, it.flags);
                }
                if it.is_dir {
                    dirs += 1;
                    if it.descend {
                        subdirs.push((child, it.name.clone()));
                    }
                } else {
                    files += 1;
                    bytes += it.size;
                    alloc += it.alloc;
                    newest = newest.max(it.mtime);
                }
            }
            // Running totals up the chain; `finish()` recomputes them exactly at the end.
            let mut a = id;
            while a != NONE {
                let n = &mut t.nodes[a as usize];
                n.size += bytes;
                n.alloc += alloc;
                n.files += files;
                n.dirs += dirs;
                n.newest = n.newest.max(newest);
                a = n.parent;
            }
        }
        self.progress.items.fetch_add(items.len() as u64, Ordering::Relaxed);
        self.progress.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.progress.generation.fetch_add(1, Ordering::Relaxed);
        let base = dir.trim_end_matches('\\');
        subdirs.into_par_iter().for_each(|(child, name)| self.list(&format!("{base}\\{name}"), child));
    }

    fn read_dir(&self, dir: &str) -> Vec<Item> {
        let pattern = wide(&format!("{}\\*", long_path(dir).trim_end_matches('\\')));
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let handle = unsafe {
            FindFirstFileExW(pattern.as_ptr(), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, std::ptr::null(), FIND_FIRST_EX_LARGE_FETCH)
        };
        if handle == INVALID_HANDLE_VALUE {
            self.denied.fetch_add(1, Ordering::Relaxed);
            return Vec::new();
        }
        let base = dir.trim_end_matches('\\');
        let mut items = Vec::new();
        loop {
            let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
            let name = OsString::from_wide(&data.cFileName[..len]).to_string_lossy().into_owned();
            if name != "." && name != ".." {
                let attrs = data.dwFileAttributes;
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                let size = if is_dir { 0 } else { (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64 };
                let cloud = attrs & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_OFFLINE) != 0;
                // Online-only files take no space on this disk.
                let alloc = if cloud { 0 } else { size.div_ceil(self.cluster) * self.cluster };
                let mut flags = if cloud && !is_dir { tree::CLOUD } else { 0 };
                // A reparse folder (junction, symlink, mount point) is shown but not entered.
                let mut descend = is_dir && attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0;
                if is_dir && self.exclude.matches(&format!("{base}\\{name}"), &name) {
                    flags |= tree::SKIPPED;
                    descend = false;
                    self.skipped.fetch_add(1, Ordering::Relaxed);
                }
                items.push(Item { name, is_dir, size, alloc, mtime: filetime(data.ftLastWriteTime), flags, descend });
            }
            if unsafe { FindNextFileW(handle, &mut data) } == 0 {
                break;
            }
        }
        unsafe { FindClose(handle) };
        items
    }
}

pub struct WalkResult {
    pub tree: Tree,
    pub denied: u64,
    pub skipped: u64,
}

/// Walks `path`; the tree being built is published in `progress.live` for the window to draw meanwhile.
pub fn scan(path: &str, progress: &Progress, exclude: &Exclude) -> Result<WalkResult> {
    let meta = std::fs::metadata(path);
    if !meta.as_ref().is_ok_and(|m| m.is_dir()) {
        bail!("{path} is not a folder that can be read");
    }
    let live = Arc::new(Mutex::new(Tree::new(path)));
    *progress.live.lock().unwrap() = Some(live.clone());
    let walker = Walker { progress, tree: &live, exclude, cluster: cluster_size(path), denied: AtomicU64::new(0), skipped: AtomicU64::new(0) };
    walker.list(path, 0);
    let mut guard = live.lock().unwrap();
    guard.finish();
    // Hand the finished tree over; the window draws nothing from the emptied live tree (it checks the length).
    let tree = std::mem::take(&mut *guard);
    drop(guard);
    *progress.live.lock().unwrap() = None;
    Ok(WalkResult { tree, denied: walker.denied.load(Ordering::Relaxed), skipped: walker.skipped.load(Ordering::Relaxed) })
}
