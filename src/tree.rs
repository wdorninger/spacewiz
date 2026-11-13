//! The scanned file system as an arena tree: nodes in one `Vec`, names in one string, children as linked lists
//! sorted by size (largest first) once the scan is finished.

use std::collections::HashMap;

pub const NONE: u32 = u32::MAX;

pub const DIR: u8 = 1;
/// Removed (moved to the Recycle Bin); kept in the arena so ids stay valid.
pub const GONE: u8 = 2;
/// Online-only cloud placeholder (OneDrive, Synology Drive…): reading it would download it.
pub const CLOUD: u8 = 4;
/// Folder left out by the exclusion list (not entered).
pub const SKIPPED: u8 = 8;

#[derive(Clone, Debug)]
pub struct Node {
    name_off: u32,
    name_len: u32,
    pub parent: u32,
    pub first_child: u32,
    pub next_sibling: u32,
    /// Logical size (file length), totals for folders after `finish()`.
    pub size: u64,
    /// Space on disk.
    pub alloc: u64,
    /// Files and folders below (totals after `finish()`); 0 for files.
    pub files: u32,
    pub dirs: u32,
    /// Last write time as Windows FILETIME (100 ns since 1601), 0 if unknown.
    pub mtime: u64,
    /// Newest file mtime in the subtree (the file's own for files), after `finish()`.
    pub newest: u64,
    pub ext: u16,
    pub flags: u8,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & DIR != 0
    }
    pub fn size_of(&self, allocated: bool) -> u64 {
        if allocated { self.alloc } else { self.size }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtTotal {
    pub ext: u16,
    pub size: u64,
    pub alloc: u64,
    pub count: u32,
}

#[derive(Clone, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    names: String,
    /// Extension names, index = `Node::ext`; 0 = no extension.
    pub exts: Vec<String>,
    ext_ids: HashMap<String, u16>,
    /// Absolute path of node 0 (e.g. `C:\` or `D:\Projects`).
    pub root_path: String,
    /// Per-extension totals, largest first, and the largest files, filled by `finish()`.
    pub ext_totals: Vec<ExtTotal>,
    pub top_files: Vec<u32>,
    /// Totals and child order are final (false while a live scan is still adding nodes).
    pub finished: bool,
}

impl Tree {
    pub fn new(root_path: &str) -> Tree {
        let mut t = Tree { root_path: root_path.trim_end_matches('\\').to_string(), exts: vec![String::new()], ..Default::default() };
        if t.root_path.len() == 2 && t.root_path.ends_with(':') {
            t.root_path.push('\\');
        }
        t.add_raw(root_path.trim_end_matches('\\'), NONE, DIR, 0, 0, 0);
        t
    }

