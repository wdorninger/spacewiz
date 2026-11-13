//! Reads the NTFS Master File Table straight from the volume (needs administrator rights), the way WizTree
//! does: one sequential read of the whole table in large chunks, records parsed in parallel, then linked into a
//! tree by parent record number. Only what a size view needs is parsed: the name and parent from
//! `$FILE_NAME`, the last write time from `$STANDARD_INFORMATION`, and the sizes of the unnamed `$DATA` stream.

use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

use super::Progress;
use crate::tree::Tree;

const ROOT_RECORD: u64 = 5;
const CHUNK: usize = 8 << 20;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

struct Volume(HANDLE);

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Volume {
    fn open(root: &str) -> Result<Volume> {
        let letter = root.trim_end_matches('\\').trim_end_matches(':');
        let path: Vec<u16> = std::ffi::OsStr::new(&format!("\\\\.\\{letter}:")).encode_wide().chain([0]).collect();
        let h = unsafe {
            CreateFileW(path.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN, std::ptr::null_mut())
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("open volume");
        }
        Ok(Volume(h))
    }

    fn ntfs_data(&self) -> Result<NTFS_VOLUME_DATA_BUFFER> {
        let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                &mut data as *mut _ as *mut _,
                std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("FSCTL_GET_NTFS_VOLUME_DATA");
        }
        Ok(data)
    }

    /// Reads exactly `buf.len()` bytes at `offset` (both multiples of the sector size).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), FILE_BEGIN) } == 0 {
            return Err(std::io::Error::last_os_error()).context("seek");
        }
        let mut done = 0usize;
        while done < buf.len() {
            let want = (buf.len() - done).min(64 << 20) as u32;
            let mut got = 0u32;
            let ok = unsafe { ReadFile(self.0, buf[done..].as_mut_ptr(), want, &mut got, std::ptr::null_mut()) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error()).context(format!("read at {}", offset + done as u64));
            }
            if got == 0 {
                bail!("unexpected end of volume at {}", offset + done as u64);
            }
            done += got as usize;
        }
        Ok(())
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// Applies the update sequence array (the last two bytes of every 512-byte sector are stored in the header
/// and replaced by a check value on disk). False if the record is torn or not a FILE record.
pub fn apply_fixups(rec: &mut [u8]) -> bool {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return false;
    }
    let usa = u16_at(rec, 4) as usize;
    let count = u16_at(rec, 6) as usize;
    if count == 0 || usa + count * 2 > rec.len() || (count - 1) * 512 > rec.len() {
        return false;
    }
    let check = [rec[usa], rec[usa + 1]];
    for i in 1..count {
        let end = i * 512 - 2;
        if rec[end..end + 2] != check {
            return false;
        }
        rec[end] = rec[usa + i * 2];
        rec[end + 1] = rec[usa + i * 2 + 1];
    }
    true
}

/// Data runs of a non-resident attribute as (start cluster, cluster count); sparse runs have no start.
pub fn decode_runs(mut b: &[u8]) -> Vec<(Option<u64>, u64)> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    while let Some(&header) = b.first() {
        if header == 0 {
            break;
        }
        let (len_size, off_size) = ((header & 0x0F) as usize, (header >> 4) as usize);
        if len_size == 0 || len_size > 8 || off_size > 8 || b.len() < 1 + len_size + off_size {
            break;
        }
        let mut len = 0u64;
        for i in 0..len_size {
            len |= (b[1 + i] as u64) << (8 * i);
        }
        if off_size == 0 {
            runs.push((None, len));
        } else {
            let mut off = 0i64;
            for i in 0..off_size {
                off |= (b[1 + len_size + i] as i64) << (8 * i);
            }
            // Sign-extend the relative offset.
            let shift = 64 - 8 * off_size as u32;
            off = (off << shift) >> shift;
            lcn += off;
            runs.push((Some(lcn as u64), len));
        }
        b = &b[1 + len_size + off_size..];
    }
    runs
}

#[derive(Default, Debug, Clone)]
pub struct Record {
    /// Record this one extends (0 for base records).
    pub base: u64,
    pub is_dir: bool,
    pub parent: u64,
    pub name: Option<String>,
    /// Namespace rank of `name`: 3 = Win32, 2 = POSIX, 1 = DOS 8.3 only.
    pub name_rank: u8,
    pub data: Option<(u64, u64)>,
    pub mtime: u64,
    /// Online-only cloud placeholder (recall/offline attributes in $STANDARD_INFORMATION).
    pub cloud: bool,
}

