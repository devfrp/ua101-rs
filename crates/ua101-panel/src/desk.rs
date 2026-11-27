//! The mixer drawn as a console desk: a brushed blue plate, channel strips
//! with M and S keys, a pan knob, a dB-scaled fader beside its meter, LINK
//! bars under each pair, and a master section on the right with the sample
//! rate on a seven-segment display. The desk is laid out in desk units and
//! scaled to the window, so it keeps its proportions at any size.

use eframe::egui::{
    self, Align2, Color32, FontId, Id, Mesh, Painter, Pos2, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use ua101_usb::protocol::mixer::Bus;

use crate::{FADER_MAX_DB, FADER_MIN_DB, Panel, format_db};

// ---------------------------------------------------------------- layout

const STRIP_W: f32 = 40.0;
const MARGIN: f32 = 14.0;
const GAP: f32 = 14.0;
const MASTER_W: f32 = 156.0;
const HEADER_H: f32 = 34.0;
/// Top of the fader travel, from the top of the strips.
const FADER_TOP: f32 = 104.0;
const FADER_H: f32 = 270.0;
const STRIP_H: f32 = FADER_TOP + FADER_H + 64.0;
const SECTIONS: [Bus; 3] = [Bus::Input, Bus::Output, Bus::MonitorOut];
const DESK_H: f32 = MARGIN + HEADER_H + STRIP_H + MARGIN;

fn desk_w() -> f32 {
    let strips: f32 = SECTIONS.iter().map(|b| b.channels() as f32).sum();
    MARGIN + strips * STRIP_W + SECTIONS.len() as f32 * GAP + MASTER_W + MARGIN
}

/// Fader taper: (dB, position from the top of the travel). The bottom is
/// -inf, as `FADER_MIN_DB` is.
const TAPER: [(f32, f32); 8] = [
    (FADER_MAX_DB, 0.0),
    (0.0, 0.18),
    (-6.0, 0.31),
    (-12.0, 0.44),
    (-20.0, 0.58),
    (-30.0, 0.72),
    (-40.0, 0.84),
    (FADER_MIN_DB, 1.0),
];

fn db_to_t(db: f32) -> f32 {
    let db = db.clamp(FADER_MIN_DB, FADER_MAX_DB);
    for w in TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (w[0], w[1]);
        if db <= d0 && db >= d1 {
            return t0 + (d0 - db) / (d0 - d1) * (t1 - t0);
        }
    }
    1.0
}

fn t_to_db(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    for w in TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (w[0], w[1]);
        if t >= t0 && t <= t1 {
            return d0 - (t - t0) / (t1 - t0) * (d0 - d1);
        }
    }
    FADER_MIN_DB
}

/// Maps desk units to the screen.
#[derive(Clone, Copy)]
struct Geo {
    o: Pos2,
    s: f32,
}

impl Geo {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.o + vec2(x, y) * self.s
    }
    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), vec2(w, h) * self.s)
    }
    fn font(&self, size: f32) -> FontId {
        FontId::proportional(size * self.s)
    }
}

// ---------------------------------------------------------------- palette

const PLATE_TOP: Color32 = Color32::from_rgb(0x23, 0x40, 0x94);
const PLATE_BOTTOM: Color32 = Color32::from_rgb(0x15, 0x2b, 0x6e);
const PLATE_EDGE: Color32 = Color32::from_rgb(0x07, 0x0b, 0x18);
const LEGEND: Color32 = Color32::from_rgb(0xe9, 0xee, 0xf8);
const LEGEND_DIM: Color32 = Color32::from_rgb(0xa9, 0xb7, 0xdd);
const RULE: Color32 = Color32::from_rgba_premultiplied(0x9a, 0xb0, 0xf0, 0x50);
const KEY: Color32 = Color32::from_rgb(0xb4, 0xb8, 0xc1);
const KEY_OFF: Color32 = Color32::from_rgb(0x5d, 0x63, 0x73);
const KEY_TEXT: Color32 = Color32::from_rgb(0x1f, 0x22, 0x2a);
const MUTE_LIT: Color32 = Color32::from_rgb(0xff, 0x5c, 0x5c);
const LINK_LIT: Color32 = Color32::from_rgb(0xa8, 0xf0, 0xb8);
const LAMP_ON: Color32 = Color32::from_rgb(0xff, 0x77, 0x77);
const OK: Color32 = Color32::from_rgb(0x74, 0xf0, 0xa0);
const WARN: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
const BAD: Color32 = Color32::from_rgb(0xff, 0x55, 0x55);
const WELL: Color32 = Color32::from_rgb(0x07, 0x0b, 0x1a);
const SLOT: Color32 = Color32::from_rgb(0x04, 0x06, 0x0e);
/// Meter colours, low to high: blue, sky blue, yellow (readable by
/// colour-blind users and in grey).
const METER: [(f32, Color32); 3] = [
    (0.0, Color32::from_rgb(0x1c, 0x5f, 0xc8)),
    (0.65, Color32::from_rgb(0x2f, 0xb4, 0xe6)),
    (1.0, Color32::from_rgb(0xf2, 0xd0, 0x3c)),
];
const PEAK_LINE: Color32 = Color32::from_rgb(0xe8, 0xf0, 0xff);
const CLIP: Color32 = Color32::from_rgb(0xff, 0x5a, 0x1f);
const SEG_ON: Color32 = Color32::from_rgb(0xff, 0x2e, 0x2e);
const SEG_OFF: Color32 = Color32::from_rgb(0x3a, 0x08, 0x0a);

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn meter_color(f: f32) -> Color32 {
    for w in METER.windows(2) {
        let ((f0, c0), (f1, c1)) = (w[0], w[1]);
        if f <= f1 {
            return lerp(c0, c1, ((f - f0) / (f1 - f0)).clamp(0.0, 1.0));
        }
    }
    METER[METER.len() - 1].1
}

