//! The sunburst chart: the focused folder in the centre, its contents in concentric rings (outer = deeper),
//! angle proportional to size. Hovering lifts the sector and its subtree outward ("explodes" it) and shows a
//! card with the details; zooming animates every sector from its old place to its new one.

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Vec2, vec2};

use crate::chart::{self, Highlight, Kind, Options, Output, Target};
use crate::theme::{self, Palette};
use crate::tree::{self, NONE, Tree};

const TAU: f32 = std::f32::consts::TAU;
/// Children narrower than this (fraction of the circle, about 0.25°) are merged into one "small items" sector.
const MIN_SPAN: f64 = 1.0 / 1440.0;
const ANIM_SECS: f32 = 0.38;
const EXPLODE_PX: f32 = 7.0;

#[derive(Clone, Debug)]
pub struct Sector {
    pub kind: Kind,
    /// 1 = first ring around the centre.
    pub depth: usize,
    /// Start and end as fractions of the full circle, clockwise from the top.
    pub a0: f32,
    pub a1: f32,
    pub hue: f32,
    pub is_file: bool,
    /// Index of the parent sector, or usize::MAX for the first ring.
    pub parent: usize,
}

impl Sector {
    pub fn node(&self) -> Option<u32> {
        self.kind.node()
    }
    /// Key for matching sectors between the old and new layout during the zoom animation.
    fn key(&self) -> (u8, u32) {
        match self.kind {
            Kind::Node(id) => (0, id),
            Kind::Small { parent, .. } => (1, parent),
            Kind::Free(_) => (2, 0),
        }
    }
}

#[derive(Default)]
pub struct Layout {
    pub focus: u32,
    pub sectors: Vec<Sector>,
    /// Sum shown around the circle (focus size, plus free space if drawn).
    pub total: u64,
}

/// Children of `id` largest first: the tree keeps them sorted once finished, a live scan doesn't.
pub fn sorted_children(tree: &Tree, id: u32, allocated: bool) -> Vec<u32> {
    let mut kids: Vec<u32> = tree.children(id).collect();
    if !tree.finished || allocated {
        kids.sort_by_key(|&c| std::cmp::Reverse(tree.nodes[c as usize].size_of(allocated)));
    }
    kids
}

pub fn layout(tree: &Tree, focus: u32, opts: &Options) -> Layout {
    let focus_size = tree.nodes[focus as usize].size_of(opts.allocated);
    let free = if focus == 0 { opts.free.unwrap_or(0) } else { 0 };
    let total = focus_size + free;
    let mut sectors = Vec::new();
    if total == 0 {
        return Layout { focus, sectors, total };
    }
    // Breadth-first: (node, depth, a0, a1, hue, parent index).
    let mut queue: std::collections::VecDeque<(u32, usize, f64, f64, Option<f32>, usize)> = std::collections::VecDeque::new();
    queue.push_back((focus, 0, 0.0, focus_size as f64 / total as f64, None, usize::MAX));
    while let Some((id, depth, a0, a1, hue, parent_idx)) = queue.pop_front() {
        if depth >= opts.depth {
            continue;
        }
        let size = tree.nodes[id as usize].size_of(opts.allocated);
        if size == 0 {
            continue;
        }
        let scale = (a1 - a0) / size as f64;
        let mut a = a0;
        let mut small = (0u32, 0u64);
        for c in sorted_children(tree, id, opts.allocated) {
            let n = &tree.nodes[c as usize];
            let s = n.size_of(opts.allocated);
            if s == 0 {
                continue;
            }
            let span = s as f64 * scale;
            if span < MIN_SPAN {
                small.0 += 1;
                small.1 += s;
                continue;
            }
            let mid = (a + span / 2.0) as f32;
            // First ring: hue from position around the circle; deeper: the parent's hue, nudged by position.
            let h = match hue {
                None => mid * 0.92 + 0.56,
                Some(ph) => ph + ((a + span / 2.0 - a0) / (a1 - a0) - 0.5) as f32 * 0.12,
            };
            let idx = sectors.len();
            sectors.push(Sector { kind: Kind::Node(c), depth: depth + 1, a0: a as f32, a1: (a + span) as f32, hue: h, is_file: !n.is_dir(), parent: parent_idx });
            if n.is_dir() && n.first_child != NONE {
                queue.push_back((c, depth + 1, a, a + span, Some(h), idx));
            }
            a += span;
        }
        if small.0 > 0 {
            let span = small.1 as f64 * scale;
            sectors.push(Sector {
                kind: Kind::Small { parent: id, count: small.0, bytes: small.1 },
                depth: depth + 1,
                a0: a as f32,
                a1: (a + span).min(a1) as f32,
                hue: hue.unwrap_or(0.0),
                is_file: true,
                parent: parent_idx,
            });
        }
    }
    if free > 0 {
        sectors.push(Sector { kind: Kind::Free(free), depth: 1, a0: (focus_size as f64 / total as f64) as f32, a1: 1.0, hue: 0.0, is_file: false, parent: usize::MAX });
    }
    Layout { focus, sectors, total }
}