/// Parses one FILE record (fixups already applied). None for records not in use.
pub fn parse_record(rec: &[u8]) -> Option<Record> {
    let flags = u16_at(rec, 22);
    if flags & 1 == 0 {
        return None;
    }
    let mut r = Record { base: u64_at(rec, 32) & 0xFFFF_FFFF_FFFF, is_dir: flags & 2 != 0, ..Default::default() };
    let mut off = u16_at(rec, 20) as usize;
    while off + 16 <= rec.len() {
        let kind = u32_at(rec, off);
        let len = u32_at(rec, off + 4) as usize;
        if kind == ATTR_END || len < 16 || off + len > rec.len() {
            break;
        }
        let a = &rec[off..off + len];
        let non_resident = a[8] != 0;
        let name_len = a[9];
        let value = || -> &[u8] {
            let (vlen, voff) = (u32_at(a, 16) as usize, u16_at(a, 20) as usize);
            a.get(voff..voff + vlen).unwrap_or(&[])
        };
        match kind {
            ATTR_STANDARD_INFORMATION if !non_resident => {
                let v = value();
                r.mtime = u64_at(v, 8);
                // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE
                r.cloud = u32_at(v, 0x20) & (0x40_0000 | 0x4_0000 | 0x1000) != 0;
            }
            ATTR_FILE_NAME if !non_resident => {
                let v = value();
                let n = *v.get(0x40).unwrap_or(&0) as usize;
                let rank = match v.get(0x41) {
                    Some(1) | Some(3) => 3,
                    Some(0) => 2,
                    _ => 1,
                };
                if rank > r.name_rank && v.len() >= 0x42 + n * 2 {
                    let units: Vec<u16> = (0..n).map(|i| u16_at(v, 0x42 + i * 2)).collect();
                    r.name = Some(String::from_utf16_lossy(&units));
                    r.parent = u64_at(v, 0) & 0xFFFF_FFFF_FFFF;
                    r.name_rank = rank;
                }
            }
            ATTR_DATA if name_len == 0 => {
                if non_resident {
                    // Only the first extent (VCN 0) carries the sizes.
                    if u64_at(a, 16) == 0 {
                        let compressed = u16_at(a, 12) & 0x00FF != 0;
                        let alloc = if compressed && u16_at(a, 32) >= 72 { u64_at(a, 64) } else { u64_at(a, 40) };
                        r.data = Some((u64_at(a, 48), alloc));
                    }
                } else {
                    r.data = Some((u32_at(a, 16) as u64, 0));
                }
            }
            _ => {}
        }
        off += len;
    }
    Some(r)
}

/// Merges an extension record into its base record.
fn merge(into: &mut Record, ext: Record) {
    if ext.name_rank > into.name_rank {
        into.name = ext.name;
        into.parent = ext.parent;
        into.name_rank = ext.name_rank;
    }
    if into.data.is_none() {
        into.data = ext.data;
    }
    if into.mtime == 0 {
        into.mtime = ext.mtime;
    }
    into.cloud |= ext.cloud;
}

/// Reads and parses the whole MFT of an NTFS volume (`C:\`) into a tree rooted at the volume.
pub fn scan(root: &str, progress: &Progress) -> Result<Tree> {
    let vol = Volume::open(root)?;
    let info = vol.ntfs_data()?;
    let cluster = info.BytesPerCluster as u64;
    let rec_size = info.BytesPerFileRecordSegment as usize;
    if cluster == 0 || !(512..=65536).contains(&rec_size) {
        bail!("unexpected NTFS geometry (cluster {cluster}, record {rec_size})");
    }
    let mft_start = info.MftStartLcn as u64 * cluster;
    let mft_bytes = info.MftValidDataLength as u64;
    let total = (mft_bytes / rec_size as u64) as usize;

    // Record 0 is $MFT itself: its $DATA runs say where the rest of the table is.
    let mut rec0 = vec![0u8; rec_size.max(info.BytesPerSector as usize)];
    vol.read_at(mft_start, &mut rec0)?;
    rec0.truncate(rec_size);
    if !apply_fixups(&mut rec0) {
        bail!("$MFT record is not valid");
    }
    let runs = mft_runs(&rec0).context("$MFT data runs")?;

    let mut records: Vec<Option<Record>> = Vec::with_capacity(total);
    let mut buf = vec![0u8; CHUNK];
    'runs: for (lcn, clusters) in runs {
        let Some(lcn) = lcn else { bail!("sparse run in $MFT") };
        let mut offset = lcn * cluster;
        let mut left = clusters * cluster;
        while left > 0 && records.len() < total {
            if progress.cancelled() {
                break 'runs;
            }
            let len = left.min(CHUNK as u64) as usize;
            let chunk = &mut buf[..len];
            vol.read_at(offset, chunk)?;
            let want = ((len / rec_size).min(total - records.len())) * rec_size;
            let parsed: Vec<Option<Record>> = chunk[..want]
                .par_chunks_mut(rec_size)
                .map(|rec| if apply_fixups(rec) { parse_record(rec) } else { None })
                .collect();
            let files = parsed.iter().flatten().filter(|r| r.base == 0 && !r.is_dir).count() as u64;
            let bytes: u64 = parsed.iter().flatten().filter_map(|r| r.data.map(|d| d.0)).sum();
            records.extend(parsed);
            progress.items.fetch_add(files, Ordering::Relaxed);
            progress.bytes.fetch_add(bytes, Ordering::Relaxed);
            progress.permyriad.store((records.len() as u64 * 9000 / total.max(1) as u64).max(1), Ordering::Relaxed);
            offset += len as u64;
            left -= len as u64;
        }
    }
    if progress.cancelled() {
        return Ok(Tree::new(root));
    }
    Ok(build_tree(root, records))
}

