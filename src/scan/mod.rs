//! Scans a drive or folder on a background thread: reads the NTFS Master File Table directly when running as
//! administrator (whole volume in seconds), otherwise walks the folders in parallel.

pub mod mft;
pub mod walk;

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::tree::Tree;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Mft,
    Walk,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Mft => "MFT",
            Method::Walk => "Folder walk",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stats {
    pub method: Method,
    pub elapsed: Duration,
    /// Folders that could not be read (access denied), walk only.
    pub denied: u64,
    /// Folders left out by the exclusion list.
    pub skipped: u64,
    /// Why the MFT was not used, if it was wanted.
    pub note: Option<String>,
}

/// Live counters the UI reads while a scan runs.
#[derive(Default)]
pub struct Progress {
    pub items: AtomicU64,
    pub bytes: AtomicU64,
    /// Fraction done × 10000 when known (MFT), else 0.
    pub permyriad: AtomicU64,
    pub cancel: AtomicBool,
    /// The tree a folder walk is building, for drawing while it runs; `generation` is bumped per folder added.
    pub live: Mutex<Option<Arc<Mutex<Tree>>>>,
    pub generation: AtomicU64,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        let p = self.permyriad.load(Ordering::Relaxed);
        (p > 0).then(|| p as f32 / 10000.0)
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn live(&self) -> Option<Arc<Mutex<Tree>>> {
        self.live.lock().unwrap().clone()
    }
}

/// Folders not to scan: entries with a backslash are full paths, others folder names anywhere (both
/// case-insensitive), e.g. `C:\Windows\WinSxS` or `node_modules`.
#[derive(Clone, Default, Debug)]
pub struct Exclude {
    paths: Vec<String>,
    names: Vec<String>,
}

impl Exclude {
    pub fn new(list: &[String]) -> Exclude {
        let mut e = Exclude::default();
        for item in list {
            let item = item.trim().trim_matches('"').trim_end_matches('\\').to_lowercase();
            if item.is_empty() {
                continue;
            }
            if item.contains('\\') { e.paths.push(item) } else { e.names.push(item) }
        }
        e
    }
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty()
    }
    pub fn matches(&self, path: &str, name: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        let name = name.to_lowercase();
        if self.names.iter().any(|n| *n == name) {
            return true;
        }
        let path = path.trim_end_matches('\\').to_lowercase();
        self.paths.iter().any(|p| *p == path)
    }
    /// Applies the list to a finished tree (the MFT reads everything): matching folders lose their contents.
    fn prune(&self, tree: &mut Tree) -> u64 {
        if self.is_empty() {
            return 0;
        }
        let mut hits = Vec::new();
        let mut stack = vec![0u32];
        while let Some(id) = stack.pop() {
            for c in tree.children(id) {
                if !tree.nodes[c as usize].is_dir() {
                    continue;
                }
                if self.matches(&tree.path(c), tree.name(c)) {
                    hits.push(c);
                } else {
                    stack.push(c);
                }
            }
        }
        for &id in &hits {
            tree.nodes[id as usize].first_child = crate::tree::NONE;
            tree.set_flag(id, crate::tree::SKIPPED);
        }
        if !hits.is_empty() {
            tree.finish();
        }
        hits.len() as u64
    }
}

pub enum Event {
    /// The finished tree, and the node to show (a subfolder when the whole volume was read from the MFT).
    Done { tree: Tree, focus: u32, stats: Stats },
    Failed(String),
}

pub struct Job {
    pub path: String,
    pub progress: Arc<Progress>,
    pub events: Receiver<Event>,
    pub started: Instant,
}

/// `C:\` for `C:\Users\x`, if the path is on a lettered drive.
pub fn volume_root(path: &str) -> Option<String> {
    let b = path.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}

/// Normalises user input: `c:` → `C:\`, trailing backslashes removed from folders.
pub fn normalize(path: &str) -> String {
    let p = path.trim().trim_matches('"').replace('/', "\\");
    let p = p.trim_end_matches('\\');
    if p.len() == 2 && p.ends_with(':') {
        return format!("{}\\", p.to_ascii_uppercase());
    }
    let mut s = p.to_string();
    if s.as_bytes().get(1) == Some(&b':') {
        s[..1].make_ascii_uppercase();
    }
    s
}

pub fn start(path: &str, prefer_mft: bool, exclude: Exclude) -> Job {
    let path = normalize(path);
    let progress = Arc::new(Progress::default());
    let (tx, rx) = crossbeam_channel::bounded(1);
    let job = Job { path: path.clone(), progress: progress.clone(), events: rx, started: Instant::now() };
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || run(path, prefer_mft, exclude, progress, tx))
        .expect("spawn scan thread");
    job
}