/// Ring radii for `depth` rings: a centre disc, then rings getting a little thinner outward.
#[derive(Clone, Copy)]
pub struct Geometry {
    pub center: Pos2,
    pub core: f32,
    pub outer: f32,
    depth: usize,
}

impl Geometry {
    pub fn new(rect: Rect, depth: usize) -> Geometry {
        let outer = (rect.width().min(rect.height()) / 2.0 - 14.0).max(60.0);
        Geometry { center: rect.center(), core: (outer * 0.19).max(44.0), outer, depth: depth.max(1) }
    }
    const RATIO: f32 = 0.88;
    /// Inner radius of ring `d` (1-based); d may be fractional during animation and beyond the last ring.
    pub fn radius(&self, d: f32) -> f32 {
        if d <= 1.0 {
            return self.core * d.max(0.0);
        }
        let n = self.depth as f32;
        let w1 = (self.outer - self.core) * (1.0 - Self::RATIO) / (1.0 - Self::RATIO.powf(n));
        let k = d - 1.0;
        self.core + w1 * (1.0 - Self::RATIO.powf(k)) / (1.0 - Self::RATIO)
    }
    /// Ring (1-based) and angle fraction at a screen position; ring 0 = centre disc.
    pub fn polar(&self, p: Pos2) -> (usize, f32) {
        let v = p - self.center;
        let r = v.length();
        let a = ((v.y.atan2(v.x) + TAU / 4.0) / TAU).rem_euclid(1.0);
        if r < self.core {
            return (0, a);
        }
        for d in 1..=self.depth {
            if r < self.radius(d as f32 + 1.0) {
                return (d, a);
            }
        }
        (usize::MAX, a)
    }
}

/// Animated sector state: start, end and depth.
#[derive(Clone, Copy)]
struct Pose {
    a0: f32,
    a1: f32,
    depth: f32,
    alpha: f32,
}

#[derive(Default)]
pub struct Chart {
    layout: Layout,
    /// What the current layout was built from, to rebuild only when something changed.
    built: Option<(u32, u64, usize, bool, Option<u64>)>,
    from: Vec<Pose>,
    anim_start: Option<Instant>,
}

/// Per-frame inputs from the app that both chart views use.
pub struct Extras<'a> {
    pub ext_rank: &'a [usize],
    pub highlight: Option<&'a Highlight>,
    /// A node to draw attention to (from a list), with its strength 0..1.
    pub pulse: Option<(u32, f32)>,
    /// A node hovered in a side list: lifted as if hovered in the chart.
    pub hovered_from_list: Option<u32>,
    pub now: u64,
}

impl Chart {
    /// Rebuilds the layout if focus, tree generation or options changed; animates focus changes.
    pub fn update(&mut self, tree: &Tree, generation: u64, focus: u32, opts: &Options) {
        let key = (focus, generation, opts.depth, opts.allocated, opts.free);
        if self.built == Some(key) {
            return;
        }
        let animate = self.built.is_some_and(|b| b.1 == generation && b.0 != focus) && !self.layout.sectors.is_empty();
        let new = layout(tree, focus, opts);
        if animate {
            self.from = self.poses_from(&new, tree);
            self.anim_start = Some(Instant::now());
        } else {
            self.anim_start = None;
        }
        self.layout = new;
        self.built = Some(key);
    }

