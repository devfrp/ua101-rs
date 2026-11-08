//! Patch bay with virtual cables: drag from an INPUT jack to an OUTPUT jack
//! to route that input to that output, channel by channel. Dragging a
//! cable's output end away unplugs it (or moves it to another output).
//! Outputs without a cable keep playing the computer's audio, and the whole
//! patch bay can be bypassed.

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Vec2,
    epaint::CubicBezierShape,
};

use crate::theme;

/// A change the user made, to send to `ua101d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchChange {
    Enable(bool),
    Clear,
    /// 0-based output, 0-based input (`None` unplugs).
    Route(usize, Option<usize>),
}

pub struct PatchbayUi {
    pub enabled: bool,
    /// Per output: the cabled input.
    pub routes: Vec<Option<usize>>,
    /// Input whose cable is being dragged.
    dragging: Option<usize>,
}

const ROW: f32 = 26.0;
const JACK: f32 = 7.0;
const WIDTH: f32 = 470.0;
const LABEL_W: f32 = 70.0;

fn cable_color(input: usize) -> Color32 {
    egui::ecolor::Hsva::new((input as f32 * 0.137) % 1.0, 0.7, 0.95, 1.0).into()
}

fn cable(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    let dx = (to.x - from.x).abs() * 0.45;
    let sag = 18.0 + (to.y - from.y).abs() * 0.15;
    let shape = CubicBezierShape::from_points_stroke(
        [
            from,
            from + Vec2::new(dx, sag),
            to + Vec2::new(-dx, sag),
            to,
        ],
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    );
    // Dark outline first so crossing cables stay readable.
    let mut outline = shape.clone();
    outline.stroke = Stroke::new(width + 2.0, Color32::from_black_alpha(160)).into();
    painter.add(outline);
    painter.add(shape);
}

fn jack(painter: &egui::Painter, at: Pos2, color: Option<Color32>, hot: bool) {
    painter.circle_filled(at, JACK, theme::BG);
    let ring = if hot { theme::TEXT } else { theme::BORDER };
    painter.circle_stroke(at, JACK, Stroke::new(2.0, ring));
    if let Some(c) = color {
        painter.circle_filled(at, JACK - 3.0, c);
    }
}

impl PatchbayUi {
    pub fn new(outputs: usize) -> Self {
        Self {
            enabled: false,
            routes: vec![None; outputs],
            dragging: None,
        }
    }

