//! What the sunburst and the treemap share: what a chart element stands for, view options, colours (by
//! folder, file type or age), highlighting (search, file type, cleanup, duplicates dim everything else), the
//! hover card and the age legend.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, pos2, vec2};

use crate::theme::{self, Palette};
use crate::tree::{self, NONE, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Node(u32),
    /// Children of `parent` too small to draw, merged.
    Small { parent: u32, count: u32, bytes: u64 },
    Free(u64),
}

impl Kind {
    pub fn node(self) -> Option<u32> {
        match self {
            Kind::Node(id) => Some(id),
            _ => None,
        }
    }
}

/// What the user clicked or right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Center,
    Sector(Kind),
}

#[derive(Default)]
pub struct Output {
    pub clicked: Option<Target>,
    pub double_clicked: Option<Target>,
    pub hovered: Option<Kind>,
    /// Right-clicked: the app shows the context menu for it.
    pub context: Option<Target>,
    /// A press on this element moved far enough to start dragging it out of the window.
    pub drag_out: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    Folder,
    Type,
    Age,
}

impl ColorMode {
    pub fn from_config(s: &str) -> ColorMode {
        match s {
            "type" => ColorMode::Type,
            "age" => ColorMode::Age,
            _ => ColorMode::Folder,
        }
    }
    pub fn config(self) -> &'static str {
        match self {
            ColorMode::Folder => "folder",
            ColorMode::Type => "type",
            ColorMode::Age => "age",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Options {
    pub depth: usize,
    pub allocated: bool,
    pub color: ColorMode,
    /// Free bytes of the drive, drawn as a sector when the focus is the drive root.
    pub free: Option<u64>,
}

/// Per-node marks for highlighting: 2 = match, 1 = folder containing a match, 0 = other (dimmed).
#[derive(Clone, Default)]
pub struct Highlight {
    marks: Vec<u8>,
}

impl Highlight {
    pub fn from_nodes(tree: &Tree, ids: impl IntoIterator<Item = u32>) -> Highlight {
        let mut marks = vec![0u8; tree.len()];
        for id in ids {
            if (id as usize) >= marks.len() {
                continue;
            }
            marks[id as usize] = 2;
            let mut a = tree.nodes[id as usize].parent;
            while a != NONE && marks[a as usize] == 0 {
                marks[a as usize] = 1;
                a = tree.nodes[a as usize].parent;
            }
        }
        Highlight { marks }
    }

    /// All files with this extension.
    pub fn by_ext(tree: &Tree, ext: u16) -> Highlight {
        let ids = (0..tree.len() as u32).filter(|&i| {
            let n = &tree.nodes[i as usize];
            !n.is_dir() && n.ext == ext && n.flags & tree::GONE == 0
        });
        Highlight::from_nodes(tree, ids)
    }

    pub fn mark(&self, kind: Kind) -> u8 {
        match kind {
            Kind::Node(id) => self.marks.get(id as usize).copied().unwrap_or(0),
            Kind::Small { parent, .. } => self.marks.get(parent as usize).map_or(0, |&m| m.min(1)),
            Kind::Free(_) => 0,
        }
    }
}

/// Current time as FILETIME, for the age colours.
pub fn now_filetime() -> u64 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (secs + 11_644_473_600) * 10_000_000
}

/// Age bucket edges in days and their labels, for the age colours and the legend.
pub const AGE_BUCKETS: [(f32, &str); 6] = [(7.0, "1 week"), (31.0, "1 month"), (183.0, "6 months"), (365.0, "1 year"), (1095.0, "3 years"), (f32::MAX, "older")];

pub fn age_bucket(filetime: u64, now: u64) -> usize {
    if filetime == 0 {
        return AGE_BUCKETS.len() - 1;
    }
    let days = now.saturating_sub(filetime) as f32 / 864_000_000_000.0;
    AGE_BUCKETS.iter().position(|(d, _)| days <= *d).unwrap_or(AGE_BUCKETS.len() - 1)
}

/// Colour of a chart element. `hue` and `depth` come from the layout (branch colour and ring/level).
#[allow(clippy::too_many_arguments)]
pub fn color(kind: Kind, hue: f32, depth: usize, is_file: bool, tree: &Tree, opts: &Options, ext_rank: &[usize], now: u64, dark: bool) -> Color32 {
    let p = theme::palette(dark);
    match kind {
        Kind::Free(_) => p.free,
        // Merged small items: the branch colour, faded, so they don't read as free space.
        Kind::Small { .. } => match opts.color {
            ColorMode::Folder => mix(theme::sector_color(hue, depth, true, dark), p.bg, 0.55),
            _ => p.small,
        },
        Kind::Node(id) => {
            let n = &tree.nodes[id as usize];
            match opts.color {
                ColorMode::Folder => theme::sector_color(hue, depth, is_file, dark),
                ColorMode::Type if is_file => theme::type_color(ext_rank.get(n.ext as usize).copied().unwrap_or(usize::MAX), dark),
                ColorMode::Type => theme::hsl(hue, 0.10, if dark { 0.36 + depth as f32 * 0.025 } else { 0.72 + depth as f32 * 0.02 }),
                ColorMode::Age => {
                    let c = theme::age_color(age_bucket(n.newest, now), dark);
                    if is_file { c } else { mix(c, p.bg, 0.12) }
                }
            }
        }
    }
}

/// Dims an element's colour by its highlight mark.
pub fn dim(color: Color32, mark: u8, p: &Palette) -> Color32 {
    match mark {
        2 => color,
        1 => mix(color, p.bg, 0.45),
        _ => mix(color, p.bg, 0.78),
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), a.a())
}