fn vgrad(p: &Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.right_bottom(), bottom);
    m.colored_vertex(r.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(m);
}

// ---------------------------------------------------------------- widgets

/// Peak hold and clip light of one meter.
#[derive(Clone, Copy, Default)]
pub struct Hold {
    peak: f32,
    peak_at: f64,
    clip_at: Option<f64>,
}

const PEAK_HOLD_S: f64 = 1.5;
const CLIP_HOLD_S: f64 = 2.0;

fn plate(p: &Painter, rect: Rect, s: f32) {
    p.rect_filled(rect.expand(3.0 * s), 7.0 * s, PLATE_EDGE);
    vgrad(p, rect, PLATE_TOP, PLATE_BOTTOM);
    // Brushed metal: faint horizontal lines of varying brightness.
    let mut y = rect.top();
    let mut i: u32 = 0;
    while y < rect.bottom() {
        let h = i.wrapping_mul(2_654_435_761).rotate_right(13);
        let a = (h % 10) as u8;
        let c = if h & 0x100 != 0 {
            Color32::from_white_alpha(a)
        } else {
            Color32::from_black_alpha(a + 6)
        };
        p.hline(rect.x_range(), y, Stroke::new(1.0, c));
        y += 1.5;
        i += 1;
    }
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, Color32::from_white_alpha(40)),
    );
}

/// A square key. `lit` gives its colour when on; a disabled key only
/// shows its tooltip.
fn key(
    ui: &Ui,
    rect: Rect,
    id: Id,
    label: &str,
    lit: Option<Color32>,
    enabled: bool,
    s: f32,
) -> Response {
    let r = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter();
    let cr = 2.0 * s;
    p.rect_filled(
        rect.translate(vec2(0.0, 1.5 * s)),
        cr,
        Color32::from_black_alpha(120),
    );
    let mut body = if enabled { lit.unwrap_or(KEY) } else { KEY_OFF };
    if enabled && r.hovered() {
        body = lerp(body, Color32::WHITE, 0.18);
    }
    if lit.is_some() && enabled {
        p.rect_filled(rect.expand(2.0 * s), 3.0 * s, body.gamma_multiply(0.25));
    }
    p.rect_filled(rect, cr, body);
    p.rect_filled(
        Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y)),
        cr,
        Color32::from_white_alpha(30),
    );
    p.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, Color32::from_black_alpha(150)),
        StrokeKind::Inside,
    );
    if !label.is_empty() {
        let tc = if enabled {
            KEY_TEXT
        } else {
            Color32::from_rgb(0x2e, 0x32, 0x3c)
        };
        let size = (rect.height() * 0.55).min(10.5 * s);
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(size),
            tc,
        );
    }
    r
}

/// A read-only lamp: on, off, or unknown (`None`).
fn lamp(ui: &Ui, rect: Rect, id: Id, state: Option<bool>, s: f32) -> Response {
    let r = ui.interact(rect, id, Sense::hover());
    let p = ui.painter();
    let cr = 2.0 * s;
    p.rect_filled(
        rect.translate(vec2(0.0, 1.5 * s)),
        cr,
        Color32::from_black_alpha(120),
    );
    let body = match state {
        Some(true) => {
            p.rect_filled(rect.expand(3.0 * s), 4.0 * s, LAMP_ON.gamma_multiply(0.3));
            LAMP_ON
        }
        Some(false) => KEY,
        None => KEY_OFF,
    };
    p.rect_filled(rect, cr, body);
    p.rect_filled(
        Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y)),
        cr,
        Color32::from_white_alpha(35),
    );
    p.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, Color32::from_black_alpha(150)),
        StrokeKind::Inside,
    );
    r
}