    /// Updates from a `patch on|off R,R,…` report.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn set_state(&mut self, enabled: bool, routes: &[usize]) {
        self.enabled = enabled;
        self.routes = routes.iter().map(|&r| r.checked_sub(1)).collect();
    }

    /// Draws the patch bay; `available` is false without `ua101d`.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        inputs: usize,
        levels_in: &[f32],
        available: bool,
    ) -> Vec<PatchChange> {
        let mut changes = Vec::new();
        let outputs = self.routes.len();

        ui.horizontal(|ui| {
            let (label, color) = if self.enabled {
                ("PATCH BAY  ON", theme::GREEN)
            } else {
                ("PATCH BAY  OFF (bypass)", theme::TEXT_DIM)
            };
            let b = egui::Button::new(RichText::new(label).strong().color(if self.enabled {
                Color32::BLACK
            } else {
                theme::TEXT
            }))
            .fill(if self.enabled { color } else { theme::STRIP });
            if ui.add_enabled(available, b).clicked() {
                changes.push(PatchChange::Enable(!self.enabled));
            }
            if ui
                .add_enabled(available, egui::Button::new("1:1 (IN n → OUT n)"))
                .on_hover_text("Cable INPUT n to OUTPUT n for every output")
                .clicked()
            {
                for o in 0..outputs.min(inputs) {
                    changes.push(PatchChange::Route(o, Some(o)));
                }
            }
            if ui
                .add_enabled(available, egui::Button::new("Unplug all"))
                .clicked()
            {
                changes.push(PatchChange::Clear);
            }
        });
        ui.label(
            RichText::new(
                "Drag from an INPUT to an OUTPUT. Drag a cable's end away to unplug it. \
                 Outputs without a cable play the computer's audio.",
            )
            .size(11.0)
            .color(theme::TEXT_DIM),
        );
        if !available {
            ui.colored_label(
                theme::YELLOW,
                "The patch bay runs in the audio daemon: start ua101d to use it.",
            );
        }
        ui.add_space(6.0);

        let rows = inputs.max(outputs);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(WIDTH, rows as f32 * ROW + 28.0), Sense::hover());
        let painter = ui.painter_at(rect.expand(4.0));
        painter.rect_filled(rect, CornerRadius::same(8), theme::CARD);

        let title_y = rect.top() + 12.0;
        let in_x = rect.left() + LABEL_W + 16.0;
        let out_x = rect.right() - LABEL_W - 16.0;
        let font = egui::FontId::proportional(11.0);
        painter.text(
            Pos2::new(in_x - 10.0, title_y),
            egui::Align2::CENTER_CENTER,
            "INPUTS",
            font.clone(),
            theme::section_color(ua101_usb::protocol::mixer::Bus::Input),
        );
        painter.text(
            Pos2::new(out_x + 10.0, title_y),
            egui::Align2::CENTER_CENTER,
            "OUTPUTS",
            font.clone(),
            theme::section_color(ua101_usb::protocol::mixer::Bus::Output),
        );
        let row_y = |i: usize, n: usize| {
            // Centre the shorter column.
            let offset = (rows - n) as f32 * ROW / 2.0;
            rect.top() + 28.0 + offset + (i as f32 + 0.5) * ROW
        };
        let in_pos = |i: usize| Pos2::new(in_x, row_y(i, inputs));
        let out_pos = |o: usize| Pos2::new(out_x, row_y(o, outputs));
        let hit = |p: Pos2| Rect::from_center_size(p, Vec2::splat(ROW));
        let pointer = ui.ctx().pointer_interact_pos();

        // Input jacks: start a cable.
        for i in 0..inputs {
            let p = in_pos(i);
            let resp = ui.interact(hit(p), ui.scope_id().with(("patch-in", i)), Sense::drag());
            if available && resp.drag_started() {
                self.dragging = Some(i);
            }
            let level = levels_in.get(i).copied().unwrap_or(0.0);
            let lit = level > 0.003; // about -50 dBFS
            painter.circle_filled(
                Pos2::new(rect.left() + 12.0, p.y),
                3.0,
                if lit { theme::GREEN } else { theme::STRIP },
            );
            painter.text(
                Pos2::new(in_x - JACK - 8.0, p.y),
                egui::Align2::RIGHT_CENTER,
                format!("IN {}", i + 1),
                font.clone(),
                theme::TEXT,
            );
            let used = self.routes.contains(&Some(i)) || self.dragging == Some(i);
            jack(&painter, p, used.then(|| cable_color(i)), resp.hovered());
        }

        // Output jacks: drop target, and pick up an existing cable.
        let mut drop_target = None;
        for o in 0..outputs {
            let p = out_pos(o);
            let resp = ui.interact(hit(p), ui.scope_id().with(("patch-out", o)), Sense::drag());
            if available
                && resp.drag_started()
                && let Some(i) = self.routes[o]
            {
                self.routes[o] = None;
                self.dragging = Some(i);
                changes.push(PatchChange::Route(o, None));
            }
            let over = pointer.is_some_and(|q| hit(p).contains(q));
            if over && self.dragging.is_some() {
                drop_target = Some(o);
            }
            painter.text(
                Pos2::new(out_x + JACK + 8.0, p.y),
                egui::Align2::LEFT_CENTER,
                format!("OUT {}", o + 1),
                font.clone(),
                theme::TEXT,
            );
            let src = match self.routes[o] {
                Some(i) => format!("IN {}", i + 1),
                None => "PC".into(),
            };
            painter.text(
                Pos2::new(rect.right() - 8.0, p.y),
                egui::Align2::RIGHT_CENTER,
                src,
                egui::FontId::monospace(10.0),
                theme::TEXT_DIM,
            );
            jack(
                &painter,
                p,
                self.routes[o].map(cable_color),
                resp.hovered() || over,
            );
        }

        // Cables.
        let width = if self.enabled { 3.0 } else { 2.0 };
        for (o, r) in self.routes.iter().enumerate() {
            if let Some(i) = *r
                && i < inputs
            {
                let mut c = cable_color(i);
                if !self.enabled {
                    c = c.gamma_multiply(0.35);
                }
                cable(&painter, in_pos(i), out_pos(o), c, width);
            }
        }
        if let (Some(i), Some(q)) = (self.dragging, pointer) {
            let to = drop_target.map_or(q, out_pos);
            cable(&painter, in_pos(i), to, cable_color(i), 3.0);
            ui.ctx().request_repaint();
        }

        // Plug in on release.
        if self.dragging.is_some() && ui.input(|i| i.pointer.any_released()) {
            if let (Some(i), Some(o)) = (self.dragging, drop_target) {
                self.routes[o] = Some(i);
                changes.push(PatchChange::Route(o, Some(i)));
            }
            self.dragging = None;
        }
        // Optimistic local update; the daemon's report confirms it.
        for c in &changes {
            match *c {
                PatchChange::Enable(on) => self.enabled = on,
                PatchChange::Clear => self.routes.iter_mut().for_each(|r| *r = None),
                PatchChange::Route(o, i) => {
                    if let Some(r) = self.routes.get_mut(o) {
                        *r = i;
                    }
                }
            }
        }
        changes
    }
}
