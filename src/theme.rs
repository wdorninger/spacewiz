//! Windows 11 look (Segoe UI, rounded flat widgets, surface colours) and the chart colours.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, Theme};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

pub struct Palette {
    pub bg: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub error: Color32,
    pub warn: Color32,
    pub card: Color32,
    pub border: Color32,
    pub free: Color32,
    pub small: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const LIGHT: Palette = Palette {
    bg: hex(0xf9f9f9),
    text: hex(0x1b1b1b),
    muted: hex(0x6b6b6b),
    accent: hex(0x005fb8),
    on_accent: Color32::WHITE,
    error: hex(0xc42b1c),
    warn: hex(0x9d5d00),
    card: hex(0xffffff),
    border: hex(0xe0e0e0),
    free: hex(0xe4e4e4),
    small: hex(0xc8c8c8),
};

pub const DARK: Palette = Palette {
    bg: hex(0x1c1c1c),
    text: hex(0xececec),
    muted: hex(0x9d9d9d),
    accent: hex(0x60cdff),
    on_accent: hex(0x000000),
    error: hex(0xff99a4),
    warn: hex(0xfce100),
    card: hex(0x2b2b2b),
    border: hex(0x3a3a3a),
    free: hex(0x333333),
    small: hex(0x4a4a4a),
};

pub fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// HSL (all 0..1) to a colour.
pub fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let h = h.rem_euclid(1.0) * 6.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

/// Colour of a chart sector: `hue` from the top-level branch it belongs to, a little lighter and less
/// saturated with every ring outward; files paler than folders.
pub fn sector_color(hue: f32, depth: usize, is_file: bool, dark: bool) -> Color32 {
    let d = depth.saturating_sub(1) as f32;
    let (mut s, mut l) = if dark { (0.62 - d * 0.035, 0.52 + d * 0.035) } else { (0.68 - d * 0.035, 0.50 + d * 0.04) };
    if is_file {
        s *= 0.55;
        l += if dark { -0.06 } else { 0.08 };
    }
    hsl(hue + d * 0.012, s.max(0.2), l.min(0.86))
}

/// Colour of a file type by its rank in the type list (largest first); the long tail is grey.
pub fn type_color(rank: usize, dark: bool) -> Color32 {
    if rank == usize::MAX || rank >= 24 {
        return palette(dark).small;
    }
    // Golden-ratio steps spread neighbouring ranks far apart on the colour wheel.
    let hue = 0.58 + rank as f32 * 0.618_034;
    let l = if dark { 0.58 } else { 0.52 } + if rank % 2 == 1 { 0.06 } else { 0.0 };
    hsl(hue, 0.62, l)
}

/// Age colours from recent (warm) to untouched for years (cool, grey-blue); `bucket` 0..=5.
pub fn age_color(bucket: usize, dark: bool) -> Color32 {
    const LIGHT_AGE: [u32; 6] = [0xe0482b, 0xf08a24, 0xe9c537, 0x7cbf6b, 0x3f8fc4, 0x7b8796];
    const DARK_AGE: [u32; 6] = [0xff6a4d, 0xffa043, 0xf2d35b, 0x8fd17e, 0x5aa7dc, 0x6c7787];
    hex(if dark { DARK_AGE } else { LIGHT_AGE }[bucket.min(5)])
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Whether the Windows icon font loaded (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10).
static HAS_ICONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub const ICON_BACK: char = '\u{E72B}';
pub const ICON_FORWARD: char = '\u{E72A}';
pub const ICON_UP: char = '\u{E74A}';
pub const ICON_REFRESH: char = '\u{E72C}';
pub const ICON_FOLDER: char = '\u{E838}';
pub const ICON_SETTINGS: char = '\u{E713}';
pub const ICON_SHIELD: char = '\u{EA18}';
pub const ICON_DRIVE: char = '\u{EDA2}';

/// An icon from the Windows icon font (the glyphs Settings and Explorer use), or `fallback` if it is missing.
pub fn icon(glyph: char, fallback: &str) -> egui::RichText {
    if HAS_ICONS.load(std::sync::atomic::Ordering::Relaxed) {
        egui::RichText::new(glyph.to_string()).family(FontFamily::Name("icons".into())).size(14.0)
    } else {
        egui::RichText::new(fallback)
    }
}
/// Segoe UI from the Windows fonts folder, with egui's built-in fonts as fallback for symbols and emoji.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load("segoe", "segoeui.ttf");
    let bold_ok = load("segoe-bold", "segoeuib.ttf");
    let symbols = load("segoe-symbol", "seguisym.ttf");
    let icons = load("icons", "SegoeIcons.ttf") || load("icons", "segmdl2.ttf");
    HAS_ICONS.store(icons, std::sync::atomic::Ordering::Relaxed);
    if icons {
        fonts.families.insert(FontFamily::Name("icons".into()), vec!["icons".into()]);
    }
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family = |primary: Option<&str>| {
        let mut v: Vec<String> = primary.map(|p| vec![p.to_string()]).unwrap_or_default();
        if regular && primary != Some("segoe") {
            v.push("segoe".into());
        }
        v.extend(fallbacks.iter().cloned());
        if symbols {
            v.push("segoe-symbol".into());
        }
        v
    };
    fonts.families.insert(FontFamily::Proportional, family(regular.then_some("segoe")));
    fonts.families.insert(bold(), family(bold_ok.then_some("segoe-bold")));
    ctx.set_fonts(fonts);
}

/// Rounded, flat widgets and Windows 11 surface colours for both themes.
pub fn install_style(ctx: &egui::Context) {
    for (theme, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
        ctx.style_mut_of(theme, |style| {
            let p = palette(dark);
            let v = &mut style.visuals;
            v.panel_fill = if dark { hex(0x202020) } else { hex(0xf3f3f3) };
            v.window_fill = if dark { hex(0x2b2b2b) } else { hex(0xfbfbfb) };
            v.extreme_bg_color = if dark { hex(0x1c1c1c) } else { hex(0xffffff) };
            v.selection.bg_fill = p.accent.gamma_multiply(0.35);
            v.selection.stroke = Stroke::new(1.0, p.text);
            v.hyperlink_color = p.accent;
            let radius = CornerRadius::same(5);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                w.corner_radius = radius;
            }
            v.widgets.noninteractive.corner_radius = radius;
            v.widgets.inactive.weak_bg_fill = if dark { hex(0x2d2d2d) } else { hex(0xfbfbfb) };
            v.widgets.inactive.bg_fill = v.widgets.inactive.weak_bg_fill;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, if dark { hex(0x3a3a3a) } else { hex(0xe0e0e0) });
            v.widgets.hovered.weak_bg_fill = if dark { hex(0x323232) } else { hex(0xf5f5f5) };
            v.widgets.hovered.bg_fill = v.widgets.hovered.weak_bg_fill;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, if dark { hex(0x444444) } else { hex(0xd5d5d5) });
            v.window_corner_radius = CornerRadius::same(8);
            v.menu_corner_radius = CornerRadius::same(8);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
            style.spacing.interact_size.y = 26.0;
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            // Like native controls: label and button text in the Windows text colour (egui's default is a
            // mid grey, which makes text look thinner) at 12 px (Segoe UI 9 pt; egui's default is 13 px).
            let w = &mut style.visuals.widgets;
            for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                s.fg_stroke.color = p.text;
            }
            for t in [egui::TextStyle::Body, egui::TextStyle::Button] {
                style.text_styles.insert(t, egui::FontId::new(12.0, FontFamily::Proportional));
            }
        });
    }
}

