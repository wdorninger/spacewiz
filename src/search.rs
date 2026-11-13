//! Search: names with `*`/`?` wildcards or plain substrings (case-insensitive), several terms separated by
//! spaces or `;` (any may match), optional size filters `>100MB` `<1KB` and `file:` / `folder:` to restrict
//! the kind. Runs on a worker thread over the focused subtree.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::chart::Highlight;
use crate::tree::{self, Tree};

/// At most this many matches are listed (all are counted and highlighted).
pub const LIST_MAX: usize = 10_000;

#[derive(Debug, Clone, PartialEq)]
enum Pattern {
    Glob(Vec<char>),
    Sub(Vec<char>),
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    terms: Vec<Pattern>,
    min: Option<u64>,
    max: Option<u64>,
    dirs: Option<bool>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// "100MB", "1.5g", "200" (bytes) → bytes.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f64 = num.parse().ok()?;
    let mult = match unit.trim().trim_end_matches('b') {
        "" => 1u64,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return None,
    };
    Some((v * mult as f64) as u64)
}

impl Query {
    /// None for an empty or meaningless query.
    pub fn parse(text: &str) -> Option<Query> {
        let mut q = Query::default();
        for word in text.split(|c: char| c == ';' || c.is_whitespace()).filter(|w| !w.is_empty()) {
            let lw = word.to_lowercase();
            if let Some(rest) = lw.strip_prefix(">=").or_else(|| lw.strip_prefix('>')) {
                q.min = Some(parse_size(rest)?);
            } else if let Some(rest) = lw.strip_prefix("<=").or_else(|| lw.strip_prefix('<')) {
                q.max = Some(parse_size(rest)?);
            } else if lw == "file:" || lw == "files:" {
                q.dirs = Some(false);
            } else if lw == "folder:" || lw == "folders:" || lw == "dir:" {
                q.dirs = Some(true);
            } else {
                let (kind, rest) = if let Some(r) = lw.strip_prefix("file:") {
                    (Some(false), r)
                } else if let Some(r) = lw.strip_prefix("folder:") {
                    (Some(true), r)
                } else {
                    (None, lw.as_str())
                };
                if kind.is_some() {
                    q.dirs = kind;
                }
                if rest.is_empty() {
                    continue;
                }
                let chars: Vec<char> = rest.chars().map(lower).collect();
                q.terms.push(if rest.contains(['*', '?']) { Pattern::Glob(chars) } else { Pattern::Sub(chars) });
            }
        }
        (!q.terms.is_empty() || q.min.is_some() || q.max.is_some() || q.dirs.is_some()).then_some(q)
    }

    pub fn matches_name(&self, name: &str) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        let text: Vec<char> = name.chars().map(lower).collect();
        self.terms.iter().any(|t| match t {
            Pattern::Glob(p) => glob(p, &text),
            Pattern::Sub(p) => p.len() <= text.len() && text.windows(p.len()).any(|w| w == p.as_slice()),
        })
    }

    fn matches(&self, tree: &Tree, id: u32, allocated: bool) -> bool {
        let n = &tree.nodes[id as usize];
        if n.flags & tree::GONE != 0 || self.dirs.is_some_and(|d| d != n.is_dir()) {
            return false;
        }
        let size = n.size_of(allocated);
        if self.min.is_some_and(|m| size < m) || self.max.is_some_and(|m| size > m) {
            return false;
        }
        self.matches_name(tree.name(id))
    }
}

/// `*` = any run of characters, `?` = one character; both sides already lowercase.
pub fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

pub struct Results {
    pub text: String,
    /// Largest first, at most `LIST_MAX`.
    pub ids: Vec<u32>,
    pub count: usize,
    /// Total size of the matches (nested matches counted once: a match inside a matching folder is not added).
    pub bytes: u64,
    pub highlight: Highlight,
}

/// Searches the subtree of `root`; None if cancelled.
pub fn run(tree: &Arc<Tree>, root: u32, text: &str, query: &Query, allocated: bool, cancel: &AtomicBool) -> Option<Results> {
    let mut ids = Vec::new();
    let mut bytes = 0u64;
    // (node, inside a matching folder)
    let mut stack = vec![(root, false)];
    let mut n = 0usize;
    while let Some((id, inside)) = stack.pop() {
        n += 1;
        if n % 65_536 == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let hit = id != root && query.matches(tree, id, allocated);
        if hit {
            ids.push(id);
            if !inside {
                bytes += tree.nodes[id as usize].size_of(allocated);
            }
        }
        for c in tree.children(id) {
            stack.push((c, inside || hit));
        }
    }
    let count = ids.len();
    let highlight = Highlight::from_nodes(tree, ids.iter().copied());
    ids.sort_by_key(|&i| std::cmp::Reverse(tree.nodes[i as usize].size_of(allocated)));
    ids.truncate(LIST_MAX);
    Some(Results { text: text.to_string(), ids, count, bytes, highlight })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(p: &str, t: &str) -> bool {
        glob(&p.chars().collect::<Vec<_>>(), &t.chars().collect::<Vec<_>>())
    }

    #[test]
    fn globbing() {
        assert!(g("*.iso", "ubuntu.iso"));
        assert!(!g("*.iso", "ubuntu.iso.part"));
        assert!(g("a?c", "abc"));
        assert!(g("*", ""));
        assert!(g("*cache*", "npm-cache-old"));
        assert!(!g("a*b", "acd"));
    }

    #[test]
    fn parsing() {
        assert!(Query::parse("  ").is_none());
        let q = Query::parse("*.ISO; >1.5GB file:").unwrap();
        assert_eq!(q.min, Some((1.5 * (1u64 << 30) as f64) as u64));
        assert_eq!(q.dirs, Some(false));
        assert!(q.matches_name("Big.iso"));
        assert!(!q.matches_name("big.isox"));
        assert_eq!(parse_size("200"), Some(200));
        assert_eq!(parse_size("1kb"), Some(1024));
        assert!(Query::parse(">12XB").is_none());
        let q = Query::parse("node_modules folder:").unwrap();
        assert!(q.matches_name("Node_Modules") && q.dirs == Some(true));
    }

    #[test]
    fn search_counts_nested_once() {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "cache", true, 0, 0, 0);
        t.add(a, "cache.bin", false, 100, 100, 0);
        t.add(0, "other.cache", false, 5, 5, 0);
        t.add(0, "x.txt", false, 7, 7, 0);
        t.finish();
        let t = Arc::new(t);
        let q = Query::parse("cache").unwrap();
        let r = run(&t, 0, "cache", &q, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.count, 3);
        assert_eq!(r.bytes, 105);
        assert_eq!(t.name(r.ids[0]), "cache");
        let q = Query::parse(">50").unwrap();
        let r = run(&t, 0, "", &q, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.count, 2); // the folder and the file in it
    }
}