/// Rotary pan knob around `c`. Drag up or right to turn right, scroll,
/// double-click to centre.
fn knob(ui: &Ui, g: Geo, c: Pos2, id: Id, pan: &mut i32) -> Response {
    let s = g.s;
    let rad = 11.0 * s;
    let r = ui.interact(
        Rect::from_center_size(c, Vec2::splat(rad * 2.6)),
        id,
        Sense::click_and_drag(),
    );
    let acc_id = id.with("acc");
    if r.drag_started() {
        ui.data_mut(|d| d.insert_temp(acc_id, *pan as f32));
    }
    if r.dragged() {
        let d = r.drag_delta();
        let acc = ui.data_mut(|m| {
            let a = m.get_temp_mut_or(acc_id, *pan as f32);
            *a = (*a + (d.x - d.y) / s * 0.6).clamp(-64.0, 63.0);
            *a
        });
        *pan = acc.round() as i32;
    }
    if r.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            *pan = (*pan + if scroll > 0.0 { 2 } else { -2 }).clamp(-64, 63);
        }
    }
    if r.double_clicked() {
        *pan = 0;
    }

    let p = ui.painter();
    let frac = if *pan < 0 {
        *pan as f32 / 64.0
    } else {
        *pan as f32 / 63.0
    };
    let sweep = 135f32.to_radians();
    let dir = |a: f32| vec2(a.sin(), -a.cos());
    for a in [-sweep, 0.0, sweep] {
        p.line_segment(
            [c + dir(a) * (rad + 2.0 * s), c + dir(a) * (rad + 5.0 * s)],
            Stroke::new(1.2 * s, LEGEND_DIM),
        );
    }
    p.circle_filled(
        c + vec2(1.0, 2.0) * s,
        rad + 1.0 * s,
        Color32::from_black_alpha(130),
    );
    p.circle_filled(c, rad, Color32::from_rgb(0x16, 0x17, 0x1c));
    p.circle_filled(
        c - vec2(2.5, 3.0) * s,
        rad * 0.55,
        Color32::from_white_alpha(14),
    );
    p.circle_stroke(
        c,
        rad,
        Stroke::new(1.2 * s, Color32::from_rgb(0x5a, 0x5e, 0x68)),
    );
    let a = frac * sweep;
    p.line_segment(
        [c + dir(a) * rad * 0.3, c + dir(a) * rad * 0.92],
        Stroke::new(2.0 * s, Color32::WHITE),
    );
    let f = g.font(7.5);
    p.text(
        c + vec2(-rad - 1.0 * s, rad + 4.0 * s),
        Align2::CENTER_CENTER,
        "L",
        f.clone(),
        LEGEND,
    );
    p.text(
        c + vec2(rad + 1.0 * s, rad + 4.0 * s),
        Align2::CENTER_CENTER,
        "R",
        f,
        LEGEND,
    );
    r
}

/// Fader cap colours, as on a console: black on inputs, silver on the
/// computer's outputs, red on the master.
fn cap_colors(bus: Bus) -> (Color32, Color32, Color32) {
    match bus {
        Bus::Input => (
            Color32::from_rgb(0x2a, 0x2b, 0x30),
            Color32::from_rgb(0x0c, 0x0c, 0x0f),
            Color32::WHITE,
        ),
        Bus::Output => (
            Color32::from_rgb(0xdc, 0xdf, 0xe4),
            Color32::from_rgb(0x8a, 0x8f, 0x98),
            Color32::from_rgb(0x30, 0x33, 0x3a),
        ),
        Bus::MonitorOut => (
            Color32::from_rgb(0xe0, 0x3a, 0x30),
            Color32::from_rgb(0x8a, 0x14, 0x12),
            Color32::WHITE,
        ),
    }
}