fn mft_runs(rec0: &[u8]) -> Option<Vec<(Option<u64>, u64)>> {
    let mut off = u16_at(rec0, 20) as usize;
    while off + 16 <= rec0.len() {
        let kind = u32_at(rec0, off);
        let len = u32_at(rec0, off + 4) as usize;
        if kind == ATTR_END || len < 16 || off + len > rec0.len() {
            break;
        }
        let a = &rec0[off..off + len];
        if kind == ATTR_DATA && a[8] != 0 && a[9] == 0 {
            return Some(decode_runs(&a[u16_at(a, 32) as usize..]));
        }
        off += len;
    }
    None
}

/// Links parsed records into a tree: record 5 is the root folder; records without a reachable parent are dropped.
pub fn build_tree(root: &str, mut records: Vec<Option<Record>>) -> Tree {
    // Fold extension records into their base records.
    for i in 0..records.len() {
        let base = match &records[i] {
            Some(r) if r.base != 0 && (r.base as usize) < records.len() && r.base as usize != i => r.base as usize,
            _ => continue,
        };
        let ext = records[i].take().unwrap();
        if let Some(b) = records[base].as_mut() {
            merge(b, ext);
        }
    }
    let count = records.iter().flatten().count();
    let mut tree = Tree::with_capacity(root, count);
    let mut ids = vec![crate::tree::NONE; records.len()];
    if (ROOT_RECORD as usize) < ids.len() {
        ids[ROOT_RECORD as usize] = 0;
        if let Some(Some(r)) = records.get(ROOT_RECORD as usize) {
            tree.nodes[0].mtime = r.mtime;
        }
    }
    for (i, r) in records.iter().enumerate() {
        let Some(r) = r else { continue };
        if i as u64 == ROOT_RECORD || r.base != 0 {
            continue;
        }
        let Some(name) = &r.name else { continue };
        let (size, alloc) = if r.is_dir { (0, 0) } else { r.data.unwrap_or((0, 0)) };
        ids[i] = tree.push(name, r.is_dir, size, alloc, r.mtime);
        if r.cloud && !r.is_dir {
            tree.set_flag(ids[i], crate::tree::CLOUD);
        }
    }
    for (i, r) in records.iter().enumerate() {
        let id = ids[i];
        if id == crate::tree::NONE || id == 0 {
            continue;
        }
        let parent = r.as_ref().unwrap().parent as usize;
        if parent < ids.len() && ids[parent] != crate::tree::NONE && parent != i && records[parent].as_ref().is_some_and(|p| p.is_dir) {
            tree.link(id, ids[parent]);
        }
    }
    tree.finish();
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1024-byte FILE record with the given attributes, fixups applied as on disk.
    fn record(flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&48u16.to_le_bytes()); // update sequence array offset
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // 1 check value + 2 sectors
        r[20..22].copy_from_slice(&56u16.to_le_bytes());
        r[22..24].copy_from_slice(&flags.to_le_bytes());
        r[32..40].copy_from_slice(&base.to_le_bytes());
        let mut off = 56;
        for a in attrs {
            r[off..off + a.len()].copy_from_slice(a);
            off += a.len();
        }
        r[off..off + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        // Store the real sector tails in the array and put the check value there.
        r[48..50].copy_from_slice(&[0xAB, 0xCD]);
        for i in 1..3 {
            let end = i * 512 - 2;
            let (a, b) = (r[end], r[end + 1]);
            r[48 + i * 2] = a;
            r[48 + i * 2 + 1] = b;
            r[end] = 0xAB;
            r[end + 1] = 0xCD;
        }
        r
    }

    fn resident(kind: u32, name_len: u8, value: &[u8]) -> Vec<u8> {
        let len = (24 + value.len() + 7) & !7;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[9] = name_len;
        a[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[20..22].copy_from_slice(&24u16.to_le_bytes());
        a[24..24 + value.len()].copy_from_slice(value);
        a
    }

    fn file_name(parent: u64, name: &str, namespace: u8) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 0x42 + units.len() * 2];
        v[0..8].copy_from_slice(&(parent | 1 << 48).to_le_bytes()); // sequence number in the top bits
        v[0x40] = units.len() as u8;
        v[0x41] = namespace;
        for (i, u) in units.iter().enumerate() {
            v[0x42 + i * 2..0x44 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        resident(ATTR_FILE_NAME, 0, &v)
    }

    fn non_resident_data(real: u64, alloc: u64, runs: &[u8]) -> Vec<u8> {
        let len = (64 + runs.len() + 7) & !7;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[32..34].copy_from_slice(&64u16.to_le_bytes());
        a[40..48].copy_from_slice(&alloc.to_le_bytes());
        a[48..56].copy_from_slice(&real.to_le_bytes());
        a[64..64 + runs.len()].copy_from_slice(runs);
        a
    }

    #[test]
    fn fixups() {
        let mut r = record(1, 0, &[]);
        r[510] = 0xAB; // already the check value: fine
        assert!(apply_fixups(&mut r));
        let mut torn = record(1, 0, &[]);
        torn[1022] = 0;
        assert!(!apply_fixups(&mut torn));
        let mut not_file = record(1, 0, &[]);
        not_file[0] = b'B';
        assert!(!apply_fixups(&mut not_file));
    }

    #[test]
    fn runs() {
        // 0x21: 1-byte length, 2-byte offset. Second run goes backwards (negative offset), third is sparse.
        let runs = decode_runs(&[0x21, 0x10, 0x00, 0x01, 0x21, 0x08, 0x00, 0xFF, 0x01, 0x04, 0x00]);
        assert_eq!(runs, vec![(Some(0x100), 0x10), (Some(0x100 - 0x100), 8), (None, 4)]);
        let big = decode_runs(&[0x31, 0x20, 0x00, 0x00, 0x10, 0x00]);
        assert_eq!(big, vec![(Some(0x100000), 0x20)]);
    }

    #[test]
    fn parse_and_build() {
        let mut si = vec![0u8; 48];
        si[8..16].copy_from_slice(&132_000_000_000_000_000u64.to_le_bytes());
        let mut root = record(3, 0, &[resident(ATTR_STANDARD_INFORMATION, 0, &si), file_name(5, ".", 3)]);
        let mut dir = record(3, 0, &[file_name(5, "Users", 3)]);
        let mut file = record(1, 0, &[
            resident(ATTR_STANDARD_INFORMATION, 0, &si),
            file_name(64, "BIGFIL~1.ISO", 2),
            file_name(64, "big file.iso", 1),
            non_resident_data(5_000_000, 5_001_216, &[0x21, 0x10, 0x00, 0x01, 0x00]),
        ]);
        let mut small = record(1, 0, &[file_name(64, "a.txt", 3), resident(ATTR_DATA, 0, b"hello")]);
        // Named stream (ADS) must not count.
        let mut ads = record(1, 0, &[file_name(5, "x.bin", 3), resident(ATTR_DATA, 4, b"zzzzzzzzzz"), resident(ATTR_DATA, 0, b"1")]);
        let mut unused = record(0, 0, &[file_name(5, "deleted.txt", 3)]);
        for r in [&mut root, &mut dir, &mut file, &mut small, &mut ads, &mut unused] {
            assert!(apply_fixups(r));
        }
        let f = parse_record(&file).unwrap();
        assert_eq!(f.name.as_deref(), Some("big file.iso"));
        assert_eq!(f.parent, 64);
        assert_eq!(f.data, Some((5_000_000, 5_001_216)));
        assert_eq!(f.mtime, 132_000_000_000_000_000);
        assert!(parse_record(&unused).is_none());

        let mut records: Vec<Option<Record>> = vec![None; 70];
        records[5] = parse_record(&root);
        records[64] = parse_record(&dir);
        records[65] = parse_record(&file);
        records[66] = parse_record(&small);
        records[67] = parse_record(&ads);
        let t = build_tree("C:\\", records);
        assert_eq!(t.nodes[0].size, 5_000_006);
        assert_eq!(t.nodes[0].files, 3);
        let users = t.find("C:\\Users").unwrap();
        assert_eq!(t.nodes[users as usize].size, 5_000_005);
        assert_eq!(t.path(t.top_files[0]), "C:\\Users\\big file.iso");
    }
}
