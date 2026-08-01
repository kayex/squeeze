//! Visual styling, derived from the logo's palette.
//!
//! egui has no stylesheet: everything is a `Style`/`Visuals` struct you mutate
//! once at startup, plus per-widget overrides. Gradients aren't supported by the
//! built-in widgets either, so [`gradient_bar`] paints one from a `Mesh`.

use eframe::egui::{self, Color32};

// Sampled from assets/logo.png.
pub const NAVY_DEEP: Color32 = Color32::from_rgb(0x08, 0x0B, 0x22); // tile background
pub const CYAN: Color32 = Color32::from_rgb(0x0B, 0xDD, 0xE4); // top of the S
pub const PURPLE: Color32 = Color32::from_rgb(0x4F, 0x27, 0xD3); // bottom of the S

// Surfaces built around them.
pub const BG: Color32 = Color32::from_rgb(0x0A, 0x0D, 0x28);
pub const SURFACE: Color32 = Color32::from_rgb(0x12, 0x16, 0x38);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x1A, 0x1F, 0x47);
pub const BORDER: Color32 = Color32::from_rgb(0x25, 0x2B, 0x5C);

pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xF5);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8A, 0x90, 0xB8);

pub const OK: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const WARN: Color32 = Color32::from_rgb(0xE5, 0x9B, 0x3C);
pub const ERR: Color32 = Color32::from_rgb(0xF2, 0x5F, 0x5F);

/// Bundled type. The UI face carries the interface text; the mono face carries
/// anything technical (resolutions, frame rates, sizes) so digits stay aligned
/// and don't jitter as values update.
///
/// egui's stock Ubuntu-Light is a *light* weight and renders thin on macOS,
/// which is why it's replaced rather than merely supplemented.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // `SQUEEZE_FONT=inter` switches pairing while comparing looks.
    let inter = std::env::var("SQUEEZE_FONT").as_deref() == Ok("inter");
    let (ui_name, ui_bytes, mono_name, mono_bytes): (_, &[u8], _, &[u8]) = if inter {
        (
            "Inter",
            include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
            "JetBrainsMono",
            include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
        )
    } else {
        (
            "IBMPlexSans",
            include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
            "IBMPlexMono",
            include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
        )
    };

    fonts.font_data.insert(
        ui_name.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(ui_bytes)),
    );
    fonts.font_data.insert(
        mono_name.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(mono_bytes)),
    );

    // Put ours first, then keep egui's stock faces as fallbacks so emoji and
    // stray symbols (the → in the shape summary) still resolve.
    if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        f.insert(0, ui_name.to_owned());
        f.push("Hack".to_owned());
    }
    if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        f.insert(0, mono_name.to_owned());
    }

    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    // Pin the app to its own dark palette regardless of the OS preference, and
    // write it into both style slots so a theme switch can't undo it.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(apply_to);
}

fn apply_to(style: &mut egui::Style) {
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = NAVY_DEEP;
    v.faint_bg_color = SURFACE;
    v.override_text_color = Some(TEXT);
    v.window_stroke = egui::Stroke::new(1.0, BORDER);

    // Selection (the chosen size budget) picks up the logo's cyan.
    v.selection.bg_fill = CYAN.gamma_multiply(0.22);
    v.selection.stroke = egui::Stroke::new(1.0, CYAN);

    let w = &mut v.widgets;
    for s in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        // Small: a checkbox is only ~14px, so a large radius rounds it into a
        // circle and it reads as a radio button.
        s.corner_radius = egui::CornerRadius::same(4);
        s.bg_stroke = egui::Stroke::new(1.0, BORDER);
        s.fg_stroke = egui::Stroke::new(1.0, TEXT);
    }
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_DIM);

    w.inactive.bg_fill = SURFACE;
    w.inactive.weak_bg_fill = SURFACE;

    w.hovered.bg_fill = SURFACE_HI;
    w.hovered.weak_bg_fill = SURFACE_HI;
    w.hovered.bg_stroke = egui::Stroke::new(1.0, CYAN.gamma_multiply(0.6));

    w.active.bg_fill = SURFACE_HI;
    w.active.weak_bg_fill = SURFACE_HI;
    w.active.bg_stroke = egui::Stroke::new(1.0, CYAN);

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);

    use egui::{FontFamily::Proportional, FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Heading, FontId::new(21.0, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(13.5, Proportional)),
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, egui::FontFamily::Monospace),
        ),
    ]
    .into();
}

/// A thin cyan→purple progress bar, echoing the logo's gradient.
///
/// Built by hand: egui's `ProgressBar` takes a single fill colour, and clipping
/// is rectangle-only, so the fill is a `Mesh` quad and the track carries the
/// rounding.
pub fn gradient_bar(ui: &mut egui::Ui, fraction: f32) {
    let height = 6.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let radius = height / 2.0;

    painter.rect_filled(rect, radius, SURFACE_HI);

    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    let filled = egui::Rect::from_min_size(
        rect.min,
        egui::vec2((rect.width() * fraction).max(height), height),
    );

    // Colour is interpolated across the *whole* track, so the gradient stays put
    // as the bar grows rather than stretching with it.
    let end = lerp_color(CYAN, PURPLE, fraction);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(filled.left_top(), CYAN);
    mesh.colored_vertex(filled.left_bottom(), CYAN);
    mesh.colored_vertex(filled.right_top(), end);
    mesh.colored_vertex(filled.right_bottom(), end);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(2, 1, 3);

    // Rounded ends: clip the quad to the track, then cap it with circles.
    painter.with_clip_rect(rect).add(egui::Shape::mesh(mesh));
    painter.circle_filled(
        egui::pos2(filled.left() + radius, filled.center().y),
        radius,
        CYAN,
    );
    painter.circle_filled(
        egui::pos2(filled.right() - radius, filled.center().y),
        radius,
        end,
    );
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}
