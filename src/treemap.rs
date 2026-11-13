//! Treemap view: the focused folder as nested rectangles, area proportional to size (squarified layout, so
//! cells stay close to square). Folders get a title strip when there is room; colours, hover card,
//! highlighting and the context menu are shared with the sunburst (`chart.rs`).

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

use crate::chart::{self, Kind, Options, Output, Target};
use crate::sunburst::{Extras, fit_text, sorted_children};
use crate::theme;
use crate::tree::{NONE, Tree};

const ANIM_SECS: f32 = 0.32;
const TITLE_H: f32 = 17.0;
const PAD: f32 = 2.0;

#[derive(Clone, Debug)]
pub struct Cell {
    pub kind: Kind,
    pub rect: Rect,
    /// 1 = a child of the focus.
    pub depth: usize,
    pub hue: f32,
    pub is_file: bool,
    pub parent: usize,
}

impl Cell {
    fn key(&self) -> (u8, u32) {
        match self.kind {
            Kind::Node(id) => (0, id),
            Kind::Small { parent, .. } => (1, parent),
            Kind::Free(_) => (2, 0),
        }
    }
}

/// Squarified treemap (Bruls, Huizing, van Wijk): `sizes` largest first, returns one rectangle per size.
pub fn squarify(sizes: &[f64], rect: Rect) -> Vec<Rect> {
    let total: f64 = sizes.iter().sum();
    let mut out = Vec::with_capacity(sizes.len());
    if total <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return vec![Rect::NOTHING; sizes.len()];
    }
    let scale = (rect.width() as f64 * rect.height() as f64) / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    let mut free = rect;
    let mut i = 0;
    while i < areas.len() {
        let short = free.width().min(free.height()) as f64;
        // Grow the row while the worst aspect ratio improves.
        let worst = |row: &[f64]| {
            let sum: f64 = row.iter().sum();
            let (max, min) = row.iter().fold((0.0f64, f64::MAX), |(mx, mn), &a| (mx.max(a), mn.min(a)));
            let s2 = sum * sum;
            ((short * short * max) / s2).max(s2 / (short * short * min.max(1e-12)))
        };
        let mut j = i + 1;
        while j < areas.len() && worst(&areas[i..=j]) <= worst(&areas[i..j]) {
            j += 1;
        }
        let row = &areas[i..j];
        let sum: f64 = row.iter().sum();
        let horizontal = free.width() >= free.height();
        // The row is a strip along the short side.
        let thickness = (sum / short) as f32;
        let mut pos = 0.0f32;
        for &a in row {
            let len = (a / sum * short) as f32;
            let r = if horizontal {
                Rect::from_min_size(pos2(free.left(), free.top() + pos), vec2(thickness, len))
            } else {
                Rect::from_min_size(pos2(free.left() + pos, free.top()), vec2(len, thickness))
            };
            out.push(r);
            pos += len;
        }
        free = if horizontal { Rect::from_min_max(pos2(free.left() + thickness, free.top()), free.max) } else { Rect::from_min_max(pos2(free.left(), free.top() + thickness), free.max) };
        i = j;
    }
    out
}

pub fn layout(tree: &Tree, focus: u32, rect: Rect, opts: &Options) -> Vec<Cell> {
    let mut cells = Vec::new();
    // (node, rect for its children, depth of the children, hue, parent cell index)
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((focus, rect, 1usize, None::<f32>, usize::MAX));
    while let Some((id, area, depth, hue, parent)) = queue.pop_front() {
        if depth > opts.depth || area.width() < 4.0 || area.height() < 4.0 {
            continue;
        }
        let total = tree.nodes[id as usize].size_of(opts.allocated) as f64;
        if total <= 0.0 {
            continue;
        }
        // Children too small for ~4 px² each are merged into one cell.
        let min = total * 6.0 / (area.width() * area.height()) as f64;
        let mut items: Vec<(Kind, f64)> = Vec::new();
        let mut small = (0u32, 0u64);
        for c in sorted_children(tree, id, opts.allocated) {
            let s = tree.nodes[c as usize].size_of(opts.allocated);
            if s == 0 {
                continue;
            }
            if (s as f64) < min {
                small.0 += 1;
                small.1 += s;
            } else {
                items.push((Kind::Node(c), s as f64));
            }
        }
        if small.0 > 0 {
            items.push((Kind::Small { parent: id, count: small.0, bytes: small.1 }, small.1 as f64));
        }
        let sizes: Vec<f64> = items.iter().map(|i| i.1).collect();
        let rects = squarify(&sizes, area);
        let mut acc = 0.0;
        for ((kind, size), r) in items.into_iter().zip(rects) {
            let frac = (acc + size / 2.0) / total;
            acc += size;
            let h = match hue {
                None => frac as f32 * 0.92 + 0.56,
                Some(ph) => ph + (frac as f32 - 0.5) * 0.12,
            };
            let (is_file, is_dir_with_kids) = match kind {
                Kind::Node(c) => {
                    let n = &tree.nodes[c as usize];
                    (!n.is_dir(), n.is_dir() && n.first_child != NONE)
                }
                _ => (true, false),
            };
            let idx = cells.len();
            cells.push(Cell { kind, rect: r, depth, hue: h, is_file, parent });
            if is_dir_with_kids {
                let title = if r.width() > 60.0 && r.height() > 34.0 { TITLE_H } else { 0.0 };
                let inner = Rect::from_min_max(pos2(r.left() + PAD, r.top() + PAD + title), pos2(r.right() - PAD, r.bottom() - PAD));
                if let Kind::Node(c) = kind {
                    queue.push_back((c, inner, depth + 1, Some(h), idx));
                }
            }
        }
    }
    cells
}

