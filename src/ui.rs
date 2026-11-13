//! Main window: drive list on the left, toolbar with navigation, breadcrumbs and search, the chart (sunburst or
//! treemap) in the middle, side view tabs on the right (folders, file types, largest files, cleanup hints,
//! search results, duplicates), status bar at the bottom.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, Margin, Rect, RichText, Sense, Stroke, ThemePreference, pos2, vec2};
use toml::Value;

use crate::chart::{self, ColorMode, Highlight, Kind, Options, Target};
use crate::scan::{self, Event, Exclude, Job};
use crate::search::{self, Query};
use crate::sunburst::{Chart, Extras};
use crate::theme::{self, Palette};
use crate::tree::{self, NONE, Tree};
use crate::treemap::Treemap;
use crate::{actions, cleanup, drives, dupes};

const ROW_H: f32 = 22.0;
/// Re-layout the chart of a running scan at most this often.
const LIVE_REFRESH: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug)]
enum Action {
    Zoom(u32),
    Open(u32),
    Explorer(u32),
    CopyPath(u32),
    Rescan(u32),
    Recycle(u32),
    Skip(u32),
    Dupes(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Folders,
    Types,
    Largest,
    Cleanup,
    Search,
    Dupes,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Folders => "Folders",
            Tab::Types => "Types",
            Tab::Largest => "Largest",
            Tab::Cleanup => "Cleanup",
            Tab::Search => "Search",
            Tab::Dupes => "Duplicates",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Sunburst,
    Treemap,
}

/// Where a wheel step over the chart goes.
enum WheelNav {
    Into(u32),
    Up,
}

/// A running scan: either a whole new tree, or one folder of the current tree being rescanned.
struct Running {
    job: Job,
    rescan: Option<u32>,
}

struct Settings {
    theme: String,
    show_tree: bool,
    show_types: bool,
    show_top_files: bool,
    show_cleanup: bool,
    ring_depth: usize,
    allocated: bool,
    color: ColorMode,
    view: View,
    show_free_space: bool,
    prefer_mft: bool,
    exclude: Vec<String>,
    dupes_min_mb: i64,
}

impl Settings {
    fn load(cfg: &crate::config::Config) -> Settings {
        Settings {
            theme: cfg.str("ui", "theme"),
            show_tree: cfg.bool("ui", "show_tree"),
            show_types: cfg.bool("ui", "show_types"),
            show_top_files: cfg.bool("ui", "show_top_files"),
            show_cleanup: cfg.bool("ui", "show_cleanup"),
            ring_depth: cfg.i64("ui", "ring_depth").clamp(2, 12) as usize,
            allocated: cfg.str("ui", "size_mode") == "allocated",
            color: ColorMode::from_config(&cfg.str("ui", "color_by")),
            view: if cfg.str("ui", "view") == "treemap" { View::Treemap } else { View::Sunburst },
            show_free_space: cfg.bool("ui", "show_free_space"),
            prefer_mft: cfg.bool("scan", "prefer_mft"),
            exclude: cfg.str_list("scan", "exclude"),
            dupes_min_mb: cfg.i64("dupes", "min_size_mb").max(0),
        }
    }
}

struct Loaded {
    tree: Arc<Tree>,
    stats: scan::Stats,
    /// Bumped whenever the tree changes (recycle, rescan), so the chart lays out again.
    generation: u64,
    /// Rank of each extension in the type list, for colours.
    ext_rank: Vec<usize>,
    /// Drive total/free if the tree is a whole drive.
    space: Option<(u64, u64)>,
    hints: Vec<cleanup::Hint>,
}

impl Loaded {
    fn new(tree: Tree, stats: scan::Stats, generation: u64) -> Loaded {
        let space = (tree.root_path.len() == 3).then(|| drives::space(&tree.root_path)).flatten();
        let mut l = Loaded { tree: Arc::new(tree), stats, generation, ext_rank: Vec::new(), space, hints: Vec::new() };
        l.refresh();
        l
    }
    fn refresh(&mut self) {
        self.ext_rank = vec![usize::MAX; self.tree.exts.len()];
        for (i, t) in self.tree.ext_totals.iter().enumerate() {
            if t.ext != 0 {
                self.ext_rank[t.ext as usize] = i;
            }
        }
        self.hints = cleanup::find(&self.tree);
    }
    /// The tree for changing (copied first if a search or duplicate job still reads it).
    fn tree_mut(&mut self) -> &mut Tree {
        Arc::make_mut(&mut self.tree)
    }
    fn changed(&mut self) {
        self.generation += 1;
        self.refresh();
        if self.space.is_some() {
            self.space = drives::space(&self.tree.root_path);
        }
    }
}

/// What to do once the first scan is done (command line).
#[derive(Default)]
pub struct Startup {
    pub search: Option<String>,
    pub find_dupes: bool,
}

struct SearchJob {
    rx: Receiver<Option<search::Results>>,
    cancel: Arc<AtomicBool>,
}

struct DupesJob {
    rx: Receiver<Option<dupes::Report>>,
    progress: Arc<dupes::Progress>,
    path: String,
}

pub struct App {
    settings: Settings,
    elevated: bool,
    /// Admin account without elevation: offer the restart for the fast MFT scan.
    can_elevate: bool,
    loaded: Option<Loaded>,
    running: Option<Running>,
    focus: u32,
    back: Vec<u32>,
    forward: Vec<u32>,
    chart: Chart,
    treemap: Treemap,
    menu_target: Option<Target>,
    drives: Vec<drives::Drive>,
    drives_at: Instant,
    expanded: HashSet<u32>,
    selected: Option<u32>,
    hovered: Option<Kind>,
    hovered_from_list: Option<u32>,
    pulse: Option<(u32, Instant)>,
    confirm_recycle: Option<u32>,
    settings_open: bool,
    exclude_text: String,
    message: Option<(String, bool, Instant)>,
    pending: Vec<Action>,
    generation: u64,
    tab: Tab,
    // Search
    search_text: String,
    search_edited: Option<Instant>,
    search_job: Option<SearchJob>,
    search: Option<search::Results>,
    focus_search: bool,
    /// What dims the chart, with a label for the chip above it.
    highlight: Option<(String, Highlight)>,
    // Duplicates
    dupes_job: Option<DupesJob>,
    dupes: Option<dupes::Report>,
    dupes_open: HashSet<usize>,
    // Live scan view
    live_generation: u64,
    live_at: Instant,
    // Wheel zoom
    wheel_acc: f32,
    wheel_at: Instant,
    // Drag out
    drag_request: Option<PathBuf>,
    release_after_drag: bool,
    startup: Startup,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: crate::config::Config, open: Option<String>, startup: Startup) -> App {
        theme::install_fonts(&cc.egui_ctx);
        theme::install_style(&cc.egui_ctx);
        let settings = Settings::load(&cfg);
        let mut app = App {
            exclude_text: settings.exclude.join("\n"),
            settings,
            elevated: actions::is_elevated(),
            can_elevate: actions::can_elevate(),
            loaded: None,
            running: None,
            focus: 0,
            back: Vec::new(),
            forward: Vec::new(),
            chart: Chart::default(),
            treemap: Treemap::default(),
            menu_target: None,
            drives: drives::list(),
            drives_at: Instant::now(),
            expanded: HashSet::new(),
            selected: None,
            hovered: None,
            hovered_from_list: None,
            pulse: None,
            confirm_recycle: None,
            settings_open: false,
            message: None,
            pending: Vec::new(),
            generation: 0,
            tab: Tab::Folders,
            search_text: String::new(),
            search_edited: None,
            search_job: None,
            search: None,
            focus_search: false,
            highlight: None,
            dupes_job: None,
            dupes: None,
            dupes_open: HashSet::new(),
            live_generation: 0,
            live_at: Instant::now(),
            wheel_acc: 0.0,
            wheel_at: Instant::now(),
            drag_request: None,
            release_after_drag: false,
            startup,
        };
        app.apply_theme(&cc.egui_ctx);
        if let Some(path) = open {
            app.start_scan(&path);
        }
        app
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.theme.as_str() {
            "dark" => ThemePreference::Dark,
            "light" => ThemePreference::Light,
            _ => ThemePreference::System,
        });
    }

    fn save(&self, section: &str, key: &str, value: Value) {
        if let Err(e) = crate::config::save_setting(section, key, &value) {
            log::warn!("Could not save {section}.{key}: {e:#}");
        }
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            log::warn!("{text}");
        }
        self.message = Some((text, error, Instant::now()));
    }

    fn exclude(&self) -> Exclude {
        Exclude::new(&self.settings.exclude)
    }

    /// Runs `f` on the tree on screen: the loaded one, or the one a scan is building.
    fn with_shown_tree<R>(&self, f: impl FnOnce(&Tree, u32) -> R) -> Option<R> {
        if let Some(l) = &self.loaded {
            return Some(f(&l.tree, self.focus));
        }
        let live = self.running.as_ref().filter(|r| r.rescan.is_none())?.job.progress.live()?;
        let t = live.lock().unwrap();
        ((self.focus as usize) < t.len()).then(|| f(&t, self.focus))
    }

    // ---- scanning ----

    fn start_scan(&mut self, path: &str) {
        if let Some(r) = &self.running {
            r.job.progress.cancel.store(true, Ordering::Relaxed);
        }
        log::info!("Scanning {path}");
        self.running = Some(Running { job: scan::start(path, self.settings.prefer_mft, self.exclude()), rescan: None });
        // The new tree is browsed live while it is scanned; everything tied to the old one goes.
        self.loaded = None;
        self.focus = 0;
        self.back.clear();
        self.forward.clear();
        self.expanded.clear();
        self.selected = None;
        self.clear_search();
        self.highlight = None;
        self.cancel_dupes();
        self.dupes = None;
        self.chart = Chart::default();
        self.treemap = Treemap::default();
    }

    fn start_rescan(&mut self, id: u32) {
        let Some(l) = &self.loaded else { return };
        if id == 0 {
            let root = l.tree.root_path.clone();
            self.start_scan(&root);
            return;
        }
        let path = l.tree.path(id);
        log::info!("Rescanning {path}");
        // A walk of one folder is quick; reading the MFT would re-read the whole volume.
        self.running = Some(Running { job: scan::start(&path, false, self.exclude()), rescan: Some(id) });
    }

    fn poll_scan(&mut self) {
        let Some(r) = &self.running else { return };
        let Ok(event) = r.job.events.try_recv() else { return };
        let rescan = r.rescan;
        self.running = None;
        match event {
            Event::Done { tree, focus, stats } => match (rescan, self.loaded.as_mut()) {
                (Some(id), Some(l)) => {
                    l.tree_mut().replace_subtree(id, &tree);
                    l.changed();
                    let msg = format!("Rescanned {} in {:.1} s", l.tree.path(id), stats.elapsed.as_secs_f32());
                    self.notify(msg, false);
                }
                _ => {
                    self.generation += 1;
                    let walked = stats.method == scan::Method::Walk;
                    let l = Loaded::new(tree, stats, self.generation * 1_000_000);
                    // After browsing the live walk, stay where the user is (node ids don't change).
                    let keep = walked && (self.focus as usize) < l.tree.len();
                    if !keep {
                        self.focus = focus;
                        self.back.clear();
                        self.forward.clear();
                    }
                    if l.stats.skipped > 0 {
                        self.notify(format!("{} folders skipped by the exclusion list", tree::format_count(l.stats.skipped)), false);
                    }
                    self.loaded = Some(l);
                    self.drives = drives::list();
                    if let Some(q) = self.startup.search.take() {
                        self.search_text = q;
                        self.start_search();
                    }
                    if std::mem::take(&mut self.startup.find_dupes) {
                        self.start_dupes(self.focus);
                    }
                }
            },
            Event::Failed(e) => self.notify(format!("Scan failed: {e}"), true),
        }
    }

    // ---- navigation ----

    fn navigate(&mut self, to: u32) {
        if to == self.focus {
            return;
        }
        self.back.push(self.focus);
        self.forward.clear();
        self.focus = to;
    }

    fn go_back(&mut self) {
        if let Some(b) = self.back.pop() {
            self.forward.push(self.focus);
            self.focus = b;
        }
    }

    fn go_forward(&mut self) {
        if let Some(f) = self.forward.pop() {
            self.back.push(self.focus);
            self.focus = f;
        }
    }

    fn go_up(&mut self) {
        let p = self.with_shown_tree(|t, focus| t.nodes[focus as usize].parent).unwrap_or(NONE);
        if p != NONE {
            self.navigate(p);
        }
    }

    /// Shows a node: navigates to the folder holding it and makes it pulse.
    fn reveal(&mut self, id: u32) {
        let Some(l) = &self.loaded else { return };
        let parent = l.tree.nodes[id as usize].parent;
        self.navigate(if parent == NONE { 0 } else { parent });
        self.selected = Some(id);
        self.pulse = Some((id, Instant::now()));
    }

    /// Valid focus after nodes were removed: the nearest ancestor that still exists.
    fn fix_focus(&mut self) {
        let Some(l) = &self.loaded else { return };
        let gone = |id: u32| l.tree.chain(id).iter().any(|&a| l.tree.nodes[a as usize].flags & tree::GONE != 0);
        while gone(self.focus) {
            self.focus = l.tree.nodes[self.focus as usize].parent;
        }
        self.back.retain(|&b| !gone(b));
        self.forward.retain(|&f| !gone(f));
    }

    // ---- actions ----

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let Some(l) = &self.loaded else { return };
        let id = match action {
            Action::Zoom(id) | Action::Open(id) | Action::Explorer(id) | Action::CopyPath(id) | Action::Rescan(id) | Action::Recycle(id) | Action::Skip(id) | Action::Dupes(id) => id,
        };
        let path = l.tree.path(id);
        match action {
            Action::Zoom(id) => self.navigate(id),
            Action::Open(_) => actions::shell_open(std::path::Path::new(&path)),
            Action::Explorer(_) => actions::show_in_explorer(std::path::Path::new(&path)),
            Action::CopyPath(_) => {
                ctx.copy_text(path.clone());
                self.notify(format!("Copied {path}"), false);
            }
            Action::Rescan(id) => self.start_rescan(id),
            Action::Recycle(id) => self.confirm_recycle = Some(id),
            Action::Skip(_) => {
                if !self.settings.exclude.iter().any(|e| e.eq_ignore_ascii_case(&path)) {
                    self.settings.exclude.push(path.clone());
                    self.exclude_text = self.settings.exclude.join("\n");
                    self.save_exclude();
                }
                self.notify(format!("{path} will be skipped in future scans (Settings → Skip folders)"), false);
            }
            Action::Dupes(id) => self.start_dupes(id),
        }
    }

    fn save_exclude(&self) {
        let list = self.settings.exclude.iter().map(|s| Value::String(s.clone())).collect();
        self.save("scan", "exclude", Value::Array(list));
    }

    fn do_recycle(&mut self, id: u32) {
        let Some(l) = &mut self.loaded else { return };
        let path = l.tree.path(id);
        let size = l.tree.nodes[id as usize].size;
        match actions::recycle(std::path::Path::new(&path)) {
            Ok(()) => {
                log::info!("Moved {path} to the Recycle Bin");
                l.tree_mut().remove(id);
                l.changed();
                self.fix_focus();
                self.drives = drives::list();
                // Lists that may contain the removed item.
                self.highlight = None;
                if let Some(r) = &mut self.search {
                    r.ids.retain(|&i| i != id);
                }
                self.notify(format!("Moved {path} to the Recycle Bin ({} freed once emptied)", tree::format_size(size)), false);
            }
            Err(e) => self.notify(format!("{path}: {e}"), true),
        }
    }

    // ---- search ----

    fn clear_search(&mut self) {
        if let Some(j) = self.search_job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
        self.search = None;
        self.search_edited = None;
        if self.highlight.as_ref().is_some_and(|(l, _)| l.starts_with("Search")) {
            self.highlight = None;
        }
    }

    fn start_search(&mut self) {
        let text = self.search_text.trim().to_string();
        if let Some(j) = self.search_job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
        let Some(query) = Query::parse(&text) else {
            self.clear_search();
            return;
        };
        let Some(l) = &self.loaded else { return };
        let (tree, root, allocated) = (l.tree.clone(), self.focus, self.settings.allocated);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let c = cancel.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                let _ = tx.send(search::run(&tree, root, &text, &query, allocated, &c));
            })
            .expect("spawn search thread");
        self.search_job = Some(SearchJob { rx, cancel });
    }

    fn poll_search(&mut self) {
        if self.search_edited.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            self.search_edited = None;
            self.start_search();
        }
        let Some(j) = &self.search_job else { return };
        let Ok(result) = j.rx.try_recv() else { return };
        self.search_job = None;
        if let Some(r) = result {
            self.highlight = Some((format!("Search “{}”: {} items, {}", r.text, tree::format_count(r.count as u64), tree::format_size(r.bytes)), r.highlight.clone()));
            self.search = Some(r);
            self.tab = Tab::Search;
        }
    }

    // ---- duplicates ----

    fn cancel_dupes(&mut self) {
        if let Some(j) = self.dupes_job.take() {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn start_dupes(&mut self, root: u32) {
        let Some(tree) = self.loaded.as_ref().map(|l| l.tree.clone()) else { return };
        self.cancel_dupes();
        let path = tree.path(root);
        let min = self.settings.dupes_min_mb as u64 * (1 << 20);
        let progress = Arc::new(dupes::Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let p = progress.clone();
        log::info!("Looking for duplicates in {path}");
        std::thread::Builder::new()
            .name("dupes".into())
            .spawn(move || {
                let _ = tx.send(dupes::find(&tree, root, min, &p));
            })
            .expect("spawn dupes thread");
        self.dupes_job = Some(DupesJob { rx, progress, path });
        self.dupes = None;
        self.dupes_open.clear();
        self.tab = Tab::Dupes;
    }

    fn poll_dupes(&mut self) {
        let Some(j) = &self.dupes_job else { return };
        let Ok(result) = j.rx.try_recv() else { return };
        let path = j.path.clone();
        self.dupes_job = None;
        if let Some(r) = result {
            log::info!("Duplicates in {path}: {} groups, {} wasted", r.groups.len(), tree::format_size(r.wasted));
            self.dupes = Some(r);
        }
    }

    // ---- chart ----

    fn chart_options(&self) -> Options {
        let free = self.loaded.as_ref().and_then(|l| l.space).map(|(_, free)| free).filter(|_| self.settings.show_free_space);
        Options { depth: self.settings.ring_depth, allocated: self.settings.allocated, color: self.settings.color, free }
    }

    fn pulse_strength(&self) -> Option<(u32, f32)> {
        let (id, at) = self.pulse?;
        let e = at.elapsed().as_secs_f32();
        (e < 2.0).then(|| (id, (e * 6.0).sin().abs() * (1.0 - e / 2.0)))
    }

    /// Draws the chart of `tree` (loaded or live) and returns what happened.
    fn draw_chart(&mut self, ui: &mut egui::Ui, rect: Rect, tree: &Tree, generation: u64, ext_rank: &[usize], live: bool) -> (chart::Output, egui::Response) {
        let mut opts = self.chart_options();
        if live {
            opts.free = None;
        }
        let pulse = self.pulse_strength();
        if pulse.is_some() {
            ui.ctx().request_repaint();
        }
        let extras = Extras { ext_rank, highlight: self.highlight.as_ref().map(|(_, h)| h), pulse, hovered_from_list: self.hovered_from_list, now: chart::now_filetime() };
        let out = match self.settings.view {
            View::Sunburst => {
                self.chart.update(tree, generation, self.focus, &opts);
                self.chart.show(ui, rect, tree, &opts, &extras)
            }
            View::Treemap => {
                let inner = rect.shrink(10.0);
                self.treemap.update(tree, generation, self.focus, inner, &opts);
                self.treemap.show(ui, inner, tree, &opts, &extras)
            }
        };
        if self.settings.color == ColorMode::Age {
            chart::age_legend(&ui.painter_at(rect), rect, ui.style().visuals.dark_mode);
        }
        out
    }

    /// Wheel over the chart: up zooms into the folder under the pointer, down goes up a level.
    fn wheel(&mut self, ui: &egui::Ui, hovered: Option<Kind>, tree: &Tree) -> Option<WheelNav> {
        let dy: f32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => Some(
                        delta.y
                            * match unit {
                                egui::MouseWheelUnit::Point => 1.0,
                                egui::MouseWheelUnit::Line => 40.0,
                                egui::MouseWheelUnit::Page => 400.0,
                            },
                    ),
                    _ => None,
                })
                .sum()
        });
        if dy == 0.0 {
            if self.wheel_at.elapsed() > Duration::from_millis(400) {
                self.wheel_acc = 0.0;
            }
            return None;
        }
        self.wheel_acc += dy;
        if self.wheel_acc.abs() < 50.0 || self.wheel_at.elapsed() < Duration::from_millis(300) {
            return None;
        }
        let up = self.wheel_acc > 0.0;
        self.wheel_acc = 0.0;
        self.wheel_at = Instant::now();
        if !up {
            return Some(WheelNav::Up);
        }
        let target = match hovered {
            Some(Kind::Node(id)) if tree.nodes[id as usize].is_dir() => Some(id),
            Some(Kind::Node(id)) => Some(tree.nodes[id as usize].parent).filter(|&p| p != self.focus && p != NONE),
            Some(Kind::Small { parent, .. }) => Some(parent).filter(|&p| p != self.focus),
            _ => None,
        };
        target.map(WheelNav::Into)
    }

    fn apply_wheel(&mut self, nav: Option<WheelNav>) {
        match nav {
            Some(WheelNav::Into(id)) => self.navigate(id),
            Some(WheelNav::Up) => self.go_up(),
            None => {}
        }
    }

    fn handle_chart_output(&mut self, out: chart::Output, tree: &Tree) {
        self.hovered = out.hovered;
        if let Some(t) = out.context {
            self.menu_target = Some(t);
        }
        if let Some(id) = out.drag_out {
            self.drag_request = Some(PathBuf::from(tree.path(id)));
        }
        let is_dir = |id: u32| tree.nodes[id as usize].is_dir();
        if let Some(Target::Sector(Kind::Node(id))) = out.double_clicked {
            if !is_dir(id) {
                actions::shell_open(std::path::Path::new(&tree.path(id)));
                return;
            }
        }
        match out.clicked {
            Some(Target::Center) => self.go_up(),
            Some(Target::Sector(Kind::Node(id))) if is_dir(id) => self.navigate(id),
            Some(Target::Sector(Kind::Node(id))) => self.selected = Some(id),
            Some(Target::Sector(Kind::Small { parent, .. })) => self.navigate(parent),
            _ => {}
        }
    }

    // ---- panels ----

    fn toolbar(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.horizontal(|ui| {
            let browsing = self.loaded.is_some() || self.running.as_ref().is_some_and(|r| r.rescan.is_none());
            let up_ok = browsing && self.focus != 0;
            if ui.add_enabled(!self.back.is_empty(), egui::Button::new(theme::icon(theme::ICON_BACK, "←"))).on_hover_text("Back (Alt+Left)").clicked() {
                self.go_back();
            }
            if ui.add_enabled(!self.forward.is_empty(), egui::Button::new(theme::icon(theme::ICON_FORWARD, "→"))).on_hover_text("Forward (Alt+Right)").clicked() {
                self.go_forward();
            }
            if ui.add_enabled(up_ok, egui::Button::new(theme::icon(theme::ICON_UP, "↑"))).on_hover_text("Parent folder (Backspace)").clicked() {
                self.go_up();
            }
            if ui.add_enabled(self.loaded.is_some() && self.running.is_none(), egui::Button::new(theme::icon(theme::ICON_REFRESH, "⟳"))).on_hover_text("Rescan this folder (F5)").clicked() {
                self.pending.push(Action::Rescan(self.focus));
            }
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(theme::icon(theme::ICON_SETTINGS, "⚙")).on_hover_text("Settings").clicked() {
                    self.settings_open = true;
                }
                self.view_menu(ui);
                self.tools_menu(ui);
                self.view_toggle(ui, p);
                let search = egui::TextEdit::singleline(&mut self.search_text).hint_text("Search  *.iso  >1GB").desired_width(190.0).id(egui::Id::unique("search-box"));
                let resp = ui.add_enabled(self.loaded.is_some(), search).on_hover_text("Ctrl+F · wildcards * ?, several terms, >100MB <1KB, file: folder: · Esc clears");
                if self.focus_search {
                    resp.request_focus();
                    self.focus_search = false;
                }
                if resp.changed() {
                    self.search_edited = Some(Instant::now());
                }
                if self.search_job.is_some() {
                    ui.spinner();
                }
                if !self.elevated && self.can_elevate {
                    let b = egui::Button::new((theme::icon(theme::ICON_SHIELD, "").color(p.warn), RichText::new("Fast scan").color(p.warn))).stroke(Stroke::new(1.0, p.warn.gamma_multiply(0.6)));
                    if ui.add(b).on_hover_text("Restart as administrator to read the NTFS Master File Table directly:\nwhole drives in seconds instead of minutes.").clicked() {
                        let path = self.loaded.as_ref().map(|l| l.tree.path(self.focus)).unwrap_or_default();
                        if actions::restart_elevated(&path) {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        } else {
                            self.notify("Administrator rights were not granted", true);
                        }
                    }
                }
                if let Some(l) = &self.loaded {
                    let s = &l.stats;
                    let text = format!("{} · {:.1} s", s.method.label(), s.elapsed.as_secs_f32());
                    let mut tip = format!("Scanned with {} in {:.2} s", s.method.label(), s.elapsed.as_secs_f64());
                    if let Some(n) = &s.note {
                        tip.push_str(&format!("\nMFT not used: {n}"));
                    }
                    if s.denied > 0 {
                        tip.push_str(&format!("\n{} folders could not be read (access denied)", tree::format_count(s.denied)));
                    }
                    if s.skipped > 0 {
                        tip.push_str(&format!("\n{} folders skipped (exclusion list)", tree::format_count(s.skipped)));
                    }
                    let color = if s.method == scan::Method::Mft { p.accent } else { p.muted };
                    badge(ui, &text, color).on_hover_text(tip);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.breadcrumbs(ui, p));
            });
        });
    }

    fn view_toggle(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let mut changed = None;
        for (view, label, tip) in [(View::Treemap, "▦", "Treemap"), (View::Sunburst, "◉", "Sunburst")] {
            let on = self.settings.view == view;
            let b = egui::Button::new(RichText::new(label).size(14.0).color(if on { p.accent } else { p.muted })).selected(on);
            if ui.add(b).on_hover_text(tip).clicked() && !on {
                changed = Some(view);
            }
        }
        if let Some(v) = changed {
            self.settings.view = v;
            self.save("ui", "view", Value::String(if v == View::Treemap { "treemap" } else { "sunburst" }.into()));
        }
    }

    fn tools_menu(&mut self, ui: &mut egui::Ui) {
        let has = self.loaded.is_some();
        ui.add_enabled_ui(has, |ui| {
            ui.menu_button("Tools", |ui| {
                if ui.button("Find duplicate files in this folder").clicked() {
                    self.pending.push(Action::Dupes(self.focus));
                    ui.close();
                }
                if ui.button("Skip folders…").clicked() {
                    self.settings_open = true;
                    ui.close();
                }
            });
        });
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("View", |ui| {
            let s = &mut self.settings;
            let mut changes: Vec<(&str, &str, Value)> = Vec::new();
            ui.label(RichText::new("Side tabs").weak());
            if ui.checkbox(&mut s.show_tree, "Folder tree").changed() {
                changes.push(("ui", "show_tree", Value::Boolean(s.show_tree)));
            }
            if ui.checkbox(&mut s.show_types, "File types").changed() {
                changes.push(("ui", "show_types", Value::Boolean(s.show_types)));
            }
            if ui.checkbox(&mut s.show_top_files, "Largest files").changed() {
                changes.push(("ui", "show_top_files", Value::Boolean(s.show_top_files)));
            }
            if ui.checkbox(&mut s.show_cleanup, "Cleanup hints").changed() {
                changes.push(("ui", "show_cleanup", Value::Boolean(s.show_cleanup)));
            }
            ui.separator();
            ui.label(RichText::new("Colours").weak());
            for (mode, label) in [(ColorMode::Folder, "By folder"), (ColorMode::Type, "Files by type"), (ColorMode::Age, "By age (newest file)")] {
                if ui.radio_value(&mut s.color, mode, label).changed() {
                    changes.push(("ui", "color_by", Value::String(mode.config().into())));
                }
            }
            ui.separator();
            if ui.checkbox(&mut s.show_free_space, "Free space in drive view").changed() {
                changes.push(("ui", "show_free_space", Value::Boolean(s.show_free_space)));
            }
            if ui.checkbox(&mut s.allocated, "Size on disk").changed() {
                changes.push(("ui", "size_mode", Value::String(if s.allocated { "allocated" } else { "logical" }.into())));
            }
            for (sec, key, v) in changes {
                self.save(sec, key, v);
            }
        });
    }

    fn breadcrumbs(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let crumbs = self.with_shown_tree(|t, focus| t.chain(focus).into_iter().map(|id| (id, if id == 0 { t.root_path.clone() } else { t.name(id).to_string() })).collect::<Vec<_>>());
        let Some(chain) = crumbs else {
            ui.label(RichText::new("Choose a drive or folder on the left, or drop one here").color(p.muted));
            return;
        };
        let skip = chain.len().saturating_sub(6);
        if skip > 0 {
            ui.label(RichText::new("…").color(p.muted));
        }
        let mut go = None;
        for (i, (id, name)) in chain.into_iter().enumerate().skip(skip) {
            if i > skip || skip > 0 {
                ui.label(RichText::new("›").color(p.muted));
            }
            let last = id == self.focus;
            let text = if last { RichText::new(name).family(theme::bold()) } else { RichText::new(name) };
            if ui.add(egui::Button::new(text).frame(false).truncate()).clicked() && !last {
                go = Some(id);
            }
        }
        if let Some(id) = go {
            self.navigate(id);
        }
    }

    fn drive_panel(&mut self, ui: &mut egui::Ui, p: &Palette) {
        if self.drives_at.elapsed() > Duration::from_secs(10) {
            self.drives = drives::list();
            self.drives_at = Instant::now();
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Drives").family(theme::bold()).size(13.0));
        ui.add_space(2.0);
        let scanning = self.running.as_ref().map(|r| r.job.path.clone());
        let current = self.loaded.as_ref().map(|l| l.tree.root_path.clone());
        let mut scan_path = None;
        for d in &self.drives {
            let active = current.as_deref() == Some(d.root.as_str()) || scanning.as_deref() == Some(d.root.as_str());
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
            let painter = ui.painter_at(rect);
            let bg = if active {
                p.accent.gamma_multiply(0.14)
            } else if resp.hovered() {
                ui.style().visuals.widgets.hovered.weak_bg_fill
            } else {
                Color32::TRANSPARENT
            };
            painter.rect_filled(rect, 6.0, bg);
            if active {
                painter.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 14.0), vec2(3.0, 24.0)), 2.0, p.accent);
            }
            let x = rect.left() + 10.0;
            painter.text(pos2(x, rect.top() + 14.0), Align2::LEFT_CENTER, theme::icon(theme::ICON_DRIVE, "").text(), FontId::new(16.0, egui::FontFamily::Name("icons".into())), p.muted);
            let title = if d.label.is_empty() { d.root.trim_end_matches('\\').to_string() } else { format!("{} ({})", d.label, d.root.trim_end_matches('\\')) };
            let tx = x + 24.0;
            let w = rect.right() - tx - 8.0;
            let title_g = painter.layout(title, FontId::proportional(12.0), p.text, w);
            painter.galley(pos2(tx, rect.top() + 6.0), title_g, p.text);
            let used = d.total.saturating_sub(d.free);
            let frac = if d.total > 0 { used as f32 / d.total as f32 } else { 0.0 };
            let bar = Rect::from_min_size(pos2(tx, rect.top() + 25.0), vec2(w, 5.0));
            painter.rect_filled(bar, 3.0, p.border);
            let fill = if frac > 0.9 { p.error } else { p.accent };
            painter.rect_filled(Rect::from_min_size(bar.min, vec2(w * frac, 5.0)), 3.0, fill);
            painter.text(pos2(tx, rect.top() + 41.0), Align2::LEFT_CENTER, format!("{} free of {}", tree::format_size(d.free), tree::format_size(d.total)), FontId::proportional(10.5), p.muted);
            let resp = resp.on_hover_text(format!("{}  {}\nClick to scan", d.root, d.filesystem));
            if resp.clicked() {
                scan_path = Some(d.root.clone());
            }
        }
        ui.add_space(8.0);
        if ui.add_sized([ui.available_width(), 28.0], egui::Button::new((theme::icon(theme::ICON_FOLDER, ""), "Scan folder…"))).clicked() {
            if let Some(dir) = rfd::FileDialog::new().set_title("Folder to scan").pick_folder() {
                scan_path = Some(dir.display().to_string());
            }
        }
        if let Some(path) = scan_path {
            self.start_scan(&path);
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let (text, color) = if self.elevated { ("Administrator: drives are read from the MFT", p.accent) } else { ("Folder walk (no admin rights needed)", p.muted) };
            ui.label(RichText::new(text).size(10.5).color(color));
            if !self.settings.exclude.is_empty() {
                ui.label(RichText::new(format!("{} folders on the skip list", self.settings.exclude.len())).size(10.5).color(p.muted)).on_hover_text(self.settings.exclude.join("\n"));
            }
        });
    }

    fn central(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let full = ui.available_rect_before_wrap();
        if self.loaded.is_none() {
            match self.running.as_ref().filter(|r| r.rescan.is_none()) {
                Some(r) => {
                    // A new scan: draw the tree as it grows, or a progress card until there is something to draw.
                    let live = r.job.progress.live();
                    let gen_now = r.job.progress.generation.load(Ordering::Relaxed);
                    if !(live.is_some_and(|l| self.live_view(ui, full, &l, gen_now, p))) {
                        self.progress_card(ui, full, p);
                    }
                }
                None => self.welcome(ui, full, p),
            }
            return;
        }
        let l = self.loaded.as_ref().unwrap();
        let tree = l.tree.clone();
        let (generation, ext_rank) = (l.generation, l.ext_rank.clone());
        let (out, resp) = self.draw_chart(ui, full, &tree, generation, &ext_rank, false);
        let nav = self.wheel(ui, out.hovered, &tree);
        self.apply_wheel(nav);
        // Context menu for the right-clicked element.
        let target = self.menu_target;
        let focus = self.focus;
        let mut picked = None;
        resp.context_menu(|ui| {
            let id = match target {
                Some(Target::Sector(Kind::Node(id))) => id,
                Some(Target::Center) => focus,
                Some(Target::Sector(Kind::Small { parent, .. })) => parent,
                _ => {
                    ui.label("Free space on the drive");
                    return;
                }
            };
            picked = item_menu(ui, &tree, id);
        });
        if let Some(a) = picked {
            self.pending.push(a);
        }
        self.handle_chart_output(out, &tree);
        self.highlight_chip(ui, full, p);
    }

    /// The chart of a running walk; false while there is nothing to draw yet.
    fn live_view(&mut self, ui: &mut egui::Ui, full: Rect, live: &std::sync::Mutex<Tree>, gen_now: u64, p: &Palette) -> bool {
        if self.live_at.elapsed() > LIVE_REFRESH {
            self.live_generation = gen_now;
            self.live_at = Instant::now();
        }
        ui.ctx().request_repaint_after(LIVE_REFRESH);
        let tree = live.lock().unwrap();
        if tree.len() <= 1 || (self.focus as usize) >= tree.len() {
            return false;
        }
        let strip = Rect::from_min_size(full.min, vec2(full.width(), 34.0));
        let rect = Rect::from_min_max(pos2(full.left(), strip.bottom()), full.max);
        self.progress_strip(ui, strip, p);
        let (out, _) = self.draw_chart(ui, rect, &tree, self.live_generation, &[], true);
        let wheel = self.wheel(ui, out.hovered, &tree);
        // Browsing only: actions wait until the scan has finished.
        self.hovered = out.hovered;
        self.menu_target = None;
        let is_dir = |id: u32| tree.nodes[id as usize].is_dir();
        let nav = match out.clicked {
            Some(Target::Center) => Some(tree.nodes[self.focus as usize].parent).filter(|&p| p != NONE),
            Some(Target::Sector(Kind::Node(id))) if is_dir(id) => Some(id),
            Some(Target::Sector(Kind::Small { parent, .. })) => Some(parent),
            _ => None,
        };
        let parent = tree.nodes[self.focus as usize].parent;
        drop(tree);
        if let Some(id) = nav {
            self.navigate(id);
        }
        match wheel {
            Some(WheelNav::Into(id)) => self.navigate(id),
            Some(WheelNav::Up) if parent != NONE => self.navigate(parent),
            _ => {}
        }
        true
    }

    /// Above the chart while something is highlighted: what, and a button to clear it.
    fn highlight_chip(&mut self, ui: &mut egui::Ui, rect: Rect, p: &Palette) {
        let Some((label, _)) = &self.highlight else { return };
        let label = label.clone();
        let area = Rect::from_min_size(rect.min + vec2(12.0, 10.0), vec2(rect.width() - 24.0, 28.0));
        let mut clear = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(14.0).inner_margin(Margin::symmetric(10, 3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).size(11.5));
                    if ui.small_button("✕").on_hover_text("Clear highlight (Esc)").clicked() {
                        clear = true;
                    }
                });
            });
        });
        if clear {
            self.highlight = None;
        }
    }

    fn progress_strip(&mut self, ui: &mut egui::Ui, strip: Rect, p: &Palette) {
        let r = self.running.as_ref().unwrap();
        let pr = &r.job.progress;
        let text = format!(
            "Scanning {} · {} files · {} · {:.0} s — explore while it runs",
            r.job.path,
            tree::format_count(pr.items.load(Ordering::Relaxed)),
            tree::format_size(pr.bytes.load(Ordering::Relaxed)),
            r.job.started.elapsed().as_secs_f32()
        );
        let mut cancel = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(strip.shrink2(vec2(12.0, 4.0))), |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(text).color(p.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        });
        let bar = Rect::from_min_size(pos2(strip.left(), strip.bottom() - 2.0), vec2(strip.width(), 2.0));
        let t = (r.job.started.elapsed().as_secs_f32() * 0.6).fract();
        ui.painter().rect_filled(Rect::from_min_size(pos2(bar.left() + bar.width() * t * 0.8, bar.top()), vec2(bar.width() * 0.2, 2.0)), 1.0, p.accent);
        if cancel {
            pr.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn progress_card(&mut self, ui: &mut egui::Ui, rect: Rect, p: &Palette) {
        let r = self.running.as_ref().unwrap();
        let pr = &r.job.progress;
        let items = pr.items.load(Ordering::Relaxed);
        let bytes = pr.bytes.load(Ordering::Relaxed);
        let elapsed = r.job.started.elapsed().as_secs_f32();
        let mut cancel = false;
        let card = Rect::from_center_size(rect.center(), vec2(380.0, 170.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
            egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(10.0).inner_margin(Margin::same(20)).show(ui, |ui| {
                ui.set_width(card.width() - 40.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("Scanning {}", r.job.path)).family(theme::bold()).size(14.0));
                });
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{} files · {}", tree::format_count(items), tree::format_size(bytes))).size(13.0));
                ui.label(RichText::new(format!("{elapsed:.1} s")).color(p.muted));
                ui.add_space(6.0);
                match pr.fraction() {
                    Some(f) => ui.add(egui::ProgressBar::new(f).desired_height(6.0)),
                    None => ui.add(egui::ProgressBar::new(0.0).animate(true).desired_height(6.0)),
                };
                ui.add_space(6.0);
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        if cancel {
            pr.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui, rect: Rect, p: &Palette) {
        let painter = ui.painter_at(rect);
        let c = rect.center();
        painter.text(c + vec2(0.0, -20.0), Align2::CENTER_CENTER, "spacewiz", FontId::new(30.0, theme::bold()), p.text);
        painter.text(c + vec2(0.0, 14.0), Align2::CENTER_CENTER, "Pick a drive on the left, or drop a folder here, to see where the space goes.", FontId::proportional(14.0), p.muted);
        let hint = if self.elevated {
            "Running as administrator: NTFS drives are read from the MFT in seconds."
        } else if self.can_elevate {
            "Tip: \"Fast scan\" restarts as administrator to read NTFS drives in seconds."
        } else {
            "The chart fills in while folders are scanned; skip slow folders in Settings."
        };
        painter.text(c + vec2(0.0, 38.0), Align2::CENTER_CENTER, hint, FontId::proportional(12.0), p.muted);
    }

    fn tabs(&self) -> Vec<Tab> {
        let s = &self.settings;
        let mut v = Vec::new();
        if s.show_tree {
            v.push(Tab::Folders);
        }
        if s.show_types {
            v.push(Tab::Types);
        }
        if s.show_top_files {
            v.push(Tab::Largest);
        }
        if s.show_cleanup {
            v.push(Tab::Cleanup);
        }
        if self.search.is_some() || self.search_job.is_some() {
            v.push(Tab::Search);
        }
        if self.dupes.is_some() || self.dupes_job.is_some() {
            v.push(Tab::Dupes);
        }
        v
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let tabs = self.tabs();
        if !tabs.contains(&self.tab) {
            self.tab = tabs[0];
        }
        ui.horizontal_wrapped(|ui| {
            for t in &tabs {
                let on = self.tab == *t;
                let text = RichText::new(t.title()).size(12.5);
                let text = if on { text.family(theme::bold()).color(p.accent) } else { text.color(p.muted) };
                let resp = ui.add(egui::Button::new(text).frame(false));
                if on {
                    let r = resp.rect;
                    ui.painter().rect_filled(Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() - 2.0), vec2(r.width() - 12.0, 2.0)), 1.0, p.accent);
                }
                if resp.clicked() {
                    self.tab = *t;
                }
            }
        });
        ui.separator();
        self.hovered_from_list = None;
        match self.tab {
            Tab::Folders => self.tree_view(ui, p),
            Tab::Types => self.types_view(ui, p),
            Tab::Largest => self.top_files_view(ui, p),
            Tab::Cleanup => self.cleanup_view(ui, p),
            Tab::Search => self.search_view(ui, p),
            Tab::Dupes => self.dupes_view(ui, p),
        }
    }

    fn tree_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        let allocated = self.settings.allocated;
        // Visible rows: children of the focus, recursively for expanded folders.
        let mut rows: Vec<(u32, usize)> = Vec::new();
        let mut stack: Vec<(u32, usize)> = t.children(self.focus).take(500).map(|c| (c, 0)).collect();
        stack.reverse();
        while let Some((id, depth)) = stack.pop() {
            rows.push((id, depth));
            if rows.len() > 20_000 {
                break;
            }
            if self.expanded.contains(&id) {
                let kids: Vec<u32> = t.children(id).take(500).collect();
                for &k in kids.iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
        let parent_size = t.nodes[self.focus as usize].size_of(allocated).max(1);
        let hovered = self.hovered.and_then(Kind::node);
        let mut toggle = None;
        let mut action = None;
        let mut list_hover = None;
        let mut drag = None;
        egui::ScrollArea::vertical().id_salt("tree").auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for &(id, depth) in &rows[range] {
                let n = &t.nodes[id as usize];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                let sel = self.selected == Some(id);
                if sel || hovered == Some(id) || resp.hovered() {
                    let c = if sel { p.accent.gamma_multiply(0.22) } else { ui.style().visuals.widgets.hovered.weak_bg_fill };
                    painter.rect_filled(rect, 4.0, c);
                }
                let size = n.size_of(allocated);
                let right = rect.right() - 4.0;
                let (size_w, bar_w) = (66.0, 44.0);
                let name_right = right - size_w - bar_w - 10.0;
                let x = rect.left() + 4.0 + depth as f32 * 14.0;
                if n.is_dir() && n.first_child != NONE {
                    let arrow = if self.expanded.contains(&id) { "⏷" } else { "⏵" };
                    let ar = Rect::from_min_size(pos2(x, rect.top()), vec2(14.0, ROW_H));
                    painter.text(ar.center(), Align2::CENTER_CENTER, arrow, FontId::proportional(10.0), p.muted);
                    if resp.clicked() && resp.interact_pointer_pos().is_some_and(|pt| ar.contains(pt)) {
                        toggle = Some(id);
                    }
                }
                let skipped = n.flags & tree::SKIPPED != 0;
                let glyph = if n.is_dir() { '\u{E8B7}' } else if n.flags & tree::CLOUD != 0 { '\u{E753}' } else { '\u{E7C3}' };
                let icon_color = if skipped { p.muted } else if n.is_dir() { Color32::from_rgb(0xe8, 0xb3, 0x39) } else { p.muted };
                painter.text(pos2(x + 16.0, rect.center().y), Align2::LEFT_CENTER, glyph, FontId::new(13.0, egui::FontFamily::Name("icons".into())), icon_color);
                let name_x = x + 34.0;
                let name = if skipped { format!("{}  (skipped)", t.name(id)) } else { t.name(id).to_string() };
                let galley = painter.layout(name, FontId::proportional(12.0), if skipped { p.muted } else { p.text }, (name_right - name_x).max(10.0));
                let clip = Rect::from_min_max(pos2(name_x, rect.top()), pos2(name_right, rect.bottom()));
                painter.with_clip_rect(clip).galley(pos2(name_x, rect.center().y - galley.size().y / 2.0), galley, p.text);
                let frac = size as f32 / parent_size as f32;
                let bar = Rect::from_min_size(pos2(name_right + 4.0, rect.center().y - 3.0), vec2(bar_w, 6.0));
                painter.rect_filled(bar, 3.0, p.border);
                painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar_w * frac.clamp(0.0, 1.0), 6.0)), 3.0, p.accent.gamma_multiply(0.85));
                painter.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, tree::format_size(size), FontId::proportional(11.5), p.text);
                if resp.hovered() {
                    list_hover = Some(id);
                }
                if resp.drag_started() {
                    drag = Some(id);
                }
                if resp.clicked() && toggle != Some(id) {
                    self.selected = Some(id);
                }
                if resp.double_clicked() {
                    action = Some(if n.is_dir() { Action::Zoom(id) } else { Action::Open(id) });
                }
                let resp = resp.on_hover_text(format!(
                    "{}\n{}{}",
                    t.path(id),
                    tree::format_size(size),
                    if n.is_dir() { format!(" · {} files", tree::format_count(n.files as u64)) } else { format!(" · {}", tree::format_time(n.mtime)) }
                ));
                resp.context_menu(|ui| {
                    if let Some(a) = item_menu(ui, &t, id) {
                        action = Some(a);
                    }
                });
            }
        });
        self.hovered_from_list = list_hover;
        if let Some(id) = toggle {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
        }
        if let Some(id) = drag {
            self.drag_request = Some(PathBuf::from(t.path(id)));
        }
        if let Some(a) = action {
            self.pending.push(a);
        }
    }

    fn types_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        let dark = ui.style().visuals.dark_mode;
        let total = t.nodes[0].size_of(self.settings.allocated).max(1);
        let current = self.highlight.as_ref().map(|(lbl, _)| lbl.clone()).unwrap_or_default();
        let mut toggle = None;
        egui::ScrollArea::vertical().id_salt("types").auto_shrink([false, false]).show_rows(ui, ROW_H, t.ext_totals.len().min(500), |ui, range| {
            for (rank, et) in t.ext_totals.iter().enumerate().take(range.end).skip(range.start) {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let painter = ui.painter_at(rect);
                let name = if et.ext == 0 { "(no extension)".to_string() } else { format!(".{}", t.exts[et.ext as usize]) };
                let on = current == type_label(&name);
                if on || resp.hovered() {
                    painter.rect_filled(rect, 4.0, if on { p.accent.gamma_multiply(0.22) } else { ui.style().visuals.widgets.hovered.weak_bg_fill });
                }
                let sw = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(10.0, 10.0));
                painter.rect_filled(sw, 3.0, if et.ext == 0 { p.small } else { theme::type_color(rank, dark) });
                painter.text(pos2(rect.left() + 24.0, rect.center().y), Align2::LEFT_CENTER, &name, FontId::proportional(12.0), p.text);
                let size = if self.settings.allocated { et.alloc } else { et.size };
                let right = rect.right() - 4.0;
                painter.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, tree::format_size(size), FontId::proportional(11.5), p.text);
                painter.text(pos2(right - 72.0, rect.center().y), Align2::RIGHT_CENTER, tree::format_count(et.count as u64), FontId::proportional(11.0), p.muted);
                let frac = size as f32 / total as f32;
                let bar = Rect::from_min_size(pos2(right - 72.0 - 104.0, rect.center().y - 3.0), vec2(44.0, 6.0));
                if bar.left() > rect.left() + 110.0 {
                    painter.rect_filled(bar, 3.0, p.border);
                    painter.rect_filled(Rect::from_min_size(bar.min, vec2(44.0 * frac.clamp(0.0, 1.0), 6.0)), 3.0, p.accent.gamma_multiply(0.85));
                }
                if resp.on_hover_text("Click to highlight these files in the chart").clicked() {
                    toggle = Some((et.ext, name, on));
                }
            }
        });
        if let Some((ext, name, on)) = toggle {
            self.highlight = if on { None } else { Some((type_label(&name), Highlight::by_ext(&t, ext))) };
        }
    }

    /// Rows with a name, a size and the parent path below; shared by the largest-files and search tabs.
    fn file_list(&mut self, ui: &mut egui::Ui, p: &Palette, salt: &str, ids: &[u32], t: &Arc<Tree>) {
        let mut go = None;
        let mut action = None;
        let mut list_hover = None;
        let mut drag = None;
        egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show_rows(ui, 34.0, ids.len(), |ui, range| {
            for &id in &ids[range] {
                let n = &t.nodes[id as usize];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                if resp.hovered() || self.selected == Some(id) {
                    painter.rect_filled(rect, 4.0, if self.selected == Some(id) { p.accent.gamma_multiply(0.18) } else { ui.style().visuals.widgets.hovered.weak_bg_fill });
                    if resp.hovered() {
                        list_hover = Some(id);
                    }
                }
                let right = rect.right() - 4.0;
                painter.text(pos2(right, rect.top() + 10.0), Align2::RIGHT_CENTER, tree::format_size(n.size_of(self.settings.allocated)), FontId::proportional(11.5), p.text);
                let icon = if n.is_dir() { '\u{E8B7}' } else { '\u{E7C3}' };
                painter.text(
                    pos2(rect.left() + 4.0, rect.top() + 10.0),
                    Align2::LEFT_CENTER,
                    icon,
                    FontId::new(12.0, egui::FontFamily::Name("icons".into())),
                    if n.is_dir() { Color32::from_rgb(0xe8, 0xb3, 0x39) } else { p.muted },
                );
                let name = painter.layout(t.name(id).to_string(), FontId::proportional(12.0), p.text, rect.width() - 100.0);
                let clip = Rect::from_min_max(rect.min, pos2(right - 70.0, rect.bottom()));
                painter.with_clip_rect(clip).galley(pos2(rect.left() + 22.0, rect.top() + 3.0), name, p.text);
                let parent = if n.parent == NONE { String::new() } else { t.path(n.parent) };
                painter.with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom()))).text(pos2(rect.left() + 22.0, rect.top() + 25.0), Align2::LEFT_CENTER, parent, FontId::proportional(10.5), p.muted);
                if resp.drag_started() {
                    drag = Some(id);
                }
                if resp.double_clicked() {
                    action = Some(if n.is_dir() { Action::Zoom(id) } else { Action::Open(id) });
                } else if resp.clicked() {
                    go = Some(id);
                }
                let resp = resp.on_hover_text(t.path(id));
                resp.context_menu(|ui| {
                    if let Some(a) = item_menu(ui, t, id) {
                        action = Some(a);
                    }
                });
            }
        });
        if list_hover.is_some() {
            self.hovered_from_list = list_hover;
        }
        if let Some(id) = drag {
            self.drag_request = Some(PathBuf::from(t.path(id)));
        }
        if let Some(id) = go {
            self.reveal(id);
        }
        if let Some(a) = action {
            self.pending.push(a);
        }
    }

    fn top_files_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        let ids = t.top_files.clone();
        self.file_list(ui, p, "top", &ids, &t);
    }

    fn search_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        let Some(r) = &self.search else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Searching…");
            });
            return;
        };
        let more = if r.count > r.ids.len() { format!(" (largest {} listed)", tree::format_count(r.ids.len() as u64)) } else { String::new() };
        ui.label(RichText::new(format!("{} matches · {}{more}", tree::format_count(r.count as u64), tree::format_size(r.bytes))).family(theme::bold()));
        ui.label(RichText::new("*, ? wildcards · several terms · >100MB <1KB · file: folder:").size(10.5).color(p.muted));
        let ids = r.ids.clone();
        self.file_list(ui, p, "search", &ids, &t);
    }

    fn cleanup_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        if l.hints.is_empty() {
            ui.label(RichText::new("No known caches, temp or build folders of 1 MB or more found.").color(p.muted));
            return;
        }
        let total: u64 = l.hints.iter().map(|h| t.nodes[h.id as usize].size).sum();
        ui.label(RichText::new(format!("{} places · {} could be reclaimed", l.hints.len(), tree::format_size(total))).family(theme::bold()));
        ui.label(RichText::new("Suggestions only; check before deleting. Hover for details.").size(10.5).color(p.muted));
        let hints: Vec<(u32, &'static str, &'static str)> = l.hints.iter().map(|h| (h.id, h.category, h.note)).collect();
        let mut go = None;
        let mut action = None;
        let mut list_hover = None;
        egui::ScrollArea::vertical().id_salt("cleanup").auto_shrink([false, false]).show_rows(ui, 38.0, hints.len(), |ui, range| {
            for &(id, category, note) in &hints[range] {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                let painter = ui.painter_at(rect);
                if resp.hovered() {
                    painter.rect_filled(rect, 4.0, ui.style().visuals.widgets.hovered.weak_bg_fill);
                    list_hover = Some(id);
                }
                let right = rect.right() - 4.0;
                painter.text(pos2(right, rect.top() + 11.0), Align2::RIGHT_CENTER, tree::format_size(t.nodes[id as usize].size), FontId::new(12.0, theme::bold()), p.text);
                let cat = painter.layout_no_wrap(category.to_string(), FontId::proportional(10.0), p.accent);
                let chip = Rect::from_min_size(pos2(rect.left() + 4.0, rect.top() + 4.0), cat.size() + vec2(10.0, 4.0));
                painter.rect_filled(chip, 7.0, p.accent.gamma_multiply(0.14));
                painter.galley(chip.min + vec2(5.0, 2.0), cat, p.accent);
                let clip = Rect::from_min_max(rect.min, pos2(right - 70.0, rect.bottom()));
                painter.with_clip_rect(clip).text(pos2(chip.right() + 6.0, chip.center().y), Align2::LEFT_CENTER, t.name(id), FontId::proportional(12.0), p.text);
                painter.with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom()))).text(pos2(rect.left() + 4.0, rect.top() + 29.0), Align2::LEFT_CENTER, t.path(id), FontId::proportional(10.5), p.muted);
                let resp = resp.on_hover_text(note);
                if resp.clicked() {
                    go = Some(id);
                }
                resp.context_menu(|ui| {
                    if let Some(a) = item_menu(ui, &t, id) {
                        action = Some(a);
                    }
                });
            }
        });
        if list_hover.is_some() {
            self.hovered_from_list = list_hover;
        }
        if let Some(id) = go {
            self.highlight = Some((format!("Cleanup: {}", t.name(id)), Highlight::from_nodes(&t, t.subtree(id))));
            self.reveal(id);
        }
        if let Some(a) = action {
            self.pending.push(a);
        }
    }

    fn dupes_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some(l) = &self.loaded else { return };
        let t = l.tree.clone();
        if let Some(j) = &self.dupes_job {
            let pr = &j.progress;
            let (done, total) = (pr.done.load(Ordering::Relaxed), pr.total.load(Ordering::Relaxed));
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Comparing files in {} …", j.path));
            });
            ui.label(RichText::new(format!("{} of {} candidates · {} read", tree::format_count(done), tree::format_count(total), tree::format_size(pr.bytes.load(Ordering::Relaxed)))).color(p.muted));
            if ui.button("Cancel").clicked() {
                self.cancel_dupes();
            }
            ui.ctx().request_repaint_after(Duration::from_millis(200));
            return;
        }
        let Some(r) = &self.dupes else { return };
        ui.label(RichText::new(format!("{} wasted in {} groups", tree::format_size(r.wasted), tree::format_count(r.groups.len() as u64))).family(theme::bold()));
        let mut note = format!("in {}, files ≥ {} MB", t.path(r.root), self.settings.dupes_min_mb);
        if r.skipped > 0 {
            note.push_str(&format!(" · {} skipped (online-only or unreadable)", tree::format_count(r.skipped)));
        }
        ui.label(RichText::new(note).size(10.5).color(p.muted));
        // Flattened rows: group headers and, for open groups, their files.
        let mut rows: Vec<(usize, Option<u32>)> = Vec::new();
        for (gi, g) in r.groups.iter().enumerate() {
            rows.push((gi, None));
            if self.dupes_open.contains(&gi) {
                rows.extend(g.ids.iter().map(|&id| (gi, Some(id))));
            }
        }
        let groups = r.groups.clone();
        let mut toggle = None;
        let mut go = None;
        let mut action = None;
        let mut list_hover = None;
        let mut drag = None;
        egui::ScrollArea::vertical().id_salt("dupes").auto_shrink([false, false]).show_rows(ui, 26.0, rows.len(), |ui, range| {
            for &(gi, file) in &rows[range] {
                let g = &groups[gi];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                if resp.hovered() {
                    painter.rect_filled(rect, 4.0, ui.style().visuals.widgets.hovered.weak_bg_fill);
                }
                let right = rect.right() - 4.0;
                match file {
                    None => {
                        let open = self.dupes_open.contains(&gi);
                        painter.text(pos2(rect.left() + 8.0, rect.center().y), Align2::CENTER_CENTER, if open { "⏷" } else { "⏵" }, FontId::proportional(10.0), p.muted);
                        let title = format!("{} × {}", g.ids.len(), tree::format_size(g.size));
                        painter.text(pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, &title, FontId::new(12.0, theme::bold()), p.text);
                        let name_x = rect.left() + 110.0;
                        let clip = Rect::from_min_max(pos2(name_x, rect.top()), pos2(right - 80.0, rect.bottom()));
                        painter.with_clip_rect(clip).text(pos2(name_x, rect.center().y), Align2::LEFT_CENTER, t.name(g.ids[0]), FontId::proportional(12.0), p.text);
                        painter.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, format!("−{}", tree::format_size(g.wasted())), FontId::proportional(11.5), p.warn);
                        if resp.clicked() {
                            toggle = Some(gi);
                        }
                    }
                    Some(id) => {
                        if resp.hovered() {
                            list_hover = Some(id);
                        }
                        let clip = Rect::from_min_max(rect.min, pos2(right, rect.bottom()));
                        painter.with_clip_rect(clip).text(pos2(rect.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, t.path(id), FontId::proportional(11.0), p.text);
                        if resp.drag_started() {
                            drag = Some(id);
                        }
                        if resp.double_clicked() {
                            action = Some(Action::Open(id));
                        } else if resp.clicked() {
                            go = Some(id);
                        }
                        resp.context_menu(|ui| {
                            if let Some(a) = item_menu(ui, &t, id) {
                                action = Some(a);
                            }
                        });
                    }
                }
            }
        });
        if list_hover.is_some() {
            self.hovered_from_list = list_hover;
        }
        if let Some(gi) = toggle {
            if !self.dupes_open.remove(&gi) {
                self.dupes_open.insert(gi);
                let g = &groups[gi];
                self.highlight = Some((format!("Duplicates: {} × {}", g.ids.len(), t.name(g.ids[0])), Highlight::from_nodes(&t, g.ids.iter().copied())));
                // Show the folder that contains all copies.
                self.navigate(common_ancestor(&t, &g.ids));
            }
        }
        if let Some(id) = drag {
            self.drag_request = Some(PathBuf::from(t.path(id)));
        }
        if let Some(id) = go {
            self.reveal(id);
        }
        if let Some(a) = action {
            self.pending.push(a);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.horizontal(|ui| {
            if let Some((text, error, at)) = &self.message {
                if at.elapsed() < Duration::from_secs(6) {
                    ui.label(RichText::new(text).color(if *error { p.error } else { p.accent }));
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                    return;
                }
            }
            if let Some(r) = &self.running {
                if r.rescan.is_some() {
                    ui.spinner();
                    ui.label(format!("Rescanning {} …", r.job.path));
                    return;
                }
            }
            let Some(l) = &self.loaded else {
                ui.label(RichText::new(if self.running.is_some() { "Scanning…" } else { "Ready" }).color(p.muted));
                return;
            };
            let t = &l.tree;
            match self.hovered {
                Some(Kind::Node(id)) => {
                    let n = &t.nodes[id as usize];
                    ui.label(RichText::new(tree::format_size(n.size_of(self.settings.allocated))).family(theme::bold()));
                    ui.label(RichText::new(t.path(id)).color(p.muted));
                }
                Some(Kind::Small { count, bytes, .. }) => {
                    ui.label(format!("{} small items, {} together. Click to zoom in.", tree::format_count(count as u64), tree::format_size(bytes)));
                }
                Some(Kind::Free(free)) => {
                    ui.label(format!("{} free", tree::format_size(free)));
                }
                None => {
                    let root = &t.nodes[0];
                    let mut s = format!(
                        "{}: {} in {} files and {} folders",
                        t.root_path,
                        tree::format_size(root.size_of(self.settings.allocated)),
                        tree::format_count(root.files as u64),
                        tree::format_count(root.dirs as u64)
                    );
                    if let Some((total, free)) = l.space {
                        s.push_str(&format!("  ·  drive {} used of {}", tree::format_size(total - free), tree::format_size(total)));
                    }
                    if l.stats.skipped > 0 {
                        s.push_str(&format!("  ·  {} folders skipped", tree::format_count(l.stats.skipped)));
                    }
                    ui.label(RichText::new(s).color(p.muted));
                }
            }
        });
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut changes: Vec<(&str, &str, Value)> = Vec::new();
        let mut theme_changed = false;
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new("Settings").family(theme::bold()).size(16.0));
            ui.add_space(8.0);
            let s = &mut self.settings;
            egui::Grid::new("settings-grid").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    for (v, label) in [("system", "System"), ("light", "Light"), ("dark", "Dark")] {
                        if ui.radio_value(&mut s.theme, v.to_string(), label).changed() {
                            changes.push(("ui", "theme", Value::String(v.into())));
                            theme_changed = true;
                        }
                    }
                });
                ui.end_row();
                ui.label("Side tabs");
                ui.vertical(|ui| {
                    if ui.checkbox(&mut s.show_tree, "Folder tree").changed() {
                        changes.push(("ui", "show_tree", Value::Boolean(s.show_tree)));
                    }
                    if ui.checkbox(&mut s.show_types, "File types").changed() {
                        changes.push(("ui", "show_types", Value::Boolean(s.show_types)));
                    }
                    if ui.checkbox(&mut s.show_top_files, "Largest files").changed() {
                        changes.push(("ui", "show_top_files", Value::Boolean(s.show_top_files)));
                    }
                    if ui.checkbox(&mut s.show_cleanup, "Cleanup hints").changed() {
                        changes.push(("ui", "show_cleanup", Value::Boolean(s.show_cleanup)));
                    }
                });
                ui.end_row();
                ui.label("Levels shown");
                if ui.add(egui::Slider::new(&mut s.ring_depth, 3..=10)).changed() {
                    changes.push(("ui", "ring_depth", Value::Integer(s.ring_depth as i64)));
                }
                ui.end_row();
                ui.label("Size");
                ui.horizontal(|ui| {
                    let a = ui.radio_value(&mut s.allocated, false, "Length").changed();
                    let b = ui.radio_value(&mut s.allocated, true, "On disk").changed();
                    if a || b {
                        changes.push(("ui", "size_mode", Value::String(if s.allocated { "allocated" } else { "logical" }.into())));
                    }
                });
                ui.end_row();
                ui.label("Colours");
                ui.horizontal(|ui| {
                    for (mode, label) in [(ColorMode::Folder, "By folder"), (ColorMode::Type, "By type"), (ColorMode::Age, "By age")] {
                        if ui.radio_value(&mut s.color, mode, label).changed() {
                            changes.push(("ui", "color_by", Value::String(mode.config().into())));
                        }
                    }
                });
                ui.end_row();
                ui.label("Drive view");
                if ui.checkbox(&mut s.show_free_space, "Show free space").changed() {
                    changes.push(("ui", "show_free_space", Value::Boolean(s.show_free_space)));
                }
                ui.end_row();
                ui.label("Duplicates");
                ui.horizontal(|ui| {
                    if ui.add(egui::Slider::new(&mut s.dupes_min_mb, 0..=1024).logarithmic(true).suffix(" MB")).changed() {
                        changes.push(("dupes", "min_size_mb", Value::Integer(s.dupes_min_mb)));
                    }
                    ui.label(RichText::new("smallest file").weak());
                });
                ui.end_row();
                ui.label("Scanning");
                if ui.checkbox(&mut s.prefer_mft, "Read the MFT when administrator").changed() {
                    changes.push(("scan", "prefer_mft", Value::Boolean(s.prefer_mft)));
                }
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Skip folders").family(theme::bold()));
            ui.label(RichText::new("One per line: a full path (C:\\Windows\\WinSxS) or a folder name anywhere (node_modules). Applies from the next scan.").size(11.0).weak());
            ui.add(egui::TextEdit::multiline(&mut self.exclude_text).desired_rows(4).desired_width(f32::INFINITY).hint_text("C:\\Windows\\WinSxS"));
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if resp.should_close() || close {
            self.settings_open = false;
            let list: Vec<String> = self.exclude_text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            if list != self.settings.exclude {
                self.settings.exclude = list;
                self.save_exclude();
            }
        }
        for (sec, key, v) in changes {
            self.save(sec, key, v);
        }
        if theme_changed {
            self.apply_theme(ctx);
        }
    }

    fn recycle_dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(id) = self.confirm_recycle else { return };
        let Some(l) = &self.loaded else { return };
        let (path, size, is_dir, files) = {
            let n = &l.tree.nodes[id as usize];
            (l.tree.path(id), n.size, n.is_dir(), n.files)
        };
        let mut answer = None;
        let resp = egui::Modal::new(egui::Id::unique("recycle")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Move to Recycle Bin?").family(theme::bold()).size(16.0));
            ui.add_space(6.0);
            ui.label(RichText::new(&path).color(p.text));
            let what = if is_dir { format!("{} in {} files", tree::format_size(size), tree::format_count(files as u64)) } else { tree::format_size(size) };
            ui.label(RichText::new(what).color(p.muted));
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
                let b = egui::Button::new(RichText::new("Move to Recycle Bin").color(p.on_accent)).fill(p.accent);
                if ui.add(b).clicked() {
                    answer = Some(true);
                }
            });
        });
        if resp.should_close() && answer.is_none() {
            answer = Some(false);
        }
        if let Some(yes) = answer {
            self.confirm_recycle = None;
            if yes {
                self.do_recycle(id);
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if self.settings_open || self.confirm_recycle.is_some() {
            return;
        }
        let (back, fwd, up, refresh, find, esc) = ctx.input(|i| {
            let alt = i.modifiers.alt;
            (
                (alt && i.key_pressed(egui::Key::ArrowLeft)) || i.pointer.button_pressed(egui::PointerButton::Extra1),
                (alt && i.key_pressed(egui::Key::ArrowRight)) || i.pointer.button_pressed(egui::PointerButton::Extra2),
                i.key_pressed(egui::Key::Backspace) || (alt && i.key_pressed(egui::Key::ArrowUp)),
                i.key_pressed(egui::Key::F5),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if find && self.loaded.is_some() {
            self.focus_search = true;
        }
        if esc {
            if !self.search_text.is_empty() {
                self.search_text.clear();
                self.clear_search();
            }
            self.highlight = None;
        }
        // Keys typed into a text box are not navigation.
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        if back {
            self.go_back();
        }
        if fwd {
            self.go_forward();
        }
        if up {
            self.go_up();
        }
        if refresh && self.running.is_none() && self.loaded.is_some() {
            self.start_rescan(self.focus);
        }
    }

    /// A folder dropped on the window is scanned (a file: its folder).
    fn dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(path) = dropped.into_iter().next() {
            let dir = if path.is_dir() { Some(path) } else { path.parent().map(|p| p.to_path_buf()) };
            if let Some(dir) = dir {
                self.start_scan(&dir.display().to_string());
            }
        }
    }
}

fn type_label(name: &str) -> String {
    format!("Type {name}")
}

/// Deepest folder containing all the given nodes.
fn common_ancestor(t: &Tree, ids: &[u32]) -> u32 {
    let mut chain = t.chain(t.nodes[ids[0] as usize].parent);
    for &id in &ids[1..] {
        let other = t.chain(t.nodes[id as usize].parent);
        let n = chain.iter().zip(&other).take_while(|(a, b)| a == b).count();
        chain.truncate(n.max(1));
    }
    *chain.last().unwrap_or(&0)
}

/// Context menu entries for a file or folder; returns the chosen action.
fn item_menu(ui: &mut egui::Ui, t: &Tree, id: u32) -> Option<Action> {
    let n = &t.nodes[id as usize];
    let mut a = None;
    ui.label(RichText::new(if id == 0 { t.root_path.clone() } else { t.name(id).to_string() }).family(theme::bold()));
    ui.label(RichText::new(tree::format_size(n.size)).weak());
    ui.separator();
    if n.is_dir() && ui.button("Zoom in").clicked() {
        a = Some(Action::Zoom(id));
    }
    if ui.button("Open").clicked() {
        a = Some(Action::Open(id));
    }
    if ui.button("Show in Explorer").clicked() {
        a = Some(Action::Explorer(id));
    }
    if ui.button("Copy path").clicked() {
        a = Some(Action::CopyPath(id));
    }
    if n.is_dir() {
        if ui.button("Rescan folder").clicked() {
            a = Some(Action::Rescan(id));
        }
        if ui.button("Find duplicate files here").clicked() {
            a = Some(Action::Dupes(id));
        }
        if id != 0 && ui.button("Skip in future scans").clicked() {
            a = Some(Action::Skip(id));
        }
    }
    ui.separator();
    if id != 0 && ui.button("Move to Recycle Bin…").clicked() {
        a = Some(Action::Recycle(id));
    }
    if a.is_some() {
        ui.close();
    }
    a
}

fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.0), color);
    let size = galley.size() + vec2(16.0, 8.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(rect, 10.0, color.gamma_multiply(0.12), Stroke::new(1.0, color.gamma_multiply(0.4)), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan();
        self.poll_search();
        self.poll_dupes();
        if self.running.is_some() || self.search_edited.is_some() || self.search_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.dropped(ctx);
        self.keys(ctx);
        for a in std::mem::take(&mut self.pending) {
            self.apply(ctx, a);
        }
    }

    /// After dragging a file out, Windows swallowed the mouse release; tell egui the button is up again.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.release_after_drag {
            self.release_after_drag = false;
            let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
            raw_input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dark = ui.style().visuals.dark_mode;
        let p = theme::palette(dark);
        egui::Panel::top("toolbar").frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(10, 8))).show(ui, |ui| {
            self.toolbar(ui, p);
        });
        egui::Panel::bottom("status").frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(12, 5))).show(ui, |ui| {
            self.status_bar(ui, p);
        });
        egui::Panel::left("drives").resizable(false).default_size(214.0).frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(8, 8))).show(ui, |ui| {
            self.drive_panel(ui, p);
        });
        if self.loaded.is_some() && !self.tabs().is_empty() {
            egui::Panel::right("side").resizable(true).default_size(340.0).min_size(240.0).frame(egui::Frame::side_top_panel(ui.style()).inner_margin(Margin::symmetric(8, 6))).show(ui, |ui| {
                self.side_panel(ui, p);
            });
        } else {
            self.hovered_from_list = None;
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(p.bg)).show(ui, |ui| {
            self.central(ui, p);
            // Files dragged over the window from Explorer.
            if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
                let rect = ui.max_rect();
                ui.painter().rect_filled(rect, 0.0, p.bg.gamma_multiply(0.85));
                ui.painter().rect_stroke(rect.shrink(12.0), 12.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Drop to scan", FontId::new(22.0, theme::bold()), p.accent);
            }
        });
        self.settings_dialog(&ctx);
        self.recycle_dialog(&ctx, p);
        // Drag a file or folder out to Explorer or another app (blocks until it is dropped).
        if let Some(path) = self.drag_request.take() {
            if path.exists() {
                if let Err(e) = drag::start_drag(frame, drag::DragItem::Files(vec![path]), drag::Image::Raw(Vec::new()), |_, _| {}, drag::Options::default()) {
                    log::warn!("Drag failed: {e}");
                }
                self.release_after_drag = true;
                ctx.request_repaint();
            }
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