/// Fader on the track at `x`; drag the cap, scroll over the travel,
/// double-click for 0 dB.
fn fader(ui: &Ui, g: Geo, x: f32, top: f32, id: Id, bus: Bus, db: &mut f32) -> Response {
    let s = g.s;
    let p = ui.painter();
    // Scale.
    let f = g.font(7.5);
    for &(d, t) in &TAPER {
        let y = top + t * FADER_H;
        let label = if d <= FADER_MIN_DB {
            "∞".to_string()
        } else if d > 0.0 {
            format!("+{}", d as i32)
        } else {
            format!("{}", (-d) as i32)
        };
        p.text(
            g.p(x - 9.0, y),
            Align2::RIGHT_CENTER,
            label,
            f.clone(),
            LEGEND,
        );
        p.line_segment(
            [g.p(x - 8.0, y), g.p(x - 4.5, y)],
            Stroke::new(1.0 * s, LEGEND_DIM),
        );
    }
    // Slot.
    let slot = g.r(x - 2.0, top - 6.0, 4.0, FADER_H + 12.0);
    p.rect_filled(slot, 2.0 * s, SLOT);
    p.rect_stroke(
        slot,
        2.0 * s,
        Stroke::new(1.0, Color32::from_rgb(0x3b, 0x52, 0x9a)),
        StrokeKind::Outside,
    );

    // Cap and interaction.
    let cap_at = |db: f32| g.r(x - 10.0, top + db_to_t(db) * FADER_H - 15.0, 20.0, 30.0);
    let travel = ui.interact(
        g.r(x - 12.0, top - 15.0, 24.0, FADER_H + 30.0),
        id.with("travel"),
        Sense::hover(),
    );
    let r = ui.interact(cap_at(*db), id, Sense::click_and_drag());
    let acc_id = id.with("acc");
    if r.drag_started() {
        ui.data_mut(|d| d.insert_temp(acc_id, db_to_t(*db)));
    }
    if r.dragged() {
        let dy = r.drag_delta().y / (FADER_H * s);
        let t = ui.data_mut(|m| {
            let a = m.get_temp_mut_or(acc_id, db_to_t(*db));
            *a = (*a + dy).clamp(0.0, 1.0);
            *a
        });
        let mut v = t_to_db(t);
        // Detent at 0 dB.
        if v.abs() < 0.4 {
            v = 0.0;
        }
        *db = v;
    }
    if travel.hovered() || r.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let step = if scroll > 0.0 { 0.5 } else { -0.5 };
            *db = (*db + step).clamp(FADER_MIN_DB, FADER_MAX_DB);
        }
    }
    if r.double_clicked() {
        *db = 0.0;
    }

    let cap = cap_at(*db);
    let (body, ridge, line) = cap_colors(bus);
    p.rect_filled(
        cap.translate(vec2(1.5, 2.5) * s),
        2.0 * s,
        Color32::from_black_alpha(140),
    );
    vgrad(p, cap, lerp(body, Color32::WHITE, 0.12), ridge);
    let mut yy = cap.top() + 3.0 * s;
    while yy < cap.bottom() - 2.0 * s {
        p.hline(
            cap.x_range().shrink(2.0 * s),
            yy,
            Stroke::new(1.0, ridge.gamma_multiply(0.8)),
        );
        yy += 3.0 * s;
    }
    p.hline(cap.x_range(), cap.center().y, Stroke::new(2.0 * s, line));
    p.rect_stroke(
        cap,
        2.0 * s,
        Stroke::new(1.0, Color32::from_black_alpha(170)),
        StrokeKind::Inside,
    );
    r.union(travel)
}