    /// Where each new sector starts its animation: its old place if it was visible, otherwise mapped through
    /// its parent's old place (so zooming in grows the contents out of the clicked sector).
    fn poses_from(&self, new: &Layout, tree: &Tree) -> Vec<Pose> {
        let mut old: HashMap<(u8, u32), Pose> = HashMap::with_capacity(self.layout.sectors.len() + 1);
        for s in &self.layout.sectors {
            old.insert(s.key(), Pose { a0: s.a0, a1: s.a1, depth: s.depth as f32, alpha: 1.0 });
        }
        // The old centre: a full circle at depth 0.
        old.insert((0, self.layout.focus), Pose { a0: 0.0, a1: 1.0, depth: 0.0, alpha: 1.0 });
        let zoom_out = tree.chain(self.layout.focus).contains(&new.focus) && new.focus != self.layout.focus;
        let focus_pose = old.get(&(0, new.focus)).copied().unwrap_or(Pose { a0: 0.0, a1: 1.0, depth: 0.0, alpha: 1.0 });
        let mut out: Vec<Pose> = Vec::with_capacity(new.sectors.len());
        for s in &new.sectors {
            let pose = if let Some(p) = old.get(&s.key()) {
                *p
            } else {
                let (pa0, pa1, pdepth, na0, na1) = if s.parent == usize::MAX {
                    (focus_pose.a0, focus_pose.a1, focus_pose.depth, 0.0, 1.0)
                } else {
                    let p = &new.sectors[s.parent];
                    let pp = out[s.parent];
                    (pp.a0, pp.a1, pp.depth, p.a0, p.a1)
                };
                let span = (na1 - na0).max(1e-9);
                let map = |a: f32| pa0 + (a - na0) / span * (pa1 - pa0);
                Pose { a0: map(s.a0), a1: map(s.a1), depth: pdepth + 1.0, alpha: if zoom_out { 0.0 } else { 1.0 } }
            };
            out.push(pose);
        }
        out
    }

    fn progress(&self) -> Option<f32> {
        let t = self.anim_start?.elapsed().as_secs_f32() / ANIM_SECS;
        (t < 1.0).then(|| 1.0 - (1.0 - t).powi(3))
    }

    /// Paints the chart into `rect` and handles pointer input.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, tree: &Tree, opts: &Options, extras: &Extras) -> (Output, egui::Response) {
        let dark = ui.style().visuals.dark_mode;
        let p = theme::palette(dark);
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let geo = Geometry::new(rect, opts.depth);
        let t = self.progress();
        if t.is_some() {
            ui.ctx().request_repaint();
        }
        let mut out = Output::default();

        // Hit test (not while animating).
        let hover_pos = response.hover_pos().filter(|_| t.is_none());
        let hit = hover_pos.and_then(|pos| self.hit(&geo, pos));
        let hovered_idx = hover_pos.and_then(|pos| self.hit_index(&geo, pos));
        let list_idx = extras.hovered_from_list.and_then(|id| self.layout.sectors.iter().position(|s| s.node() == Some(id)));
        let lifted = hovered_idx.or(list_idx);
        if let Some(Target::Sector(k)) = hit {
            out.hovered = Some(k);
        }
        // Lifted subtree: the hovered sector and everything below it.
        let mut in_lift = vec![false; self.layout.sectors.len()];
        if let Some(h) = lifted {
            for (i, s) in self.layout.sectors.iter().enumerate() {
                in_lift[i] = i == h || (s.parent != usize::MAX && s.parent < i && in_lift[s.parent]);
            }
        }
        let lift_dir = lifted.map(|h| {
            let s = &self.layout.sectors[h];
            let a = ((s.a0 + s.a1) / 2.0) * TAU - TAU / 4.0;
            vec2(a.cos(), a.sin())
        });

        let mut labels = Vec::new();
        for (i, s) in self.layout.sectors.iter().enumerate() {
            let (a0, a1, depth, alpha) = match t {
                Some(t) => {
                    let f = self.from[i];
                    let lerp = |x: f32, y: f32| x + (y - x) * t;
                    (lerp(f.a0, s.a0), lerp(f.a1, s.a1), lerp(f.depth, s.depth as f32), lerp(f.alpha, 1.0))
                }
                None => (s.a0, s.a1, s.depth as f32, 1.0),
            };
            if depth > opts.depth as f32 + 0.999 {
                continue;
            }
            let mut color = chart::color(s.kind, s.hue, s.depth, s.is_file, tree, opts, extras.ext_rank, extras.now, dark);
            if let Some(h) = extras.highlight {
                color = chart::dim(color, h.mark(s.kind), p);
            } else if lifted.is_some() && !in_lift[i] {
                color = chart::mix(color, p.bg, 0.28);
            }
            let offset = if in_lift[i] { lift_dir.unwrap_or_default() * EXPLODE_PX } else { Vec2::ZERO };
            if in_lift[i] {
                color = chart::lighten(color, 0.12);
            }
            let alpha = alpha * (opts.depth as f32 + 1.0 - depth).clamp(0.0, 1.0);
            let ri = geo.radius(depth);
            let ro = geo.radius(depth + 1.0).min(geo.outer + 40.0);
            paint_sector(&painter, geo.center + offset, ri, ro, a0, a1, color.gamma_multiply(alpha));
            if let Some((id, strength)) = extras.pulse {
                if s.node() == Some(id) {
                    ring_outline(&painter, geo.center + offset, ri, ro, a0, a1, Stroke::new(1.0 + 3.0 * strength, p.accent));
                }
            }
            if t.is_none() && matches!(s.kind, Kind::Node(_)) {
                labels.push((i, geo.center + offset, ri, ro, a0, a1, color));
            }
        }
        if let (Some(h), None) = (lifted, t) {
            let s = &self.layout.sectors[h];
            let off = lift_dir.unwrap_or_default() * EXPLODE_PX;
            let d = s.depth as f32;
            ring_outline(&painter, geo.center + off, geo.radius(d), geo.radius(d + 1.0), s.a0, s.a1, Stroke::new(1.5, p.text.gamma_multiply(0.8)));
        }
        for (i, c, ri, ro, a0, a1, color) in labels {
            self.label(&painter, tree, i, c, ri, ro, a0, a1, color);
        }