#[derive(Default)]
pub struct Treemap {
    cells: Vec<Cell>,
    focus: u32,
    built: Option<(u32, u64, usize, bool, [i32; 4])>,
    from: Vec<(Rect, f32)>,
    anim_start: Option<Instant>,
}

impl Treemap {
    pub fn update(&mut self, tree: &Tree, generation: u64, focus: u32, rect: Rect, opts: &Options) {
        let r = [rect.left() as i32, rect.top() as i32, rect.width() as i32, rect.height() as i32];
        let key = (focus, generation, opts.depth, opts.allocated, r);
        if self.built == Some(key) {
            return;
        }
        let animate = self.built.is_some_and(|b| b.1 == generation && b.0 != focus && b.4 == r) && !self.cells.is_empty();
        let new = layout(tree, focus, rect, opts);
        if animate {
            let mut old: HashMap<(u8, u32), Rect> = self.cells.iter().map(|c| (c.key(), c.rect)).collect();
            old.insert((0, self.focus), rect);
            let zoom_out = tree.chain(self.focus).contains(&focus);
            let focus_rect = old.get(&(0, focus)).copied().unwrap_or(rect);
            let mut from: Vec<(Rect, f32)> = Vec::with_capacity(new.len());
            for c in &new {
                let f = if let Some(r) = old.get(&c.key()) {
                    (*r, 1.0)
                } else {
                    // Map through the parent's old rectangle.
                    let (pold, pnew) = if c.parent == usize::MAX { (focus_rect, rect) } else { (from[c.parent].0, new[c.parent].rect) };
                    let map = |p: Pos2| pos2(pold.left() + (p.x - pnew.left()) / pnew.width().max(1.0) * pold.width(), pold.top() + (p.y - pnew.top()) / pnew.height().max(1.0) * pold.height());
                    (Rect::from_min_max(map(c.rect.min), map(c.rect.max)), if zoom_out { 0.0 } else { 1.0 })
                };
                from.push(f);
            }
            self.from = from;
            self.anim_start = Some(Instant::now());
        } else {
            self.anim_start = None;
        }
        self.cells = new;
        self.focus = focus;
        self.built = Some(key);
    }

    fn progress(&self) -> Option<f32> {
        let t = self.anim_start?.elapsed().as_secs_f32() / ANIM_SECS;
        (t < 1.0).then(|| 1.0 - (1.0 - t).powi(3))
    }

