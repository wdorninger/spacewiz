//! Duplicate files in a subtree: group by size, then by a hash of the first 64 KB, then by a hash of the whole
//! file (xxh3). Online-only cloud files are skipped (reading them would download them), and several names of
//! one hard-linked file are not duplicates.

use std::collections::HashMap;
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN, GetFileInformationByHandle,
};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::tree::{self, Tree};

const PARTIAL: usize = 64 * 1024;

#[derive(Default)]
pub struct Progress {
    /// Files that need hashing, and those done.
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub size: u64,
    /// Same content, largest path first is not meaningful; kept in tree order.
    pub ids: Vec<u32>,
}

impl Group {
    pub fn wasted(&self) -> u64 {
        self.size * (self.ids.len() as u64 - 1)
    }
}

pub struct Report {
    pub root: u32,
    pub groups: Vec<Group>,
    pub wasted: u64,
    /// Files left out: online-only placeholders or unreadable.
    pub skipped: u64,
}

fn is_cloud_attr(attrs: u32) -> bool {
    attrs & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_OFFLINE) != 0
}

/// Opens a file for hashing: None for cloud placeholders (checked again on disk) and unreadable files.
fn open(path: &str) -> Option<std::fs::File> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || is_cloud_attr(meta.file_attributes()) {
        return None;
    }
    std::fs::File::open(path).ok()
}

/// Volume serial + file index: equal for all names of one hard-linked file.
fn identity(f: &std::fs::File) -> Option<(u32, u64)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    (ok != 0).then(|| (info.dwVolumeSerialNumber, (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64))
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}

fn full_hash(path: &str, progress: &Progress) -> Option<u64> {
    let mut f = open(path)?;
    let mut h = Xxh3::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        progress.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    Some(h.digest())
}

/// Finds duplicate files of at least `min_size` bytes below `root`; None if cancelled.
pub fn find(tree: &Arc<Tree>, root: u32, min_size: u64, progress: &Progress) -> Option<Report> {
    let mut by_size: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut skipped = 0u64;
    for id in tree.subtree(root) {
        let n = &tree.nodes[id as usize];
        if n.is_dir() || n.flags & tree::GONE != 0 || n.size < min_size.max(1) {
            continue;
        }
        if n.flags & tree::CLOUD != 0 {
            skipped += 1;
            continue;
        }
        by_size.entry(n.size).or_default().push(id);
    }
    let candidates: Vec<(u64, u32)> = by_size.iter().filter(|(_, v)| v.len() > 1).flat_map(|(&s, v)| v.iter().map(move |&id| (s, id))).collect();
    progress.total.store(candidates.len() as u64, Ordering::Relaxed);

    // Pass 1: identity and partial hash.
    let partial: Vec<Option<(u64, u32, (u32, u64), u64)>> = candidates
        .par_iter()
        .map(|&(size, id)| {
            if progress.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let mut f = open(&tree.path(id))?;
            let ident = identity(&f)?;
            let mut buf = vec![0u8; PARTIAL.min(size as usize)];
            let n = read_up_to(&mut f, &mut buf);
            progress.done.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(n as u64, Ordering::Relaxed);
            Some((size, id, ident, xxh3_64(&buf[..n])))
        })
        .collect();
    if progress.cancel.load(Ordering::Relaxed) {
        return None;
    }
    skipped += partial.iter().filter(|p| p.is_none()).count() as u64;
    let mut by_partial: HashMap<(u64, u64), Vec<(u32, (u32, u64))>> = HashMap::new();
    for (size, id, ident, h) in partial.into_iter().flatten() {
        let v = by_partial.entry((size, h)).or_default();
        // Another name of the same file (hard link) is not a duplicate.
        if !v.iter().any(|(_, i)| *i == ident) {
            v.push((id, ident));
        }
    }
    let groups2: Vec<(u64, Vec<u32>)> = by_partial.into_iter().filter(|(_, v)| v.len() > 1).map(|((s, _), v)| (s, v.into_iter().map(|(id, _)| id).collect())).collect();

    // Pass 2: whole-file hash where the partial one didn't cover the whole file.
    let mut groups = Vec::new();
    for (size, ids) in groups2 {
        if size as usize <= PARTIAL {
            groups.push(Group { size, ids });
            continue;
        }
        let hashed: Vec<(u32, Option<u64>)> = ids.par_iter().map(|&id| (id, full_hash(&tree.path(id), progress))).collect();
        if progress.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut by_full: HashMap<u64, Vec<u32>> = HashMap::new();
        for (id, h) in hashed {
            match h {
                Some(h) => by_full.entry(h).or_default().push(id),
                None => skipped += 1,
            }
        }
        groups.extend(by_full.into_values().filter(|v| v.len() > 1).map(|ids| Group { size, ids }));
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.wasted()));
    let wasted = groups.iter().map(Group::wasted).sum();
    Some(Report { root, groups, wasted, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_duplicates_but_not_hard_links() {
        let dir = std::env::temp_dir().join(format!("spacewiz-dupes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = big.clone();
        *other.last_mut().unwrap() ^= 1; // same size and same first 64 KB, different content
        std::fs::write(dir.join("a.bin"), &big).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), &big).unwrap();
        std::fs::write(dir.join("c.bin"), &other).unwrap();
        std::fs::hard_link(dir.join("a.bin"), dir.join("a-link.bin")).unwrap();
        std::fs::write(dir.join("small1.txt"), b"hello").unwrap();
        std::fs::write(dir.join("small2.txt"), b"hello").unwrap();

        let path = dir.display().to_string();
        let progress = crate::scan::Progress::default();
        let tree = Arc::new(crate::scan::walk::scan(&path, &progress, &crate::scan::Exclude::default()).unwrap().tree);
        let p = Progress::default();
        let r = find(&tree, 0, 1, &p).unwrap();
        let names = |g: &Group| {
            let mut v: Vec<String> = g.ids.iter().map(|&i| tree.name(i).to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(r.groups.len(), 2, "{:?}", r.groups.iter().map(names).collect::<Vec<_>>());
        let g = &r.groups[0];
        assert_eq!(g.size, 200_000);
        // a.bin and a-link.bin are one file: the group is that file plus b.bin.
        assert_eq!(g.ids.len(), 2);
        assert!(names(g).contains(&"b.bin".to_string()));
        assert_eq!(names(&r.groups[1]), vec!["small1.txt", "small2.txt"]);
        assert_eq!(r.wasted, 200_005);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