    pub fn with_capacity(root_path: &str, nodes: usize) -> Tree {
        let mut t = Tree::new(root_path);
        t.nodes.reserve(nodes);
        t.names.reserve(nodes * 16);
        t
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn name(&self, id: u32) -> &str {
        let n = &self.nodes[id as usize];
        &self.names[n.name_off as usize..(n.name_off + n.name_len) as usize]
    }

    pub fn ext_name(&self, id: u32) -> &str {
        &self.exts[self.nodes[id as usize].ext as usize]
    }

    fn ext_id(&mut self, name: &str) -> u16 {
        let Some(dot) = name.rfind('.') else { return 0 };
        let ext = &name[dot + 1..];
        if dot == 0 || ext.is_empty() || ext.len() > 12 || ext.contains(' ') {
            return 0;
        }
        let ext = ext.to_ascii_lowercase();
        if let Some(&id) = self.ext_ids.get(&ext) {
            return id;
        }
        if self.exts.len() >= u16::MAX as usize {
            return 0;
        }
        let id = self.exts.len() as u16;
        self.exts.push(ext.clone());
        self.ext_ids.insert(ext, id);
        id
    }

    fn add_raw(&mut self, name: &str, parent: u32, flags: u8, size: u64, alloc: u64, mtime: u64) -> u32 {
        let id = self.nodes.len() as u32;
        let ext = if flags & DIR == 0 { self.ext_id(name) } else { 0 };
        self.nodes.push(Node {
            name_off: self.names.len() as u32,
            name_len: name.len() as u32,
            parent,
            first_child: NONE,
            next_sibling: NONE,
            size,
            alloc,
            files: 0,
            dirs: 0,
            mtime,
            newest: if flags & DIR == 0 { mtime } else { 0 },
            ext,
            flags,
        });
        self.names.push_str(name);
        id
    }

    /// Adds a node without linking it (the MFT scanner links all nodes at the end with `link`).
    pub fn push(&mut self, name: &str, is_dir: bool, size: u64, alloc: u64, mtime: u64) -> u32 {
        self.add_raw(name, NONE, if is_dir { DIR } else { 0 }, size, alloc, mtime)
    }

    pub fn link(&mut self, id: u32, parent: u32) {
        let p = &mut self.nodes[parent as usize];
        let next = p.first_child;
        p.first_child = id;
        let n = &mut self.nodes[id as usize];
        n.parent = parent;
        n.next_sibling = next;
    }

    pub fn add(&mut self, parent: u32, name: &str, is_dir: bool, size: u64, alloc: u64, mtime: u64) -> u32 {
        let id = self.push(name, is_dir, size, alloc, mtime);
        self.link(id, parent);
        id
    }

    pub fn set_flag(&mut self, id: u32, flag: u8) {
        self.nodes[id as usize].flags |= flag;
    }

    #[cfg(test)]
    pub fn has_flag(&self, id: u32, flag: u8) -> bool {
        self.nodes[id as usize].flags & flag != 0
    }

    /// Every node in the subtree of `id` (including it), depth first.
    pub fn subtree(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.children(n));
        }
        out
    }

    pub fn children(&self, id: u32) -> Children<'_> {
        Children { tree: self, next: self.nodes[id as usize].first_child }
    }

    /// Totals folders bottom-up, sorts children by size and collects extension totals and the largest files.
    pub fn finish(&mut self) {
        // Children always have larger ids than their parent in the walk; the MFT scanner may link them in any
        // order, so go by depth-first post-order instead of id order.
        let order = self.post_order(0);
        for &id in &order {
            let n = &self.nodes[id as usize];
            if !n.is_dir() || n.flags & GONE != 0 {
                continue;
            }
            let (mut size, mut alloc, mut files, mut dirs, mut newest) = (0u64, 0u64, 0u32, 0u32, 0u64);
            let mut c = n.first_child;
            while c != NONE {
                let k = &self.nodes[c as usize];
                if k.flags & GONE == 0 {
                    newest = newest.max(k.newest);
                    size += k.size;
                    alloc += k.alloc;
                    if k.is_dir() {
                        files += k.files;
                        dirs += k.dirs + 1;
                    } else {
                        files += 1;
                    }
                }
                c = k.next_sibling;
            }
            let n = &mut self.nodes[id as usize];
            n.size = size;
            n.alloc = alloc;
            n.files = files;
            n.dirs = dirs;
            n.newest = newest;
        }
        for &id in &order {
            self.sort_children(id);
        }
        self.collect_stats(&order);
        self.finished = true;
    }

    fn post_order(&self, root: u32) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(root, false)];
        while let Some((id, visited)) = stack.pop() {
            if visited {
                out.push(id);
                continue;
            }
            stack.push((id, true));
            let mut c = self.nodes[id as usize].first_child;
            while c != NONE {
                stack.push((c, false));
                c = self.nodes[c as usize].next_sibling;
            }
        }
        out
    }

    fn sort_children(&mut self, id: u32) {
        let mut kids: Vec<u32> = self.children(id).collect();
        if kids.len() < 2 {
            return;
        }
        kids.sort_by(|&a, &b| self.nodes[b as usize].size.cmp(&self.nodes[a as usize].size));
        self.nodes[id as usize].first_child = kids[0];
        for w in kids.windows(2) {
            self.nodes[w[0] as usize].next_sibling = w[1];
        }
        self.nodes[*kids.last().unwrap() as usize].next_sibling = NONE;
    }

    fn collect_stats(&mut self, order: &[u32]) {
        let mut totals: Vec<ExtTotal> = (0..self.exts.len()).map(|i| ExtTotal { ext: i as u16, ..Default::default() }).collect();
        let mut files: Vec<u32> = Vec::new();
        for &id in order {
            let n = &self.nodes[id as usize];
            if n.is_dir() || n.flags & GONE != 0 {
                continue;
            }
            let t = &mut totals[n.ext as usize];
            t.size += n.size;
            t.alloc += n.alloc;
            t.count += 1;
            files.push(id);
        }
        totals.retain(|t| t.count > 0);
        totals.sort_by(|a, b| b.size.cmp(&a.size));
        self.ext_totals = totals;
        const TOP: usize = 200;
        if files.len() > TOP {
            files.select_nth_unstable_by(TOP, |&a, &b| self.nodes[b as usize].size.cmp(&self.nodes[a as usize].size));
            files.truncate(TOP);
        }
        files.sort_by(|&a, &b| self.nodes[b as usize].size.cmp(&self.nodes[a as usize].size));
        self.top_files = files;
    }

    /// Absolute path of a node.
    pub fn path(&self, id: u32) -> String {
        let mut parts = Vec::new();
        let mut c = id;
        while c != 0 && c != NONE {
            parts.push(self.name(c));
            c = self.nodes[c as usize].parent;
        }
        let mut s = self.root_path.clone();
        for p in parts.iter().rev() {
            if !s.ends_with('\\') {
                s.push('\\');
            }
            s.push_str(p);
        }
        s
    }

    /// Ancestors from the root down to `id` (inclusive).
    pub fn chain(&self, id: u32) -> Vec<u32> {
        let mut v = vec![id];
        let mut c = self.nodes[id as usize].parent;
        while c != NONE {
            v.push(c);
            c = self.nodes[c as usize].parent;
        }
        v.reverse();
        v
    }

    /// Node at an absolute path below the root, matched case-insensitively.
    pub fn find(&self, path: &str) -> Option<u32> {
        let root = self.root_path.trim_end_matches('\\');
        let rest = path.trim_end_matches('\\');
        if rest.len() < root.len() || !rest[..root.len()].eq_ignore_ascii_case(root) {
            return None;
        }
        let mut id = 0;
        for part in rest[root.len()..].split('\\').filter(|p| !p.is_empty()) {
            id = self.children(id).find(|&c| self.name(c).eq_ignore_ascii_case(part) || self.name(c).to_lowercase() == part.to_lowercase())?;
        }
        Some(id)
    }

    /// Marks a node removed and subtracts it from all ancestors (after moving it to the Recycle Bin).
    pub fn remove(&mut self, id: u32) {
        if id == 0 || self.nodes[id as usize].flags & GONE != 0 {
            return;
        }
        let n = self.nodes[id as usize].clone();
        let (files, dirs) = if n.is_dir() { (n.files, n.dirs + 1) } else { (1, 0) };
        let parent = n.parent;
        // Unlink from the parent's child list.
        let mut prev = NONE;
        let mut c = self.nodes[parent as usize].first_child;
        while c != NONE && c != id {
            prev = c;
            c = self.nodes[c as usize].next_sibling;
        }
        if c == id {
            if prev == NONE {
                self.nodes[parent as usize].first_child = n.next_sibling;
            } else {
                self.nodes[prev as usize].next_sibling = n.next_sibling;
            }
        }
        self.nodes[id as usize].flags |= GONE;
        let mut a = parent;
        while a != NONE {
            let p = &mut self.nodes[a as usize];
            p.size = p.size.saturating_sub(n.size);
            p.alloc = p.alloc.saturating_sub(n.alloc);
            p.files = p.files.saturating_sub(files);
            p.dirs = p.dirs.saturating_sub(dirs);
            a = p.parent;
        }
        self.recount_stats();
    }

    /// Replaces the folder `id` with a freshly scanned tree of the same folder (Rescan folder).
    pub fn replace_subtree(&mut self, id: u32, sub: &Tree) {
        let old = self.nodes[id as usize].clone();
        // Detach old children (they stay in the arena, unreachable).
        self.nodes[id as usize].first_child = NONE;
        let mut map = vec![NONE; sub.nodes.len()];
        map[0] = id;
        for sid in sub.post_order(0).into_iter().rev() {
            if sid == 0 {
                continue;
            }
            let s = &sub.nodes[sid as usize];
            let parent = map[s.parent as usize];
            let new = self.add(parent, sub.name(sid), s.is_dir(), s.size, s.alloc, s.mtime);
            let n = &mut self.nodes[new as usize];
            n.files = s.files;
            n.dirs = s.dirs;
            n.newest = s.newest;
            n.flags |= s.flags & (CLOUD | SKIPPED);
            map[sid as usize] = new;
        }
        let r = &sub.nodes[0];
        let (ds, da) = (r.size as i128 - old.size as i128, r.alloc as i128 - old.alloc as i128);
        let (df, dd) = (r.files as i64 - old.files as i64, r.dirs as i64 - old.dirs as i64);
        let mut a = id;
        while a != NONE {
            let p = &mut self.nodes[a as usize];
            p.size = (p.size as i128 + ds).max(0) as u64;
            p.alloc = (p.alloc as i128 + da).max(0) as u64;
            p.files = (p.files as i64 + df).max(0) as u32;
            p.dirs = (p.dirs as i64 + dd).max(0) as u32;
            a = p.parent;
        }
        // Re-sort along the chain (sizes changed) and inside the new subtree; refresh newest times upward.
        self.nodes[id as usize].newest = r.newest;
        for a in self.chain(id).into_iter().rev().skip(1) {
            let newest = self.children(a).map(|c| self.nodes[c as usize].newest).max().unwrap_or(0);
            self.nodes[a as usize].newest = newest;
        }
        for a in self.chain(id) {
            self.sort_children(a);
        }
        for n in self.post_order(id) {
            self.sort_children(n);
        }
        self.recount_stats();
    }

    fn recount_stats(&mut self) {
        let order = self.post_order(0);
        self.collect_stats(&order);
    }
}