pub fn lighten(c: Color32, t: f32) -> Color32 {
    mix(c, Color32::WHITE, t)
}

pub fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

pub fn text_on(c: Color32) -> Color32 {
    if luminance(c) > 0.5 { Color32::from_black_alpha(220) } else { Color32::from_white_alpha(235) }
}

pub fn percent(part: u64, whole: u64) -> String {
    let v = part as f64 * 100.0 / whole.max(1) as f64;
    if v >= 10.0 { format!("{v:.0} %") } else if v >= 0.1 { format!("{v:.1} %") } else { "< 0.1 %".into() }
}

/// The card shown next to the pointer for a hovered element.
pub fn tooltip(ui: &egui::Ui, pos: Pos2, tree: &Tree, kind: Kind, focus: u32, view_total: u64, opts: &Options, p: &Palette) {
    let view_total = view_total.max(1);
    let mut rows: Vec<(String, String)> = Vec::new();
    let (title, size, path) = match kind {
        Kind::Node(id) => {
            let n = &tree.nodes[id as usize];
            let size = n.size_of(opts.allocated);
            if n.parent != NONE {
                let parent_size = tree.nodes[n.parent as usize].size_of(opts.allocated).max(1);
                rows.push(("of parent".into(), percent(size, parent_size)));
            }
            if focus != n.parent {
                rows.push(("of view".into(), percent(size, view_total)));
            }
            if n.flags & tree::SKIPPED != 0 {
                rows.push(("skipped".into(), "excluded from scans (Settings)".into()));
            } else if n.is_dir() {
                rows.push(("contents".into(), format!("{} files, {} folders", tree::format_count(n.files as u64), tree::format_count(n.dirs as u64))));
                let t = tree::format_time(n.newest);
                if !t.is_empty() {
                    rows.push(("newest file".into(), t));
                }
            } else {
                let other = if opts.allocated { ("length", n.size) } else { ("on disk", n.alloc) };
                rows.push((other.0.into(), tree::format_size(other.1)));
                let ext = tree.ext_name(id);
                if !ext.is_empty() {
                    rows.push(("type".into(), format!(".{ext}")));
                }
                let t = tree::format_time(n.mtime);
                if !t.is_empty() {
                    rows.push(("modified".into(), t));
                }
                if n.flags & tree::CLOUD != 0 {
                    rows.push(("cloud".into(), "online-only placeholder".into()));
                }
            }
            (if id == 0 { tree.root_path.clone() } else { tree.name(id).to_string() }, size, Some(tree.path(id)))
        }
        Kind::Small { parent, count, bytes } => {
            rows.push(("of view".into(), percent(bytes, view_total)));
            (format!("{} small items", tree::format_count(count as u64)), bytes, Some(tree.path(parent)))
        }
        Kind::Free(free) => {
            rows.push(("of drive".into(), percent(free, view_total)));
            ("Free space".to_string(), free, None)
        }
    };
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("chart-tip"));
    let painter = ui.ctx().layer_painter(layer);
    let title_g = painter.layout_no_wrap(title, FontId::new(13.0, theme::bold()), p.text);
    let size_g = painter.layout_no_wrap(tree::format_size(size), FontId::new(20.0, theme::bold()), p.accent);
    let row_gs: Vec<_> = rows
        .into_iter()
        .map(|(k, v)| (painter.layout_no_wrap(k, FontId::proportional(11.0), p.muted), painter.layout_no_wrap(v, FontId::proportional(12.0), p.text)))
        .collect();
    let path_g = path.map(|t| painter.layout(t, FontId::proportional(10.5), p.muted, 320.0));
    let key_w = row_gs.iter().map(|(k, _)| k.size().x).fold(0.0, f32::max);
    let mut w = title_g.size().x.max(size_g.size().x);
    for (_, v) in &row_gs {
        w = w.max(key_w + 10.0 + v.size().x);
    }
    if let Some(g) = &path_g {
        w = w.max(g.size().x);
    }
    let w = w.min(340.0) + 24.0;
    let h = 12.0
        + title_g.size().y
        + 2.0
        + size_g.size().y
        + 6.0
        + row_gs.iter().map(|(_, v)| v.size().y + 2.0).sum::<f32>()
        + path_g.as_ref().map_or(0.0, |g| g.size().y + 6.0)
        + 12.0;
    let screen = ui.ctx().content_rect();
    let mut origin = pos + vec2(18.0, 18.0);
    if origin.x + w > screen.right() - 4.0 {
        origin.x = pos.x - w - 14.0;
    }
    if origin.y + h > screen.bottom() - 4.0 {
        origin.y = pos.y - h - 14.0;
    }
    let card = Rect::from_min_size(origin, vec2(w, h));
    let shadow = egui::epaint::Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(if ui.style().visuals.dark_mode { 110 } else { 45 }) };
    painter.add(shadow.as_shape(card, 8.0));
    painter.rect(card, 8.0, p.card, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    let mut y = card.top() + 12.0;
    let x = card.left() + 12.0;
    let th = title_g.size().y;
    painter.galley(pos2(x, y), title_g, p.text);
    y += th + 2.0;
    let sh = size_g.size().y;
    painter.galley(pos2(x, y), size_g, p.accent);
    y += sh + 6.0;
    for (k, v) in row_gs {
        let vh = v.size().y;
        painter.galley(pos2(x, y + (vh - k.size().y) / 2.0), k, p.muted);
        painter.galley(pos2(x + key_w + 10.0, y), v, p.text);
        y += vh + 2.0;
    }
    if let Some(g) = path_g {
        y += 4.0;
        painter.line_segment([pos2(x, y - 2.0), pos2(card.right() - 12.0, y - 2.0)], Stroke::new(1.0, p.border));
        painter.galley(pos2(x, y + 2.0), g, p.muted);
    }
}

