//! Cleanup hints: well-known folders that hold caches, temporary files or rebuildable build output, found by
//! name and surroundings after each scan. Only suggestions: nothing is deleted without the user's action.

use crate::tree::{self, NONE, Tree};

/// Hints smaller than this are not listed.
const MIN_SIZE: u64 = 1 << 20;

pub struct Hint {
    pub id: u32,
    pub category: &'static str,
    pub note: &'static str,
}

struct Rule {
    /// Folder names (lowercase).
    names: &'static [&'static str],
    parent: Option<&'static str>,
    /// Some folder up to 6 levels above has this name.
    ancestor: Option<&'static str>,
    /// A file next to the folder has this name or extension (".csproj").
    sibling: &'static [&'static str],
    category: &'static str,
    note: &'static str,
}

const fn rule(names: &'static [&'static str], category: &'static str, note: &'static str) -> Rule {
    Rule { names, parent: None, ancestor: None, sibling: &[], category, note }
}

const RULES: &[Rule] = &[
    Rule { parent: Some("local"), ..rule(&["temp"], "Temporary files", "Your temp folder. Files not in use can be deleted (Disk Cleanup / Storage Sense do this).") },
    Rule { parent: Some("windows"), ..rule(&["temp"], "Temporary files", "Windows temp folder; needs admin rights to empty.") },
    Rule { ancestor: Some("user data"), ..rule(&["cache", "code cache", "gpucache", "service worker"], "Browser cache", "Chrome/Edge cache; the browser rebuilds it. Clear it from the browser's settings.") },
    Rule { ancestor: Some("profiles"), ..rule(&["cache2"], "Browser cache", "Firefox cache; clear it from Firefox settings.") },
    rule(&["inetcache"], "Browser cache", "Internet cache of Windows apps; safe to clear with Disk Cleanup."),
    rule(&["npm-cache"], "Package cache", "npm download cache; `npm cache clean --force` empties it."),
    Rule { parent: Some(".cargo"), ..rule(&["registry"], "Package cache", "Rust crate downloads; re-downloaded when needed (cargo-cache can trim it).") },
    Rule { parent: Some("pip"), ..rule(&["cache"], "Package cache", "pip download cache; `pip cache purge` empties it.") },
    Rule { parent: Some(".m2"), ..rule(&["repository"], "Package cache", "Maven repository; artifacts are re-downloaded on the next build.") },
    Rule { parent: Some(".gradle"), ..rule(&["caches"], "Package cache", "Gradle caches; rebuilt on the next build.") },
    Rule { parent: Some("nuget"), ..rule(&["v3-cache"], "Package cache", "NuGet HTTP cache; `dotnet nuget locals all --clear`.") },
    Rule { parent: Some(".nuget"), ..rule(&["packages"], "Package cache", "NuGet package folder; restored on the next build.") },
    Rule { sibling: &["cargo.toml"], ..rule(&["target"], "Build output", "Rust build output; `cargo clean` removes it, rebuilt on the next build.") },
    rule(&["node_modules"], "Dependencies", "npm packages of a project; `npm install` brings them back."),
    Rule { sibling: &[".csproj", ".vbproj", ".fsproj"], ..rule(&["bin", "obj"], "Build output", ".NET build output; rebuilt on the next build.") },
    rule(&["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache"], "Python cache", "Python byte-code/tool caches; recreated automatically."),
    rule(&["crashdumps"], "Crash dumps", "Memory dumps of crashed programs; only useful for debugging those crashes."),
    rule(&["$recycle.bin"], "Recycle Bin", "Deleted files still taking space; empty the Recycle Bin."),
];

fn rule_matches(tree: &Tree, id: u32, r: &Rule, name: &str) -> bool {
    if !r.names.contains(&name) {
        return false;
    }
    let parent = tree.nodes[id as usize].parent;
    if let Some(p) = r.parent {
        if parent == NONE || tree.name(parent).to_lowercase() != p {
            return false;
        }
    }
    if let Some(a) = r.ancestor {
        let mut c = parent;
        let mut found = false;
        for _ in 0..6 {
            if c == NONE {
                break;
            }
            if tree.name(c).eq_ignore_ascii_case(a) {
                found = true;
                break;
            }
            c = tree.nodes[c as usize].parent;
        }
        if !found {
            return false;
        }
    }
    if !r.sibling.is_empty() {
        if parent == NONE {
            return false;
        }
        let ok = tree.children(parent).any(|s| {
            let sn = tree.name(s).to_lowercase();
            !tree.nodes[s as usize].is_dir() && r.sibling.iter().any(|want| if want.starts_with('.') { sn.ends_with(want) } else { sn == *want })
        });
        if !ok {
            return false;
        }
    }
    true
}

/// All hints in the tree, largest first. A matched folder is not searched further (no nested repeats).
pub fn find(tree: &Tree) -> Vec<Hint> {
    let mut hints = Vec::new();
    let mut stack = vec![0u32];
    while let Some(id) = stack.pop() {
        for c in tree.children(id) {
            let n = &tree.nodes[c as usize];
            if !n.is_dir() || n.flags & tree::GONE != 0 {
                continue;
            }
            let name = tree.name(c).to_lowercase();
            if let Some(r) = RULES.iter().find(|r| rule_matches(tree, c, r, &name)) {
                if n.size >= MIN_SIZE {
                    hints.push(Hint { id: c, category: r.category, note: r.note });
                }
                continue;
            }
            stack.push(c);
        }
    }
    hints.sort_by_key(|h| std::cmp::Reverse(tree.nodes[h.id as usize].size));
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    #[test]
    fn finds_known_folders() {
        let mut t = Tree::new("C:\\");
        let rust = t.add(0, "proj", true, 0, 0, 0);
        t.add(rust, "Cargo.toml", false, 10, 10, 0);
        let target = t.add(rust, "target", true, 0, 0, 0);
        t.add(target, "big.rlib", false, 50 * MB, 50 * MB, 0);
        // "target" without Cargo.toml next to it is not build output.
        let other = t.add(0, "photos", true, 0, 0, 0);
        let t2 = t.add(other, "target", true, 0, 0, 0);
        t.add(t2, "img.jpg", false, 20 * MB, 20 * MB, 0);
        // node_modules inside node_modules is reported once.
        let web = t.add(0, "web", true, 0, 0, 0);
        let nm = t.add(web, "node_modules", true, 0, 0, 0);
        let inner = t.add(nm, "pkg", true, 0, 0, 0);
        let nm2 = t.add(inner, "node_modules", true, 0, 0, 0);
        t.add(nm2, "x.js", false, 5 * MB, 5 * MB, 0);
        // Too small to mention.
        let py = t.add(0, "__pycache__", true, 0, 0, 0);
        t.add(py, "a.pyc", false, 1000, 1000, 0);
        let users = t.add(0, "Users", true, 0, 0, 0);
        let local = t.add(users, "Local", true, 0, 0, 0);
        let temp = t.add(local, "Temp", true, 0, 0, 0);
        t.add(temp, "junk.tmp", false, 3 * MB, 3 * MB, 0);
        t.finish();
        let hints = find(&t);
        let ids: Vec<u32> = hints.iter().map(|h| h.id).collect();
        assert_eq!(ids, vec![target, nm, temp]);
        assert_eq!(hints[0].category, "Build output");
        assert_eq!(hints[2].category, "Temporary files");
    }
}