        self.paint_center(&painter, &geo, tree, opts, p, hit == Some(Target::Center));

        if response.drag_started() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                if let Some(Target::Sector(Kind::Node(id))) = self.hit(&geo, origin) {
                    out.drag_out = Some(id);
                }
            }
        }
        if let Some(target) = hit {
            if response.clicked() {
                out.clicked = Some(target);
            }
            if response.double_clicked() {
                out.double_clicked = Some(target);
            }
            if response.secondary_clicked() {
                out.context = Some(target);
            }
            if let (Some(pos), Some(idx)) = (hover_pos, hovered_idx) {
                let s = &self.layout.sectors[idx];
                chart::tooltip(ui, pos, tree, s.kind, self.layout.focus, self.layout.total, opts, p);
                if s.node().is_some_and(|id| tree.nodes[id as usize].is_dir()) || matches!(s.kind, Kind::Small { .. }) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            } else if hit == Some(Target::Center) && self.layout.focus != 0 {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        (out, response)
    }

    fn hit_index(&self, geo: &Geometry, pos: Pos2) -> Option<usize> {
        let (ring, a) = geo.polar(pos);
        if ring == 0 || ring == usize::MAX {
            return None;
        }
        self.layout.sectors.iter().position(|s| s.depth == ring && a >= s.a0 && a < s.a1)
    }

    fn hit(&self, geo: &Geometry, pos: Pos2) -> Option<Target> {
        let (ring, _) = geo.polar(pos);
        if ring == 0 {
            return Some(Target::Center);
        }
        self.hit_index(geo, pos).map(|i| Target::Sector(self.layout.sectors[i].kind))
    }

    /// Name inside the sector when it fits (horizontal text, elided).
    #[allow(clippy::too_many_arguments)]
    fn label(&self, painter: &egui::Painter, tree: &Tree, i: usize, c: Pos2, ri: f32, ro: f32, a0: f32, a1: f32, color: Color32) {
        let s = &self.layout.sectors[i];
        let Some(id) = s.node() else { return };
        let width = ro - ri;
        let mid_r = (ri + ro) / 2.0;
        let arc = (a1 - a0) * TAU * mid_r;
        if width < 16.0 || arc < 26.0 {
            return;
        }
        let am = ((a0 + a1) / 2.0) * TAU - TAU / 4.0;
        let center = c + vec2(am.cos(), am.sin()) * mid_r;
        // Room for horizontal text: limited by the arc where the sector runs vertically, by the ring where it
        // runs horizontally.
        let horiz = am.sin().abs();
        let room = (arc * horiz + width * (1.0 - horiz)).min(arc).min(150.0) - 6.0;
        if room < 22.0 {
            return;
        }
        let text_color = chart::text_on(color);
        let font = FontId::proportional(if s.depth == 1 { 12.0 } else { 11.0 });
        let Some(galley) = fit_text(painter, tree.name(id), font, text_color, room) else { return };
        if galley.size().y > width.max(arc) {
            return;
        }
        painter.galley(center - galley.size() / 2.0, galley, text_color);
    }

    fn paint_center(&self, painter: &egui::Painter, geo: &Geometry, tree: &Tree, opts: &Options, p: &Palette, hovered: bool) {
        let fill = if hovered && self.layout.focus != 0 { chart::mix(p.card, p.accent, 0.12) } else { p.card };
        painter.circle(geo.center, geo.core - 2.0, fill, Stroke::new(1.0, p.border));
        let focus = self.layout.focus;
        let n = &tree.nodes[focus as usize];
        let c = geo.center;
        let font = FontId::new(14.0, theme::bold());
        if let Some(g) = fit_text(painter, &display_name(tree, focus), font, p.text, geo.core * 1.7) {
            painter.galley(c + vec2(-g.size().x / 2.0, -16.0 - g.size().y / 2.0), g, p.text);
        }
        painter.text(c + vec2(0.0, 4.0), Align2::CENTER_CENTER, tree::format_size(n.size_of(opts.allocated)), FontId::new(17.0, theme::bold()), p.accent);
        let sub = format!("{} files", tree::format_count(n.files as u64));
        painter.text(c + vec2(0.0, 23.0), Align2::CENTER_CENTER, sub, FontId::proportional(11.0), p.muted);
        if hovered && focus != 0 {
            painter.text(c + vec2(0.0, 38.0), Align2::CENTER_CENTER, "▲ up", FontId::proportional(10.0), p.muted);
        }
    }
}