fn run(path: String, prefer_mft: bool, exclude: Exclude, progress: Arc<Progress>, tx: Sender<Event>) {
    let _ = tx.send(scan(&path, prefer_mft, &exclude, &progress));
}

/// Scans synchronously (also used by `--scan`).
pub fn scan(path: &str, prefer_mft: bool, exclude: &Exclude, progress: &Progress) -> Event {
    let started = Instant::now();
    let mut note = None;
    if prefer_mft {
        match mft_volume(path) {
            Ok(volume) => match mft::scan(&volume, progress) {
                Ok(mut tree) => {
                    if progress.cancelled() {
                        return Event::Failed("Cancelled".into());
                    }
                    let skipped = exclude.prune(&mut tree);
                    let Some(focus) = tree.find(path) else {
                        return Event::Failed(format!("{path} not found on the volume"));
                    };
                    let stats = Stats { method: Method::Mft, elapsed: started.elapsed(), denied: 0, skipped, note: None };
                    log::info!("MFT scan of {volume}: {} items in {:.2} s", tree.len(), stats.elapsed.as_secs_f32());
                    return Event::Done { tree, focus, stats };
                }
                Err(e) => {
                    log::warn!("MFT scan of {volume} failed, walking folders instead: {e:#}");
                    note = Some(format!("MFT read failed: {e}"));
                    progress.items.store(0, Ordering::Relaxed);
                    progress.bytes.store(0, Ordering::Relaxed);
                    progress.permyriad.store(0, Ordering::Relaxed);
                }
            },
            Err(why) => note = Some(why),
        }
    }
    match walk::scan(path, progress, exclude) {
        Ok(r) => {
            if progress.cancelled() {
                return Event::Failed("Cancelled".into());
            }
            let stats = Stats { method: Method::Walk, elapsed: started.elapsed(), denied: r.denied, skipped: r.skipped, note };
            log::info!("Walked {path}: {} items in {:.2} s, {} folders denied, {} skipped", r.tree.len(), stats.elapsed.as_secs_f32(), r.denied, r.skipped);
            Event::Done { tree: r.tree, focus: 0, stats }
        }
        Err(e) => Event::Failed(format!("{e:#}")),
    }
}

/// The volume to read the MFT of, or why that is not possible.
fn mft_volume(path: &str) -> Result<String, String> {
    let Some(volume) = volume_root(path) else { return Err("not on a drive letter".into()) };
    if !crate::actions::is_elevated() {
        return Err("not running as administrator".into());
    }
    match crate::drives::filesystem(&volume).as_deref() {
        Some("NTFS") => Ok(volume),
        Some(fs) => Err(format!("{fs} drive, MFT needs NTFS")),
        None => Err("file system unknown".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalize() {
        assert_eq!(super::normalize("c:"), "C:\\");
        assert_eq!(super::normalize("c:\\"), "C:\\");
        assert_eq!(super::normalize("\"d:/Data/x/\""), "D:\\Data\\x");
        assert_eq!(super::volume_root("e:\\a").as_deref(), Some("E:\\"));
    }

    #[test]
    fn exclude() {
        let e = super::Exclude::new(&["C:\\Windows\\WinSxS\\".into(), "node_modules".into(), " ".into()]);
        assert!(e.matches("c:\\windows\\winsxs", "WinSxS"));
        assert!(!e.matches("D:\\Windows\\WinSxS", "WinSxS"));
        assert!(e.matches("D:\\web\\Node_Modules", "Node_Modules"));
        assert!(!e.matches("D:\\web\\src", "src"));
        assert!(super::Exclude::new(&[]).is_empty());
    }

    #[test]
    fn prune_mft_tree() {
        let mut t = crate::tree::Tree::new("C:\\");
        let w = t.add(0, "Windows", true, 0, 0, 0);
        let s = t.add(w, "WinSxS", true, 0, 0, 0);
        t.add(s, "big.dll", false, 1000, 1000, 0);
        t.add(w, "notepad.exe", false, 10, 10, 0);
        t.finish();
        let n = super::Exclude::new(&["C:\\Windows\\WinSxS".into()]).prune(&mut t);
        assert_eq!(n, 1);
        assert_eq!(t.nodes[0].size, 10);
        assert!(t.has_flag(s, crate::tree::SKIPPED));
    }
}
