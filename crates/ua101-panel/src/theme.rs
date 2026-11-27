//! Palette and egui theme of the panel's windows and dialogs (the desk
//! itself is drawn in `desk.rs`).

use eframe::egui::{self, Color32, CornerRadius, Stroke, Vec2};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x23);
pub const CARD: Color32 = Color32::from_rgb(0x22, 0x26, 0x2c);
pub const STRIP: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x35);
pub const BORDER: Color32 = Color32::from_rgb(0x33, 0x38, 0x41);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x91, 0x9c);

pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
pub const YELLOW: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
pub const BLUE: Color32 = Color32::from_rgb(0x4c, 0x9a, 0xff);
pub const ORANGE: Color32 = Color32::from_rgb(0xff, 0x8a, 0x3d);

/// Accent colour of each mixer section.
pub fn section_color(bus: ua101_usb::protocol::mixer::Bus) -> Color32 {
    use ua101_usb::protocol::mixer::Bus;
    match bus {
        Bus::Input => BLUE,
        Bus::Output => ORANGE,
        Bus::MonitorOut => GREEN,
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = BLUE.linear_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, BLUE);
    let r = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.bg_fill = STRIP;
    v.widgets.inactive.weak_bg_fill = STRIP;
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x3b, 0x44);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x3b, 0x44);
    v.widgets.active.bg_fill = Color32::from_rgb(0x41, 0x47, 0x52);
    v.window_corner_radius = CornerRadius::same(10);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.button_padding = Vec2::new(8.0, 3.0);
        s.spacing.item_spacing = Vec2::new(6.0, 5.0);
    });
}