/// A folder scan's root shows its own name; a drive shows "C:\".
pub fn display_name(tree: &Tree, id: u32) -> String {
    if id != 0 {
        tree.name(id).to_string()
    } else if tree.root_path.len() <= 3 {
        tree.root_path.clone()
    } else {
        tree.root_path.rsplit('\\').next().unwrap_or(&tree.root_path).to_string()
    }
}

/// The text laid out in one line, shortened with "…" to fit `room`; None if not even a few letters fit.
pub fn fit_text(painter: &egui::Painter, text: &str, font: FontId, color: Color32, room: f32) -> Option<std::sync::Arc<egui::Galley>> {
    let g = painter.layout_no_wrap(text.to_string(), font.clone(), color);
    if g.size().x <= room {
        return Some(g);
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (1usize, chars.len());
    let mut best = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let s: String = chars[..mid].iter().collect::<String>() + "…";
        let g = painter.layout_no_wrap(s, font.clone(), color);
        if g.size().x <= room {
            best = Some(g);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    best.filter(|_| lo > 2)
}

fn angle(f: f32) -> f32 {
    f * TAU - TAU / 4.0
}

/// One annular sector as a triangle strip, with a hairline gap to its neighbours (constant in pixels) and a
/// slight gradient: darker inside, lighter outside.
fn paint_sector(painter: &egui::Painter, c: Pos2, ri: f32, ro: f32, a0: f32, a1: f32, color: Color32) {
    let span = a1 - a0;
    if span <= 0.0 || ro <= ri {
        return;
    }
    let gap = 0.6;
    let (ri_g, ro_g) = (if ri > 1.0 { ri + gap } else { ri }, ro - gap);
    if ro_g <= ri_g {
        return;
    }
    let steps = ((span * TAU * ro / 3.0).ceil() as usize).clamp(1, 512);
    // Angular gap at each radius, skipped for slivers that would vanish.
    let ga = |r: f32| if span * TAU * r > 4.0 * gap { gap / (r * TAU) } else { 0.0 };
    let (gi, go) = (ga(ri_g.max(1.0)), ga(ro_g));
    let inner = chart::mix(color, Color32::BLACK, 0.06);
    let outer = chart::mix(color, Color32::WHITE, 0.05);
    let mut mesh = Mesh::default();
    mesh.reserve_vertices((steps + 1) * 2);
    mesh.reserve_triangles(steps * 2);
    for k in 0..=steps {
        let f = k as f32 / steps as f32;
        let ai = angle(a0 + gi + (span - 2.0 * gi).max(0.0) * f);
        let ao = angle(a0 + go + (span - 2.0 * go).max(0.0) * f);
        mesh.colored_vertex(c + vec2(ai.cos(), ai.sin()) * ri_g, inner);
        mesh.colored_vertex(c + vec2(ao.cos(), ao.sin()) * ro_g, outer);
        if k > 0 {
            let b = (k as u32) * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    painter.add(Shape::mesh(mesh));
}

fn ring_outline(painter: &egui::Painter, c: Pos2, ri: f32, ro: f32, a0: f32, a1: f32, stroke: Stroke) {
    let span = a1 - a0;
    let steps = ((span * TAU * ro / 4.0).ceil() as usize).clamp(2, 512);
    let mut pts = Vec::with_capacity(steps * 2 + 3);
    for k in 0..=steps {
        let a = angle(a0 + span * k as f32 / steps as f32);
        pts.push(c + vec2(a.cos(), a.sin()) * ro);
    }
    if ri > 1.0 {
        for k in (0..=steps).rev() {
            let a = angle(a0 + span * k as f32 / steps as f32);
            pts.push(c + vec2(a.cos(), a.sin()) * ri);
        }
    } else {
        pts.push(c);
    }
    painter.add(Shape::closed_line(pts, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ColorMode;

    fn sample() -> Tree {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "a", true, 0, 0, 0);
        t.add(a, "big", false, 6000, 0, 0);
        for i in 0..50 {
            t.add(a, &format!("tiny{i}"), false, 1, 0, 0);
        }
        let b = t.add(0, "b", true, 0, 0, 0);
        let c = t.add(b, "c", true, 0, 0, 0);
        t.add(c, "deep", false, 3950, 0, 0);
        t.finish();
        t
    }

    fn opts(depth: usize) -> Options {
        Options { depth, allocated: false, color: ColorMode::Folder, free: None }
    }

    #[test]
    fn ring_one_covers_the_circle() {
        let t = sample();
        let l = layout(&t, 0, &opts(7));
        let ring1: Vec<&Sector> = l.sectors.iter().filter(|s| s.depth == 1).collect();
        assert!((ring1.iter().map(|s| s.a1 - s.a0).sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(ring1[0].a0, 0.0);
        // The 50 one-byte files in "a" are merged into one sector.
        let small: Vec<&Sector> = l.sectors.iter().filter(|s| matches!(s.kind, Kind::Small { .. })).collect();
        assert_eq!(small.len(), 1);
        assert!(matches!(small[0].kind, Kind::Small { count: 50, bytes: 50, .. }));
        // Children lie within their parent.
        for s in &l.sectors {
            if s.parent != usize::MAX {
                let p = &l.sectors[s.parent];
                assert!(s.a0 >= p.a0 - 1e-6 && s.a1 <= p.a1 + 1e-6 && s.depth == p.depth + 1);
            }
        }
    }

    #[test]
    fn unfinished_tree_is_sorted_on_the_fly() {
        let mut t = Tree::new("C:\\");
        t.add(0, "small", false, 10, 0, 0);
        t.add(0, "big", false, 90, 0, 0);
        t.nodes[0].size = 100; // a live scan keeps running totals
        let l = layout(&t, 0, &opts(3));
        assert_eq!(t.name(l.sectors[0].node().unwrap()), "big");
    }

    #[test]
    fn depth_limit_and_free_space() {
        let t = sample();
        let l = layout(&t, 0, &opts(2));
        assert!(l.sectors.iter().all(|s| s.depth <= 2));
        let l = layout(&t, 0, &Options { free: Some(10000), ..opts(3) });
        let free = l.sectors.iter().find(|s| matches!(s.kind, Kind::Free(_))).unwrap();
        assert!((free.a1 - free.a0 - 0.5).abs() < 1e-3);
    }

    #[test]
    fn hit_testing() {
        let t = sample();
        let mut chart = Chart::default();
        chart.update(&t, 1, 0, &opts(4));
        let geo = Geometry::new(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 600.0)), 4);
        assert_eq!(chart.hit(&geo, geo.center), Some(Target::Center));
        // Just right of the top in ring 1: the largest child ("a", 6050 of 10000 bytes) starts at the top.
        let r = (geo.radius(1.0) + geo.radius(2.0)) / 2.0;
        let a = t.find("C:\\a").unwrap();
        assert_eq!(chart.hit(&geo, geo.center + vec2(5.0, -r)), Some(Target::Sector(Kind::Node(a))));
        // Just left of the top: the last child ("b").
        let b = t.find("C:\\b").unwrap();
        assert_eq!(chart.hit(&geo, geo.center + vec2(-5.0, -r)), Some(Target::Sector(Kind::Node(b))));
        assert_eq!(chart.hit(&geo, geo.center + vec2(0.0, -geo.outer - 20.0)), None);
    }

    #[test]
    fn radii_increase_and_fill() {
        let geo = Geometry::new(Rect::from_min_size(Pos2::ZERO, vec2(500.0, 500.0)), 6);
        assert!((geo.radius(7.0) - geo.outer).abs() < 0.01);
        assert!(geo.radius(1.0) == geo.core);
        for d in 1..7 {
            assert!(geo.radius(d as f32 + 1.0) > geo.radius(d as f32));
        }
    }
}
