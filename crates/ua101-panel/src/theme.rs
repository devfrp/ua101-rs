//! Visual style of the panel: palette, egui theme and custom widgets.

use eframe::egui::{
    self, Color32, CornerRadius, Frame, Margin, Rect, Response, Sense, Stroke, Ui, Vec2, Widget,
};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x23);
pub const CARD: Color32 = Color32::from_rgb(0x22, 0x26, 0x2c);
pub const STRIP: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x35);
pub const BORDER: Color32 = Color32::from_rgb(0x33, 0x38, 0x41);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x91, 0x9c);

pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
pub const YELLOW: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
pub const RED: Color32 = Color32::from_rgb(0xf0, 0x55, 0x4a);
pub const BLUE: Color32 = Color32::from_rgb(0x4c, 0x9a, 0xff);
pub const ORANGE: Color32 = Color32::from_rgb(0xff, 0x8a, 0x3d);
pub const LCD_BG: Color32 = Color32::from_rgb(0x0b, 0x16, 0x10);
pub const LCD_FG: Color32 = Color32::from_rgb(0x6c, 0xf0, 0xa0);

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

/// Rounded card used for each mixer section.
pub fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
}

/// Background of one channel strip.
pub fn strip_frame() -> Frame {
    Frame::new()
        .fill(STRIP)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(3, 6))
}

/// iOS-style on/off switch.
pub struct Toggle<'a> {
    on: &'a mut bool,
    color: Color32,
}

impl<'a> Toggle<'a> {
    pub fn new(on: &'a mut bool, color: Color32) -> Self {
        Self { on, color }
    }
}

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = Vec2::new(36.0, 20.0);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let t = ui.ctx().animate_bool(response.id, *self.on);
            let off = Color32::from_rgb(0x44, 0x49, 0x52);
            let bg = lerp_color(off, self.color, t);
            let radius = rect.height() / 2.0;
            ui.painter().rect_filled(rect, radius, bg);
            let x = egui::lerp(rect.left() + radius..=rect.right() - radius, t);
            ui.painter().circle_filled(
                egui::pos2(x, rect.center().y),
                radius - 3.0,
                Color32::WHITE,
            );
        }
        response
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Vertical peak meter, `level` in 0.0–1.0 (linear), drawn in dBFS.
pub fn meter(ui: &mut Ui, height: f32, level: f32, active: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(6.0, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, BG);
    if !active {
        return;
    }
    // Map -60..0 dBFS onto the bar.
    let db = if level > 0.0 {
        20.0 * level.log10()
    } else {
        -120.0
    };
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    let top = rect.bottom() - rect.height() * frac;
    let segments = [(0.0, 0.75, GREEN), (0.75, 0.9, YELLOW), (0.9, 1.0, RED)];
    for (lo, hi, color) in segments {
        let y0 = rect.bottom() - rect.height() * lo;
        let y1 = (rect.bottom() - rect.height() * hi).max(top);
        if y1 < y0 {
            p.rect_filled(
                Rect::from_min_max(egui::pos2(rect.left(), y1), egui::pos2(rect.right(), y0)),
                1.0,
                color,
            );
        }
    }
}

/// Small LCD-style read-out.
pub fn lcd(ui: &mut Ui, title: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(title).size(9.0).color(TEXT_DIM));
        Frame::new()
            .fill(LCD_BG)
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x1e, 0x35, 0x28)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(value)
                        .monospace()
                        .size(15.0)
                        .color(LCD_FG),
                );
            });
    });
}

/// Round status lamp with a label.
pub fn lamp(ui: &mut Ui, label: &str, state: Option<bool>, color: Color32) -> Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::hover());
        let fill = match state {
            Some(true) => color,
            Some(false) => Color32::from_rgb(0x2d, 0x31, 0x37),
            None => Color32::from_rgb(0x22, 0x25, 0x2a),
        };
        if state == Some(true) {
            ui.painter()
                .circle_filled(rect.center(), 7.0, color.linear_multiply(0.25));
        }
        ui.painter().circle_filled(rect.center(), 5.0, fill);
        ui.label(egui::RichText::new(label).size(11.0));
    })
    .response
}

/// Pill showing the connection state.
pub fn status_pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.linear_multiply(0.15))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.6)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.label(egui::RichText::new(text).size(11.0).color(color));
            });
        });
}