/// App icon: a small sunburst (blue core, coloured ring segments), as RGBA pixels.
pub fn app_icon(size: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).expect("icon size");
    let u = size as f32 / 64.0;
    let (cx, cy) = (32.0 * u, 32.0 * u);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    let mut sector = |r0: f32, r1: f32, a0: f32, a1: f32, c: Color32| {
        let mut pb = PathBuilder::new();
        let steps = 24;
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            let (x, y) = (cx + r1 * a.cos(), cy + r1 * a.sin());
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        for i in (0..=steps).rev() {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            pb.line_to(cx + r0 * a.cos(), cy + r0 * a.sin());
        }
        pb.close();
        if let Some(p) = pb.finish() {
            paint.set_color_rgba8(c.r(), c.g(), c.b(), 255);
            pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
        }
    };
    let tau = std::f32::consts::TAU;
    let gap = 0.06;
    let inner = [(0.0, 0.42, 0.00), (0.42, 0.70, 0.33), (0.70, 1.0, 0.12)];
    for (f0, f1, hue) in inner {
        sector(13.0 * u, 22.0 * u, f0 * tau - tau / 4.0 + gap / 2.0, f1 * tau - tau / 4.0 - gap / 2.0, hsl(hue, 0.75, 0.52));
    }
    let outer = [(0.0, 0.25, 0.02), (0.25, 0.38, 0.06), (0.42, 0.62, 0.36), (0.70, 0.86, 0.13), (0.86, 0.95, 0.16)];
    for (f0, f1, hue) in outer {
        sector(23.5 * u, 31.0 * u, f0 * tau - tau / 4.0 + gap / 2.0, f1 * tau - tau / 4.0 - gap / 2.0, hsl(hue, 0.6, 0.64));
    }
    sector(0.0, 11.5 * u, 0.0, tau, Color32::from_rgb(0x2f, 0x6f, 0xd6));
    // tiny-skia stores premultiplied colours; icons want straight alpha.
    pm.pixels()
        .iter()
        .flat_map(|px| {
            let c = px.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}