/// Colour key for the age mode, in the bottom-left corner of the chart area.
pub fn age_legend(painter: &egui::Painter, rect: Rect, dark: bool) {
    let p = theme::palette(dark);
    let mut y = rect.bottom() - 14.0 - AGE_BUCKETS.len() as f32 * 18.0;
    let x = rect.left() + 14.0;
    // On a card, so it stays readable over the treemap.
    let card = Rect::from_min_max(pos2(x - 8.0, y - 22.0), pos2(x + 104.0, rect.bottom() - 6.0));
    painter.rect(card, 8.0, p.card.gamma_multiply(0.94), Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    painter.text(pos2(x, y - 4.0), egui::Align2::LEFT_BOTTOM, "Newest file", FontId::new(11.0, theme::bold()), p.muted);
    for (i, (_, label)) in AGE_BUCKETS.iter().enumerate() {
        painter.rect_filled(Rect::from_min_size(pos2(x, y + 3.0), vec2(12.0, 12.0)), 3.0, theme::age_color(i, dark));
        let text = if i + 1 == AGE_BUCKETS.len() { "older".to_string() } else { format!("≤ {label}") };
        painter.text(pos2(x + 18.0, y + 9.0), egui::Align2::LEFT_CENTER, text, FontId::proportional(11.0), p.text);
        y += 18.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_marks_ancestors() {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "a", true, 0, 0, 0);
        let x = t.add(a, "x.iso", false, 10, 10, 0);
        let y = t.add(0, "y.txt", false, 10, 10, 0);
        t.finish();
        let h = Highlight::by_ext(&t, t.nodes[x as usize].ext);
        assert_eq!(h.mark(Kind::Node(x)), 2);
        assert_eq!(h.mark(Kind::Node(a)), 1);
        assert_eq!(h.mark(Kind::Node(0)), 1);
        assert_eq!(h.mark(Kind::Node(y)), 0);
    }

    #[test]
    fn age_buckets() {
        let now = now_filetime();
        let day = 864_000_000_000u64;
        assert_eq!(age_bucket(now - day, now), 0);
        assert_eq!(age_bucket(now - 20 * day, now), 1);
        assert_eq!(age_bucket(now - 400 * day, now), 4);
        assert_eq!(age_bucket(now - 4000 * day, now), 5);
        assert_eq!(age_bucket(0, now), 5);
    }
}