/// Peak meter from -60 dBFS (bottom) to 0 dBFS (top), with a peak line held
/// 1.5 s and a clip light held 2 s. `level` is linear, 0.0–1.0.
fn meter(ui: &Ui, g: Geo, x: f32, top: f32, level: Option<f32>, hold: &mut Hold) {
    let s = g.s;
    let p = ui.painter();
    let well = g.r(x, top - 6.0, 5.0, FADER_H + 12.0);
    let clip = g.r(x, top - 12.0, 5.0, 4.0);
    p.rect_filled(well, 1.5 * s, WELL);
    p.rect_stroke(
        well,
        1.5 * s,
        Stroke::new(1.0, Color32::from_rgb(0x2c, 0x40, 0x80)),
        StrokeKind::Outside,
    );
    let Some(level) = level else {
        p.rect_filled(clip, 1.0 * s, WELL);
        return;
    };
    let now = ui.input(|i| i.time);
    let frac = |l: f32| {
        if l > 0.0 {
            ((20.0 * l.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    if level >= hold.peak || now - hold.peak_at > PEAK_HOLD_S {
        hold.peak = level;
        hold.peak_at = now;
    }
    if level >= 0.999 {
        hold.clip_at = Some(now);
    }
    let lit = frac(level);
    let inner = well.shrink(1.0 * s);
    let y_of = |f: f32| inner.bottom() - inner.height() * f;
    for w in METER.windows(2) {
        let (f0, f1) = (w[0].0, w[1].0.min(lit));
        if f1 <= f0 {
            break;
        }
        let r = Rect::from_x_y_ranges(inner.x_range(), y_of(f1)..=y_of(f0));
        vgrad(p, r, meter_color(f1), meter_color(f0));
    }
    let pk = frac(hold.peak);
    if pk > 0.0 {
        p.hline(inner.x_range(), y_of(pk), Stroke::new(1.5 * s, PEAK_LINE));
    }
    let clipping = hold.clip_at.is_some_and(|t| now - t < CLIP_HOLD_S);
    p.rect_filled(clip, 1.0 * s, if clipping { CLIP } else { WELL });
    if pk > 0.0 || clipping {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Seven-segment read-out of `text` (digits, `-` and `.`) in `rect`.
fn seven_seg(p: &Painter, rect: Rect, text: &str) {
    // Segments a..g as bits 0..6.
    let bits = |c: char| -> u8 {
        match c {
            '0' => 0b011_1111,
            '1' => 0b000_0110,
            '2' => 0b101_1011,
            '3' => 0b100_1111,
            '4' => 0b110_0110,
            '5' => 0b110_1101,
            '6' => 0b111_1101,
            '7' => 0b000_0111,
            '8' => 0b111_1111,
            '9' => 0b110_1111,
            '-' => 0b100_0000,
            _ => 0,
        }
    };
    let cells: Vec<(u8, bool)> = {
        let mut v: Vec<(u8, bool)> = Vec::new();
        for c in text.chars() {
            if c == '.' {
                if let Some(last) = v.last_mut() {
                    last.1 = true;
                }
            } else {
                v.push((bits(c), false));
            }
        }
        v
    };
    let n = cells.len().max(1) as f32;
    let h = rect.height() * 0.74;
    let w = h * 0.5;
    let gap = w * 0.45;
    let total = n * w + (n - 1.0) * gap;
    let x0 = rect.center().x - total / 2.0;
    let y0 = rect.center().y - h / 2.0;
    let th = (h * 0.11).max(1.5);
    let skew = 0.1;
    for (i, &(b, dot)) in cells.iter().enumerate() {
        let left = x0 + i as f32 * (w + gap);
        // Points of the digit, slanted.
        let pt = |fx: f32, fy: f32| {
            let y = y0 + fy * h;
            pos2(left + fx * w + (0.5 - fy) * h * skew, y)
        };
        let segs = [
            (pt(0.12, 0.0), pt(0.88, 0.0)),
            (pt(1.0, 0.08), pt(1.0, 0.44)),
            (pt(1.0, 0.56), pt(1.0, 0.92)),
            (pt(0.12, 1.0), pt(0.88, 1.0)),
            (pt(0.0, 0.56), pt(0.0, 0.92)),
            (pt(0.0, 0.08), pt(0.0, 0.44)),
            (pt(0.12, 0.5), pt(0.88, 0.5)),
        ];
        for (k, (a, z)) in segs.iter().enumerate() {
            let on = b & (1 << k) != 0;
            if on {
                p.line_segment([*a, *z], Stroke::new(th * 2.2, SEG_ON.gamma_multiply(0.18)));
            }
            p.line_segment([*a, *z], Stroke::new(th, if on { SEG_ON } else { SEG_OFF }));
        }
        let dp = pt(1.0, 1.0) + vec2(gap * 0.5, 0.0);
        p.circle_filled(dp, th * 0.7, if dot { SEG_ON } else { SEG_OFF });
    }
}

// ---------------------------------------------------------------- the desk

/// What the user did on the desk this frame.
enum Act {
    On(Bus, usize, bool),
    Pan(Bus, usize, i32),
    Volume(Bus, usize, f32),
    Link(Bus, usize),
    Main(bool),
    Connect,
    Disconnect,
}

impl Panel {
    /// Draws the whole desk in the space left in `ui`.
    pub(crate) fn desk(&mut self, ui: &mut Ui) {
        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, Sense::hover());
        let (w, h) = (desk_w(), DESK_H);
        let s = (avail.width() / w).min(avail.height() / h).max(0.3);
        let o = avail.center() - vec2(w, h) * s / 2.0;
        let g = Geo { o, s };
        let base = ui.scope_id().with("desk");
        let p = ui.painter().clone();
        plate(&p, g.r(0.0, 0.0, w, h), s);

        let total: usize = SECTIONS.iter().map(|b| b.channels() as usize).sum();
        self.holds.resize(total, Hold::default());

        let daemon_meters = self.using_daemon() && self.daemon_connected;
        let mut acts = Vec::new();
        let strips_top = MARGIN + HEADER_H;
        let mut left = MARGIN;
        let mut global = 0;
        for bus in SECTIONS {
            let n = bus.channels() as usize;
            let i = bus.index() as usize;
            let width = n as f32 * STRIP_W;
            // Header and rule.
            let title = match bus {
                Bus::Input => "INPUT MONITOR",
                Bus::Output => "OUTPUT MONITOR",
                Bus::MonitorOut => "MONITOR OUT",
            };
            p.text(
                g.p(left + width / 2.0, MARGIN + 13.0),
                Align2::CENTER_CENTER,
                title,
                g.font(11.5),
                LEGEND,
            );
            p.hline(
                g.p(left + 4.0, 0.0).x..=g.p(left + width - 4.0, 0.0).x,
                g.p(0.0, MARGIN + 26.0).y,
                Stroke::new(1.5 * s, RULE),
            );
            // Strip dividers.
            for k in 1..n {
                let x = left + k as f32 * STRIP_W;
                p.vline(
                    g.p(x, 0.0).x,
                    g.p(0.0, strips_top).y..=g.p(0.0, strips_top + STRIP_H).y,
                    Stroke::new(1.0, Color32::from_white_alpha(22)),
                );
            }

            for ch in 0..n {
                let st = self.mixer.strips[i][ch];
                let cx = left + ch as f32 * STRIP_W + STRIP_W / 2.0;
                let id = base.with((i, ch));
                let mute_hover = match bus {
                    Bus::Output => "M: take this output out of MONITOR OUT",
                    _ => "M: mute this channel in the monitor mix",
                };
                if key(
                    ui,
                    g.r(cx - 10.0, strips_top + 6.0, 20.0, 18.0),
                    id.with("m"),
                    "M",
                    (!st.on).then_some(MUTE_LIT),
                    true,
                    s,
                )
                .on_hover_text(mute_hover)
                .clicked()
                {
                    acts.push(Act::On(bus, ch, !st.on));
                }
                if bus != Bus::MonitorOut {
                    key(
                        ui,
                        g.r(cx - 10.0, strips_top + 30.0, 20.0, 18.0),
                        id.with("s"),
                        "S",
                        None,
                        false,
                        s,
                    )
                    .on_hover_text("SOLO: its address on the unit is not identified yet");
                    let mut pan = st.pan;
                    knob(ui, g, g.p(cx, strips_top + 68.0), id.with("pan"), &mut pan)
                        .on_hover_text(format!(
                            "PAN {}: drag, scroll, double-click to centre",
                            match pan {
                                0 => "C".to_string(),
                                v if v < 0 => format!("L{}", -v),
                                v => format!("R{v}"),
                            }
                        ));
                    if pan != st.pan {
                        acts.push(Act::Pan(bus, ch, pan));
                    }
                }
                let mut db = st.volume_db;
                fader(
                    ui,
                    g,
                    cx - 4.0,
                    strips_top + FADER_TOP,
                    id.with("fader"),
                    bus,
                    &mut db,
                )
                .on_hover_text(format!(
                    "VOLUME {} dB: drag the cap, scroll, double-click for 0 dB",
                    format_db(db)
                ));
                if (db - st.volume_db).abs() > f32::EPSILON {
                    acts.push(Act::Volume(bus, ch, db));
                }
                let level = match bus {
                    Bus::Input if daemon_meters => Some(self.levels_in.get(ch).copied()),
                    Bus::Output if daemon_meters => Some(self.levels_out.get(ch).copied()),
                    Bus::Input => self.meters.as_ref().map(|m| Some(m.level(ch))),
                    _ => None,
                }
                .map(|l| l.unwrap_or(0.0));
                meter(
                    ui,
                    g,
                    cx + 9.0,
                    strips_top + FADER_TOP,
                    level,
                    &mut self.holds[global + ch],
                );
                let name = match bus {
                    Bus::MonitorOut => ["L", "R"][ch.min(1)].to_string(),
                    _ => format!("{}", ch + 1),
                };
                p.text(
                    g.p(cx, strips_top + STRIP_H - 10.0),
                    Align2::CENTER_CENTER,
                    name,
                    g.font(11.0),
                    LEGEND,
                );
            }
            for (pair, &linked) in self.mixer.links[i].iter().take(n / 2).enumerate() {
                let x = left + pair as f32 * 2.0 * STRIP_W;
                if key(
                    ui,
                    g.r(
                        x + 5.0,
                        strips_top + FADER_TOP + FADER_H + 20.0,
                        2.0 * STRIP_W - 10.0,
                        17.0,
                    ),
                    base.with((i, "link", pair)),
                    "LINK",
                    linked.then_some(LINK_LIT),
                    true,
                    s,
                )
                .on_hover_text("Stereo link: fader and M move both channels of the pair")
                .clicked()
                {
                    acts.push(Act::Link(bus, pair));
                }
            }
            left += width + GAP;
            global += n;
        }

        // Master section.
        let mx = left;
        p.vline(
            g.p(mx - GAP / 2.0, 0.0).x,
            g.p(0.0, MARGIN).y..=g.p(0.0, strips_top + STRIP_H).y,
            Stroke::new(1.5 * s, RULE),
        );
        self.master(ui, g, mx, base, &mut acts);

        for a in acts {
            self.apply_act(a);
        }
    }

    fn master(&mut self, ui: &mut Ui, g: Geo, mx: f32, base: Id, acts: &mut Vec<Act>) {
        let s = g.s;
        let p = ui.painter().clone();
        let cx = mx + MASTER_W / 2.0;
        let top = MARGIN;
        ui.put(
            g.r(mx, top, MASTER_W, 26.0),
            egui::Label::new(
                RichText::new("UA-101")
                    .italics()
                    .size(21.0 * s)
                    .color(LEGEND),
            ),
        );
        ui.put(
            g.r(mx, top + 24.0, MASTER_W, 16.0),
            egui::Label::new(
                RichText::new("Control Panel")
                    .italics()
                    .size(11.5 * s)
                    .color(LEGEND_DIM),
            ),
        );

        // Sample rate.
        let rate = self
            .daemon_rate
            .filter(|_| self.using_daemon())
            .or_else(|| self.device.as_ref().map(|d| d.sample_rate()));
        let text = rate
            .map(|r| format!("{:.1}", r as f64 / 1000.0))
            .unwrap_or_else(|| "--.-".into());
        let screen = g.r(cx - 52.0, top + 48.0, 104.0, 44.0);
        p.rect_filled(
            screen.expand(2.0 * s),
            4.0 * s,
            Color32::from_rgb(0x30, 0x33, 0x3c),
        );
        p.rect_filled(screen, 3.0 * s, Color32::from_rgb(0x0d, 0x05, 0x06));
        seven_seg(&p, screen, &text);
        ui.interact(screen, base.with("rate"), Sense::hover())
            .on_hover_text("Sample rate, set by the switch on the unit's rear panel");
        p.text(
            g.p(cx, top + 102.0),
            Align2::CENTER_CENTER,
            "kHz",
            g.font(10.5),
            LEGEND,
        );

        let heading = |y: f32, t: &str| {
            p.text(
                g.p(cx, top + y),
                Align2::CENTER_CENTER,
                t,
                g.font(10.5),
                LEGEND,
            );
        };
        let caption = |x: f32, y: f32, t: &str| {
            p.text(
                g.p(x, top + y),
                Align2::CENTER_CENTER,
                t,
                g.font(8.5),
                LEGEND,
            );
        };
        let unknown = "Unknown until the switch moves: the unit reports changes only";

        heading(128.0, "CLOCK");
        let clock = self.front.clock.map(|c| c == 0);
        for (k, (label, state)) in [("INT", clock), ("DIGITAL", clock.map(|c| !c))]
            .into_iter()
            .enumerate()
        {
            let x = cx - 28.0 + k as f32 * 56.0;
            let r = lamp(
                ui,
                g.r(x - 11.0, top + 140.0, 22.0, 16.0),
                base.with(("clock", k)),
                state,
                s,
            );
            if state.is_none() {
                r.on_hover_text(unknown);
            }
            caption(x, 166.0, label);
        }

        heading(190.0, "DIRECT MONITOR");
        let soft_hover = if self.front.soft_ctrl == Some(false) {
            "SOFT CTRL is off: the unit ignores the monitor mixer from the computer"
        } else {
            "SOFT CTRL: the direct monitor follows the INPUT MONITOR mixer"
        };
        for (k, (label, state, hover)) in [
            ("MONO", self.front.mono, "MONO: the direct monitor in mono"),
            ("SOFT", self.front.soft_ctrl, soft_hover),
        ]
        .into_iter()
        .enumerate()
        {
            let x = cx - 28.0 + k as f32 * 56.0;
            lamp(
                ui,
                g.r(x - 11.0, top + 202.0, 22.0, 16.0),
                base.with(("dm", k)),
                state,
                s,
            )
            .on_hover_text(if state.is_none() { unknown } else { hover });
            caption(x, 228.0, label);
        }

        heading(252.0, "WAVE TO MAIN");
        for (k, (label, on, hover)) in [
            ("ALL", true, "Send every output to MONITOR OUT"),
            ("NONE", false, "Keep every output out of MONITOR OUT"),
        ]
        .into_iter()
        .enumerate()
        {
            let x = cx - 28.0 + k as f32 * 56.0;
            if key(
                ui,
                g.r(x - 20.0, top + 264.0, 40.0, 17.0),
                base.with(("main", k)),
                label,
                None,
                true,
                s,
            )
            .on_hover_text(hover)
            .clicked()
            {
                acts.push(Act::Main(on));
            }
        }

        let (status, color) = if self.using_daemon() {
            if self.daemon_connected {
                ("UA101D · CONNECTED", OK)
            } else {
                ("UA101D · NO DEVICE", WARN)
            }
        } else if self.link.is_some() {
            ("CONNECTED", OK)
        } else if self.device.is_some() {
            ("NOT CONNECTED", WARN)
        } else {
            ("NO DEVICE", BAD)
        };
        p.text(
            g.p(cx, top + 304.0),
            Align2::CENTER_CENTER,
            status,
            g.font(9.0),
            color,
        );
        if self.meters.is_some() {
            p.text(
                g.p(cx, top + 318.0),
                Align2::CENTER_CENTER,
                "SIGNAL METERS ON",
                g.font(8.0),
                LEGEND_DIM,
            );
        }

        let button = |y: f32| g.r(cx - 58.0, top + y, 116.0, 22.0);
        if !self.using_daemon() {
            let connected = self.link.is_some();
            let r = key(
                ui,
                button(334.0),
                base.with("connect"),
                if connected { "DISCONNECT" } else { "CONNECT" },
                connected.then_some(LINK_LIT),
                self.device.is_some(),
                s,
            )
            .on_hover_text(
                "Takes the MIDI interface. On Linux this detaches snd-ua101 \
                 (the card leaves PipeWire/ALSA) until you disconnect.",
            );
            if r.clicked() {
                acts.push(if connected {
                    Act::Disconnect
                } else {
                    Act::Connect
                });
            }
        }
        if key(
            ui,
            button(364.0),
            base.with("patch"),
            "PATCH BAY",
            None,
            true,
            s,
        )
        .clicked()
        {
            self.dialogs.patch_bay = true;
        }
        if key(
            ui,
            button(394.0),
            base.with("memory"),
            "MEMORY",
            None,
            true,
            s,
        )
        .clicked()
        {
            self.dialogs.memory = true;
        }
        let device = key(
            ui,
            button(424.0),
            base.with("device"),
            "DEVICE",
            None,
            true,
            s,
        );
        egui::Popup::menu(&device).show(|ui| self.device_menu(ui));
    }

    fn device_menu(&mut self, ui: &mut Ui) {
        for (flag, label) in [
            (&mut self.dialogs.patch_bay, "Patch Bay…"),
            (&mut self.dialogs.memory, "Load / Store Memory of Device…"),
            (&mut self.dialogs.reset, "Reset Device…"),
            (&mut self.dialogs.tone, "Test tone…"),
        ] {
            if ui.button(label).clicked() {
                *flag = true;
            }
        }
        ui.separator();
        let meters_label = if self.meters.is_some() {
            "Stop SIGNAL meters"
        } else {
            "Start SIGNAL meters"
        };
        if ui
            .add_enabled(
                cfg!(target_os = "linux") && self.device.is_some(),
                egui::Button::new(meters_label),
            )
            .clicked()
        {
            self.toggle_meters();
        }
        ui.separator();
        let mut pick = None;
        for i in 0..self.devices.len() {
            if ui
                .radio(self.selected == i, format!("UA-101 #{}", i + 1))
                .clicked()
            {
                pick = Some(i);
            }
        }
        if ui.button("Rescan").clicked() {
            pick = Some(self.selected);
        }
        if let Some(i) = pick {
            self.selected = i;
            self.detect();
        }
        ui.separator();
        if ui.button("Reset panel").clicked() {
            self.mixer = crate::Mixer::new();
        }
        if ui.button("Developer: SysEx console…").clicked() {
            self.dialogs.developer = true;
        }
        if ui
            .checkbox(&mut self.always_on_top, "Always on top")
            .changed()
        {
            let level = if self.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            };
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
        if ui.button("About…").clicked() {
            self.dialogs.about = true;
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Channels a strip change applies to: the strip, and its partner when
    /// the pair is linked.
    fn targets(&self, bus: Bus, ch: usize) -> Vec<usize> {
        let i = bus.index() as usize;
        let mut v = vec![ch];
        let partner = ch ^ 1;
        if self.mixer.links[i].get(ch / 2).copied().unwrap_or(false)
            && partner < self.mixer.strips[i].len()
        {
            v.push(partner);
        }
        v
    }

    fn apply_act(&mut self, a: Act) {
        use ua101_usb::protocol::mixer;
        match a {
            Act::On(bus, ch, on) => {
                for t in self.targets(bus, ch) {
                    self.mixer.strips[bus.index() as usize][t].on = on;
                    self.send(mixer::set_mute(bus, t as u8, !on));
                }
            }
            // Pan stays per channel when linked.
            Act::Pan(bus, ch, pan) => {
                self.mixer.strips[bus.index() as usize][ch].pan = pan;
                self.send(mixer::set_pan(bus, ch as u8, crate::pan_raw(pan)));
            }
            Act::Volume(bus, ch, db) => {
                for t in self.targets(bus, ch) {
                    self.mixer.strips[bus.index() as usize][t].volume_db = db;
                    self.send(mixer::set_volume(bus, t as u8, crate::volume_raw(db)));
                }
            }
            Act::Link(bus, pair) => {
                let l = &mut self.mixer.links[bus.index() as usize][pair];
                *l = !*l;
                let on = *l;
                self.send(mixer::set_link(bus, pair as u8, on));
            }
            Act::Main(on) => self.set_all_main(on),
            Act::Connect => self.connect(),
            Act::Disconnect => {
                self.disconnect();
                self.status = "Disconnected; the card is back with the OS driver.".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taper_round_trips() {
        for db in [6.0, 3.0, 0.0, -3.0, -6.0, -15.0, -33.0, -59.0, -60.0] {
            assert!((t_to_db(db_to_t(db)) - db).abs() < 1e-3, "{db}");
        }
        assert_eq!(db_to_t(FADER_MAX_DB), 0.0);
        assert_eq!(db_to_t(FADER_MIN_DB), 1.0);
        assert_eq!(db_to_t(-100.0), 1.0);
    }

    #[test]
    fn meter_colours_span_the_scale() {
        assert_eq!(meter_color(0.0), METER[0].1);
        assert_eq!(meter_color(1.0), METER[2].1);
    }
}