    fn hit(&self, pos: Pos2) -> Option<usize> {
        // Children come after their parents: the last hit is the deepest.
        self.cells.iter().rposition(|c| c.rect.contains(pos))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, tree: &Tree, opts: &Options, extras: &Extras) -> (Output, egui::Response) {
        let dark = ui.style().visuals.dark_mode;
        let p = theme::palette(dark);
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let t = self.progress();
        if t.is_some() {
            ui.ctx().request_repaint();
        }
        let mut out = Output::default();
        let hover_pos = response.hover_pos().filter(|_| t.is_none());
        let hovered = hover_pos.and_then(|pos| self.hit(pos));
        let listed = extras.hovered_from_list.and_then(|id| self.cells.iter().position(|c| c.kind == Kind::Node(id)));
        let lifted = hovered.or(listed);
        let view_total = tree.nodes[self.focus as usize].size_of(opts.allocated);

        for (i, c) in self.cells.iter().enumerate() {
            let (r, alpha) = match t {
                Some(t) => {
                    let (fr, fa) = self.from[i];
                    let lerp = |a: f32, b: f32| a + (b - a) * t;
                    (Rect::from_min_max(pos2(lerp(fr.left(), c.rect.left()), lerp(fr.top(), c.rect.top())), pos2(lerp(fr.right(), c.rect.right()), lerp(fr.bottom(), c.rect.bottom()))), lerp(fa, 1.0))
                }
                None => (c.rect, 1.0),
            };
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            let mut color = chart::color(c.kind, c.hue, c.depth, c.is_file, tree, opts, extras.ext_rank, extras.now, dark);
            if let Some(h) = extras.highlight {
                color = chart::dim(color, h.mark(c.kind), p);
            }
            if lifted == Some(i) {
                color = chart::lighten(color, 0.15);
            }
            let color = color.gamma_multiply(alpha);
            let inner = r.shrink(0.5);
            cushion(&painter, inner, color);
            if let Some((id, s)) = extras.pulse {
                if c.kind == Kind::Node(id) {
                    painter.rect_stroke(inner, 2.0, Stroke::new(1.0 + 3.0 * s, p.accent), egui::StrokeKind::Inside);
                }
            }
            if t.is_none() {
                self.label(&painter, tree, c, inner, color);
            }
        }
        if let (Some(i), None) = (lifted, t) {
            painter.rect_stroke(self.cells[i].rect.shrink(0.5), 2.0, Stroke::new(2.0, p.text.gamma_multiply(0.85)), egui::StrokeKind::Inside);
        }

        if response.drag_started() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                if let Some(Kind::Node(id)) = self.hit(origin).map(|i| self.cells[i].kind) {
                    out.drag_out = Some(id);
                }
            }
        }
        if let (Some(i), Some(pos)) = (hovered, hover_pos) {
            let kind = self.cells[i].kind;
            let target = Target::Sector(kind);
            out.hovered = Some(kind);
            if response.clicked() {
                out.clicked = Some(target);
            }
            if response.double_clicked() {
                out.double_clicked = Some(target);
            }
            if response.secondary_clicked() {
                out.context = Some(target);
            }
            chart::tooltip(ui, pos, tree, kind, self.focus, view_total, opts, p);
            if kind.node().is_some_and(|id| tree.nodes[id as usize].is_dir()) || matches!(kind, Kind::Small { .. }) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        } else if response.secondary_clicked() {
            out.context = Some(Target::Center);
        }
        (out, response)
    }

    fn label(&self, painter: &egui::Painter, tree: &Tree, c: &Cell, r: Rect, color: Color32) {
        let Kind::Node(id) = c.kind else { return };
        let n = &tree.nodes[id as usize];
        let text = chart::text_on(color);
        if n.is_dir() && n.first_child != NONE {
            if r.width() > 60.0 && r.height() > 34.0 {
                let strip = Rect::from_min_size(r.min, vec2(r.width(), TITLE_H));
                painter.rect_filled(strip, 2.0, chart::mix(color, Color32::BLACK, 0.18));
                let tc = chart::text_on(chart::mix(color, Color32::BLACK, 0.18));
                if let Some(g) = fit_text(painter, tree.name(id), FontId::new(11.0, theme::bold()), tc, r.width() - 8.0) {
                    painter.galley(pos2(r.left() + 4.0, strip.center().y - g.size().y / 2.0), g, tc);
                }
            }
            return;
        }
        if r.width() < 40.0 || r.height() < 16.0 {
            return;
        }
        if let Some(g) = fit_text(painter, tree.name(id), FontId::proportional(11.0), text, r.width() - 8.0) {
            let gh = g.size().y;
            painter.galley(pos2(r.left() + 4.0, r.top() + 3.0), g, text);
            if r.height() > gh * 2.0 + 8.0 {
                let size = crate::tree::format_size(n.size);
                painter.text(pos2(r.left() + 4.0, r.top() + 5.0 + gh), egui::Align2::LEFT_TOP, size, FontId::proportional(10.0), text.gamma_multiply(0.75));
            }
        }
    }
}

/// A cell with a soft vertical gradient (lighter at the top), like a flat cushion.
fn cushion(painter: &egui::Painter, r: Rect, color: Color32) {
    let top = chart::mix(color, Color32::WHITE, 0.10);
    let bottom = chart::mix(color, Color32::BLACK, 0.08);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(r.left_top(), top);
    mesh.colored_vertex(r.right_top(), top);
    mesh.colored_vertex(r.right_bottom(), bottom);
    mesh.colored_vertex(r.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ColorMode;

    #[test]
    fn squarify_tiles_the_area() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 400.0));
        let sizes = [500.0, 300.0, 120.0, 50.0, 20.0, 9.0, 1.0];
        let rects = squarify(&sizes, rect);
        let total: f32 = rects.iter().map(|r| r.area()).sum();
        assert!((total - rect.area()).abs() < 1.0, "{total}");
        for (r, s) in rects.iter().zip(sizes) {
            assert!(rect.expand(0.01).contains_rect(*r));
            assert!((r.area() / rect.area() - s as f32 / 1000.0).abs() < 1e-3);
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(a.shrink(0.01).intersect(b.shrink(0.01)).area() <= 0.0 || !a.shrink(0.01).intersects(b.shrink(0.01)));
            }
        }
    }

    #[test]
    fn nested_cells_stay_inside_parents() {
        let mut t = Tree::new("C:\\");
        let a = t.add(0, "a", true, 0, 0, 0);
        for i in 0..20 {
            t.add(a, &format!("f{i}"), false, 100 + i * 10, 0, 0);
        }
        t.add(0, "b.bin", false, 3000, 0, 0);
        t.finish();
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let cells = layout(&t, 0, rect, &Options { depth: 5, allocated: false, color: ColorMode::Folder, free: None });
        assert!(cells.iter().any(|c| c.depth == 2));
        for c in &cells {
            if c.parent != usize::MAX {
                assert!(cells[c.parent].rect.expand(0.01).contains_rect(c.rect));
            }
        }
    }
}