pub struct Children<'a> {
    tree: &'a Tree,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.next == NONE {
            return None;
        }
        let id = self.next;
        self.next = self.tree.nodes[id as usize].next_sibling;
        Some(id)
    }
}

/// "1.23 GB" with binary units like Explorer.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 { format!("{v:.0} {}", UNITS[u]) } else if v >= 10.0 { format!("{v:.1} {}", UNITS[u]) } else { format!("{v:.2} {}", UNITS[u]) }
}

/// "12,345,678" for counts.
pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// FILETIME to local date and time, empty if unknown.
pub fn format_time(filetime: u64) -> String {
    if filetime == 0 {
        return String::new();
    }
    let secs = (filetime / 10_000_000) as i64 - 11_644_473_600;
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tree {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "a", true, 0, 0, 0);
        t.add(a, "x.txt", false, 100, 4096, 0);
        t.add(a, "y.BIN", false, 300, 4096, 0);
        let b = t.add(0, "b", true, 0, 0, 0);
        t.add(b, "z.txt", false, 1000, 4096, 0);
        t.add(0, "root.dat", false, 50, 4096, 0);
        t.finish();
        t
    }

    #[test]
    fn totals_and_order() {
        let t = sample();
        assert_eq!(t.nodes[0].size, 1450);
        assert_eq!(t.nodes[0].files, 4);
        assert_eq!(t.nodes[0].dirs, 2);
        let kids: Vec<&str> = t.children(0).map(|c| t.name(c)).collect();
        assert_eq!(kids, ["b", "a", "root.dat"]);
        assert_eq!(t.exts[t.ext_totals[0].ext as usize], "txt");
        assert_eq!(t.ext_totals[0].size, 1100);
        assert_eq!(t.name(t.top_files[0]), "z.txt");
        assert_eq!(t.ext_name(t.find("C:\\a\\y.bin").unwrap()), "bin");
    }

    #[test]
    fn newest_propagates() {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "a", true, 0, 0, 5);
        t.add(a, "x", false, 1, 1, 100);
        t.add(0, "y", false, 1, 1, 50);
        t.finish();
        assert_eq!(t.nodes[a as usize].newest, 100);
        assert_eq!(t.nodes[0].newest, 100);
        assert!(t.finished);
    }

    #[test]
    fn path_find_remove() {
        let mut t = sample();
        let y = t.find("c:\\A\\y.bin").unwrap();
        assert_eq!(t.path(y), "C:\\a\\y.BIN");
        t.remove(y);
        assert_eq!(t.nodes[0].size, 1150);
        assert_eq!(t.nodes[0].files, 3);
        assert!(t.find("C:\\a\\y.bin").is_none());
    }

    #[test]
    fn replace_subtree() {
        let mut t = sample();
        let a = t.find("C:\\a").unwrap();
        let mut sub = Tree::new("C:\\a");
        sub.add(0, "new.txt", false, 5000, 8192, 0);
        sub.finish();
        t.replace_subtree(a, &sub);
        assert_eq!(t.nodes[0].size, 6050);
        assert_eq!(t.name(t.children(0).next().unwrap()), "a");
        assert!(t.find("C:\\a\\new.txt").is_some());
    }

    #[test]
    fn formatting() {
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_count(1234567), "1,234,567");
    }
}
