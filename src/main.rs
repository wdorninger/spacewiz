//! spacewiz: disk space analyzer with an interactive sunburst chart. Reads the NTFS Master File Table when
//! running as administrator, otherwise walks folders in parallel.
//! `spacewiz [path]` opens the window; `--scan <path>` scans headless and prints totals; `--dupes <path>` lists
//! duplicate files; `--console` shows the log. With a path, `--search <query>` and `--find-dupes` run once the
//! scan is done.
#![windows_subsystem = "windows"]

mod actions;
mod chart;
mod cleanup;
mod config;
mod drives;
mod dupes;
mod logger;
mod scan;
mod search;
mod sunburst;
mod theme;
mod tree;
mod treemap;
mod ui;

use std::sync::Arc;

use eframe::egui;

fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

/// `--scan <path> [--walk]`: scans without a window and prints the totals, the time taken and the largest items.
fn scan_headless(path: &str, prefer_mft: bool) {
    let progress = scan::Progress::default();
    let exclude = scan::Exclude::new(&config::Config::load().str_list("scan", "exclude"));
    match scan::scan(path, prefer_mft, &exclude, &progress) {
        scan::Event::Done { tree, focus, stats } => {
            let n = &tree.nodes[focus as usize];
            println!("{}  method: {}{}", tree.path(focus), stats.method.label(), stats.note.map(|n| format!(" ({n})")).unwrap_or_default());
            println!(
                "  {} ({} bytes), on disk {}, {} files, {} folders, {} nodes, {:.2} s, {} folders not readable",
                tree::format_size(n.size),
                n.size,
                tree::format_size(n.alloc),
                tree::format_count(n.files as u64),
                tree::format_count(n.dirs as u64),
                tree::format_count(tree.len() as u64),
                stats.elapsed.as_secs_f64(),
                stats.denied
            );
            for c in tree.children(focus).take(10) {
                println!("  {:>10}  {}", tree::format_size(tree.nodes[c as usize].size), tree.name(c));
            }
        }
        scan::Event::Failed(e) => println!("error: {e}"),
    }
}

/// `--dupes <path>`: walks the folder, finds duplicate files (min size from config.toml) and prints the largest groups.
fn dupes_headless(path: &str, min_mb: i64) {
    let progress = scan::Progress::default();
    let started = std::time::Instant::now();
    let tree = match scan::walk::scan(path, &progress, &scan::Exclude::default()) {
        Ok(r) => std::sync::Arc::new(r.tree),
        Err(e) => return println!("error: {e:#}"),
    };
    let walked = started.elapsed().as_secs_f64();
    let p = dupes::Progress::default();
    let Some(r) = dupes::find(&tree, 0, min_mb.max(0) as u64 * (1 << 20), &p) else { return };
    println!(
        "{path}: {} wasted in {} groups; walk {walked:.2} s, compare {:.2} s, {} read, {} candidates, {} skipped (online-only/unreadable)",
        tree::format_size(r.wasted),
        r.groups.len(),
        started.elapsed().as_secs_f64() - walked,
        tree::format_size(p.bytes.load(std::sync::atomic::Ordering::Relaxed)),
        p.total.load(std::sync::atomic::Ordering::Relaxed),
        r.skipped
    );
    for g in r.groups.iter().take(8) {
        println!("  {} x {}  (-{})", g.ids.len(), tree::format_size(g.size), tree::format_size(g.wasted()));
        for &id in g.ids.iter().take(3) {
            println!("      {}", tree.path(id));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let console = args.iter().any(|a| a == "--console" || a == "--scan" || a == "--dupes");
    if console {
        attach_console();
    }
    logger::init(console);
    let cfg = config::Config::load();
    if let Some(i) = args.iter().position(|a| a == "--scan") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| "C:\\".into());
        let walk = args.iter().any(|a| a == "--walk");
        scan_headless(&path, cfg.bool("scan", "prefer_mft") && !walk);
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--dupes") {
        dupes_headless(args.get(i + 1).map(String::as_str).unwrap_or("."), cfg.i64("dupes", "min_size_mb"));
        return;
    }
    std::panic::set_hook(Box::new(|info| log::error!("Fatal error: {info}")));
    // The first argument that isn't an option or an option's value is the folder to open.
    let search = args.iter().position(|a| a == "--search").and_then(|i| args.get(i + 1).cloned());
    let open = args.iter().enumerate().skip(1).find(|(i, a)| !a.starts_with("--") && args[i - 1] != "--search").map(|(_, a)| a.clone());
    let startup = ui::Startup { search, find_dupes: args.iter().any(|a| a == "--find-dupes") };

    let icon = egui::IconData { rgba: theme::app_icon(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if actions::is_elevated() { "spacewiz (Administrator)" } else { "spacewiz" })
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    let result = eframe::run_native("spacewiz", options, Box::new(move |cc| Ok(Box::new(ui::App::new(cc, cfg, open, startup)))));
    if let Err(e) = result {
        log::error!("Window failed: {e}");
    }
}
