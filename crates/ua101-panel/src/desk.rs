//! The mixer drawn as one console: a header bar with the unit's state and
//! the device buttons, then one card per mixer section. Each strip has its
//! channel tab, MUTE and SOLO pills, a pan bar, an LED ladder meter beside
//! a dB-scaled fader, a dB read-out, and a link button between the two
//! channels of each pair. Everything is laid out in desk units and scaled
//! to the window, so the console keeps its proportions at any size.

use eframe::egui::{
    self, Align2, Color32, FontId, Id, Mesh, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use ua101_usb::protocol::mixer::Bus;

use crate::{FADER_MAX_DB, FADER_MIN_DB, Panel, format_db, theme};

// ---------------------------------------------------------------- layout

const MARGIN: f32 = 14.0;
const STRIP_W: f32 = 44.0;
/// Inner padding of a section card.
const PAD: f32 = 6.0;
const SECTION_GAP: f32 = 12.0;
const HEADER_H: f32 = 58.0;
const CARD_TOP: f32 = MARGIN + HEADER_H + 10.0;
/// Title band of a section card.
const TITLE_H: f32 = 26.0;
/// From the top of a strip: the fader travel.
const FADER_TOP: f32 = 100.0;
const FADER_H: f32 = 236.0;
const STRIP_H: f32 = FADER_TOP + FADER_H + 54.0;
const CARD_H: f32 = TITLE_H + STRIP_H + 6.0;
const DESK_H: f32 = CARD_TOP + CARD_H + MARGIN;
const SECTIONS: [Bus; 3] = [Bus::Input, Bus::Output, Bus::MonitorOut];
/// First capture channel of the monitor mix (MAIN), 0-based: capture
/// channels 11-12 follow the mix (observed, see docs/protocol.md).
const MAIN_CAPTURE: usize = 10;
/// Segments of the LED ladder meter, -60 to 0 dBFS.
const LADDER: usize = 24;

fn desk_w() -> f32 {
    let strips: f32 = SECTIONS.iter().map(|b| b.channels() as f32).sum();
    2.0 * MARGIN
        + strips * STRIP_W
        + SECTIONS.len() as f32 * 2.0 * PAD
        + (SECTIONS.len() - 1) as f32 * SECTION_GAP
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
    fn mono(&self, size: f32) -> FontId {
        FontId::monospace(size * self.s)
    }
}

// ---------------------------------------------------------------- palette

const SURFACE_TOP: Color32 = Color32::from_rgb(0x1d, 0x20, 0x25);
const SURFACE_BOTTOM: Color32 = Color32::from_rgb(0x14, 0x16, 0x1a);
const CARD: Color32 = theme::CARD;
const BORDER: Color32 = theme::BORDER;
const TEXT: Color32 = theme::TEXT;
const DIM: Color32 = theme::TEXT_DIM;
const CONTROL: Color32 = Color32::from_rgb(0x2d, 0x31, 0x38);
const CONTROL_EDGE: Color32 = Color32::from_rgb(0x3b, 0x40, 0x49);
const INSET: Color32 = Color32::from_rgb(0x0f, 0x11, 0x14);
const LED_OFF: Color32 = Color32::from_rgb(0x41, 0x46, 0x4e);
const MUTE_LED: Color32 = Color32::from_rgb(0xff, 0x4d, 0x4d);
const LCD_BG: Color32 = Color32::from_rgb(0x0b, 0x16, 0x10);
const LCD_EDGE: Color32 = Color32::from_rgb(0x1e, 0x35, 0x28);
const LCD_FG: Color32 = Color32::from_rgb(0x6c, 0xf0, 0xa0);
const OK: Color32 = theme::GREEN;
const WARN: Color32 = theme::YELLOW;
const BAD: Color32 = Color32::from_rgb(0xf0, 0x55, 0x4a);
const METER_LOW: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
const METER_MID: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
const METER_HIGH: Color32 = Color32::from_rgb(0xff, 0x4d, 0x4d);

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
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

/// Colour of ladder segment `i` (0 = bottom): green, yellow from -18 dBFS,
/// red from -6 dBFS.
fn segment_color(i: usize) -> Color32 {
    let top_db = -60.0 + 60.0 * (i + 1) as f32 / LADDER as f32;
    if top_db > -6.0 {
        METER_HIGH
    } else if top_db > -18.0 {
        METER_MID
    } else {
        METER_LOW
    }
}

/// Fraction of the ladder lit for a linear level.
fn level_frac(l: f32) -> f32 {
    if l > 0.0 {
        ((20.0 * l.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// ---------------------------------------------------------------- widgets

/// A titled group of header LEDs: (label, state, colour when on, tooltip).
type LedGroup<'a> = (&'a str, f32, [(&'a str, Option<bool>, Color32, &'a str); 2]);

/// Peak hold and clip light of one meter.
#[derive(Clone, Copy, Default)]
pub struct Hold {
    peak: f32,
    peak_at: f64,
    clip_at: Option<f64>,
}

const PEAK_HOLD_S: f64 = 1.5;
const CLIP_HOLD_S: f64 = 2.0;

fn led(p: &Painter, c: Pos2, r: f32, on: Option<Color32>) {
    match on {
        Some(color) => {
            p.circle_filled(c, r * 2.0, color.gamma_multiply(0.18));
            p.circle_filled(c, r, color);
        }
        None => {
            p.circle_filled(c, r, LED_OFF);
        }
    }
}

/// A rounded pill with an LED, such as MUTE. `lit` is the LED colour when
/// on; a disabled pill only shows its tooltip.
fn pill(
    ui: &Ui,
    g: Geo,
    rect: Rect,
    id: Id,
    label: &str,
    lit: Option<Color32>,
    enabled: bool,
) -> Response {
    let s = g.s;
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
    let radius = rect.height() / 2.0;
    let mut body = CONTROL;
    if enabled && r.hovered() {
        body = lerp(body, Color32::WHITE, 0.08);
    }
    if let (Some(c), true) = (lit, enabled) {
        body = lerp(body, c, 0.18);
    }
    p.rect_filled(rect, radius, body);
    p.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, CONTROL_EDGE),
        StrokeKind::Inside,
    );
    let alpha = if enabled { 1.0 } else { 0.35 };
    led(
        p,
        pos2(rect.left() + radius + 1.0 * s, rect.center().y),
        2.4 * s,
        lit.filter(|_| enabled),
    );
    let color = if lit.is_some() && enabled { TEXT } else { DIM };
    p.text(
        pos2(rect.center().x + 4.5 * s, rect.center().y),
        Align2::CENTER_CENTER,
        label,
        g.font(7.5),
        color.gamma_multiply(alpha),
    );
    r
}

/// A header button; `lit` fills it with that colour.
fn button(
    ui: &Ui,
    g: Geo,
    rect: Rect,
    id: Id,
    label: &str,
    lit: Option<Color32>,
    enabled: bool,
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
    let mut body = lit.map_or(CONTROL, |c| lerp(CONTROL, c, 0.55));
    if enabled && r.hovered() {
        body = lerp(body, Color32::WHITE, 0.08);
    }
    p.rect_filled(rect, 6.0 * g.s, body);
    p.rect_stroke(
        rect,
        6.0 * g.s,
        Stroke::new(1.0, CONTROL_EDGE),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        g.font(8.5),
        if enabled {
            TEXT
        } else {
            DIM.gamma_multiply(0.5)
        },
    );
    r
}

/// Horizontal pan bar centred on `c`: drag sideways, scroll, double-click
/// to centre. The fill grows from the centre in the section's colour.
fn pan_bar(ui: &Ui, g: Geo, c: Pos2, id: Id, pan: &mut i32, accent: Color32) -> Response {
    let s = g.s;
    let half = 16.0 * s;
    let r = ui.interact(
        Rect::from_center_size(c, vec2(half * 2.0 + 8.0 * s, 14.0 * s)),
        id,
        Sense::click_and_drag(),
    );
    let acc_id = id.with("acc");
    if r.drag_started() {
        ui.data_mut(|d| d.insert_temp(acc_id, *pan as f32));
    }
    if r.dragged() {
        let dx = r.drag_delta().x / (half * 2.0) * 127.0;
        let acc = ui.data_mut(|m| {
            let a = m.get_temp_mut_or(acc_id, *pan as f32);
            *a = (*a + dx).clamp(-64.0, 63.0);
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
    let track = Rect::from_center_size(c, vec2(half * 2.0, 4.0 * s));
    p.rect_filled(track, 2.0 * s, INSET);
    let frac = if *pan < 0 {
        *pan as f32 / 64.0
    } else {
        *pan as f32 / 63.0
    };
    let x = c.x + frac * half;
    let fill = Rect::from_x_y_ranges(c.x.min(x)..=c.x.max(x), track.y_range());
    p.rect_filled(fill, 2.0 * s, accent);
    p.vline(
        c.x,
        (c.y - 5.0 * s)..=(c.y + 5.0 * s),
        Stroke::new(1.0, DIM),
    );
    p.circle_filled(pos2(x, c.y), 4.5 * s, TEXT);
    p.circle_stroke(pos2(x, c.y), 4.5 * s, Stroke::new(1.0, INSET));
    r
}

/// Fader on the track at `x`, with its scale on the right: drag the cap,
/// scroll over the travel, double-click for 0 dB.
fn fader(ui: &Ui, g: Geo, x: f32, top: f32, id: Id, accent: Color32, db: &mut f32) -> Response {
    let s = g.s;
    let p = ui.painter();
    let f = g.font(7.0);
    for &(d, t) in &TAPER {
        let y = top + t * FADER_H;
        let label = if d <= FADER_MIN_DB {
            "-∞".to_string()
        } else if d > 0.0 {
            format!("+{}", d as i32)
        } else {
            format!("{}", d as i32)
        };
        p.text(g.p(x + 6.0, y), Align2::LEFT_CENTER, label, f.clone(), DIM);
    }
    let track = g.r(x - 1.5, top - 4.0, 3.0, FADER_H + 8.0);
    p.rect_filled(track, 1.5 * s, INSET);

    let cap_at = |db: f32| g.r(x - 10.0, top + db_to_t(db) * FADER_H - 7.0, 20.0, 14.0);
    let travel = ui.interact(
        g.r(x - 12.0, top - 8.0, 24.0, FADER_H + 16.0),
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
    p.rect_filled(
        cap.translate(vec2(0.0, 2.0) * s),
        3.0 * s,
        Color32::from_black_alpha(110),
    );
    let (top_c, bottom_c) = if r.hovered() || r.dragged() {
        (
            Color32::from_rgb(0x55, 0x5b, 0x66),
            Color32::from_rgb(0x3a, 0x3f, 0x48),
        )
    } else {
        (
            Color32::from_rgb(0x4a, 0x50, 0x5a),
            Color32::from_rgb(0x30, 0x34, 0x3c),
        )
    };
    p.rect_filled(cap, 3.0 * s, bottom_c);
    vgrad(p, cap.shrink(1.5 * s), top_c, bottom_c);
    p.hline(
        cap.x_range().shrink(3.0 * s),
        cap.center().y,
        Stroke::new(2.0 * s, accent),
    );
    p.rect_stroke(
        cap,
        3.0 * s,
        Stroke::new(1.0, Color32::from_black_alpha(160)),
        StrokeKind::Inside,
    );
    r.union(travel)
}

/// LED ladder from -60 dBFS (bottom) to 0 dBFS (top), with a held peak
/// segment and a clip LED. `level` is linear, 0.0–1.0; `None` when the
/// level of this strip is not measured.
fn ladder(ui: &Ui, g: Geo, x: f32, top: f32, level: Option<f32>, hold: &mut Hold) {
    let s = g.s;
    let p = ui.painter();
    let gap = 1.5;
    let seg_h = (FADER_H - (LADDER - 1) as f32 * gap) / LADDER as f32;
    let clip = g.r(x, top - 9.0, 5.0, 5.0);
    let seg = |i: usize| {
        g.r(
            x,
            top + FADER_H - (i + 1) as f32 * (seg_h + gap) + gap,
            5.0,
            seg_h,
        )
    };
    let Some(level) = level else {
        for i in 0..LADDER {
            p.rect_filled(seg(i), 1.0 * s, Color32::from_rgb(0x26, 0x29, 0x2f));
        }
        p.rect_filled(clip, 1.0 * s, Color32::from_rgb(0x26, 0x29, 0x2f));
        return;
    };
    let now = ui.input(|i| i.time);
    if level >= hold.peak || now - hold.peak_at > PEAK_HOLD_S {
        hold.peak = level;
        hold.peak_at = now;
    }
    if level >= 0.999 {
        hold.clip_at = Some(now);
    }
    let lit = (level_frac(level) * LADDER as f32).round() as usize;
    let peak = (level_frac(hold.peak) * LADDER as f32).round() as usize;
    for i in 0..LADDER {
        let c = segment_color(i);
        let on = i < lit || (peak > 0 && i == peak - 1);
        p.rect_filled(seg(i), 1.0 * s, if on { c } else { lerp(INSET, c, 0.16) });
    }
    let clipping = hold.clip_at.is_some_and(|t| now - t < CLIP_HOLD_S);
    p.rect_filled(
        clip,
        1.0 * s,
        if clipping {
            METER_HIGH
        } else {
            lerp(INSET, METER_HIGH, 0.16)
        },
    );
    if peak > 0 || clipping {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Link button centred on `c`: two chain links, filled with the section's
/// colour when the pair is linked.
fn link_button(ui: &Ui, g: Geo, c: Pos2, id: Id, linked: bool, accent: Color32) -> Response {
    let s = g.s;
    let rad = 9.0 * s;
    let r = ui.interact(
        Rect::from_center_size(c, vec2(rad, rad) * 2.0),
        id,
        Sense::click(),
    );
    let p = ui.painter();
    let mut body = if linked { accent } else { CONTROL };
    if r.hovered() {
        body = lerp(body, Color32::WHITE, 0.12);
    }
    p.circle_filled(c, rad, body);
    p.circle_stroke(c, rad, Stroke::new(1.0, CONTROL_EDGE));
    let ink = if linked { INSET } else { DIM };
    for dx in [-2.6, 2.6] {
        let link = Rect::from_center_size(c + vec2(dx, 0.0) * s, vec2(7.5, 4.6) * s);
        p.rect_stroke(link, 2.3 * s, Stroke::new(1.4 * s, ink), StrokeKind::Middle);
    }
    r
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
    /// Draws the whole console in the space left in `ui`.
    pub(crate) fn desk(&mut self, ui: &mut Ui) {
        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, Sense::hover());
        let (w, h) = (desk_w(), DESK_H);
        let s = (avail.width() / w).min(avail.height() / h).max(0.3);
        let o = avail.center() - vec2(w, h) * s / 2.0;
        let g = Geo { o, s };
        let base = ui.scope_id().with("desk");
        let p = ui.painter().clone();
        let whole = g.r(0.0, 0.0, w, h);
        p.rect_filled(whole, 12.0 * s, SURFACE_BOTTOM);
        vgrad(&p, whole.shrink(6.0 * s), SURFACE_TOP, SURFACE_BOTTOM);

        let mut acts = Vec::new();
        self.header(ui, g, w, base, &mut acts);

        let total: usize = SECTIONS.iter().map(|b| b.channels() as usize).sum();
        self.holds.resize(total, Hold::default());
        let daemon_meters = self.using_daemon() && self.daemon_connected;
        let mut left = MARGIN;
        let mut global = 0;
        for bus in SECTIONS {
            let n = bus.channels() as usize;
            let i = bus.index() as usize;
            let accent = theme::section_color(bus);
            let width = n as f32 * STRIP_W + 2.0 * PAD;
            let card = g.r(left, CARD_TOP, width, CARD_H);
            p.rect_filled(card, 10.0 * s, CARD);
            p.rect_stroke(card, 10.0 * s, Stroke::new(1.0, BORDER), StrokeKind::Inside);
            p.rect_filled(g.r(left + 10.0, CARD_TOP + 8.0, 3.0, 11.0), 1.5 * s, accent);
            let title = match bus {
                Bus::Input => "INPUT MONITOR",
                Bus::Output => "OUTPUT MONITOR",
                Bus::MonitorOut => "MONITOR OUT",
            };
            p.text(
                g.p(left + 18.0, CARD_TOP + 13.5),
                Align2::LEFT_CENTER,
                title,
                g.font(9.5),
                TEXT,
            );
            if bus == Bus::Output {
                for (k, (label, on, hover)) in [
                    ("ALL TO MAIN", true, "Send every output to MONITOR OUT"),
                    ("NONE", false, "Keep every output out of MONITOR OUT"),
                ]
                .into_iter()
                .enumerate()
                {
                    let (bx, bw) = if k == 0 {
                        (left + width - 118.0, 66.0)
                    } else {
                        (left + width - 48.0, 40.0)
                    };
                    if button(
                        ui,
                        g,
                        g.r(bx, CARD_TOP + 6.0, bw, 15.0),
                        base.with(("main", k)),
                        label,
                        None,
                        true,
                    )
                    .on_hover_text(hover)
                    .clicked()
                    {
                        acts.push(Act::Main(on));
                    }
                }
            }

            let top = CARD_TOP + TITLE_H;
            let x0 = left + PAD;
            for ch in 0..n {
                let st = self.mixer.strips[i][ch];
                let cx = x0 + ch as f32 * STRIP_W + STRIP_W / 2.0;
                let id = base.with((i, ch));
                if ch > 0 {
                    p.vline(
                        g.p(x0 + ch as f32 * STRIP_W, 0.0).x,
                        g.p(0.0, top + 2.0).y..=g.p(0.0, top + STRIP_H - 4.0).y,
                        Stroke::new(1.0, Color32::from_white_alpha(8)),
                    );
                }
                // Channel tab.
                let name = match bus {
                    Bus::MonitorOut => ["L", "R"][ch.min(1)].to_string(),
                    _ => format!("{}", ch + 1),
                };
                let tab = g.r(cx - 14.0, top, 28.0, 15.0);
                p.rect_filled(tab, 4.0 * s, accent.gamma_multiply(0.2));
                p.text(
                    tab.center(),
                    Align2::CENTER_CENTER,
                    name,
                    g.font(9.5),
                    accent,
                );

                let mute_hover = match bus {
                    Bus::Output => "MUTE: take this output out of MONITOR OUT",
                    _ => "MUTE: silence this channel in the monitor mix",
                };
                if pill(
                    ui,
                    g,
                    g.r(cx - 19.0, top + 21.0, 38.0, 15.0),
                    id.with("m"),
                    "MUTE",
                    (!st.on).then_some(MUTE_LED),
                    true,
                )
                .on_hover_text(mute_hover)
                .clicked()
                {
                    acts.push(Act::On(bus, ch, !st.on));
                }
                if bus != Bus::MonitorOut {
                    pill(
                        ui,
                        g,
                        g.r(cx - 19.0, top + 40.0, 38.0, 15.0),
                        id.with("s"),
                        "SOLO",
                        None,
                        false,
                    )
                    .on_hover_text("SOLO: its address on the unit is not identified yet");
                    let mut pan = st.pan;
                    let label = match pan {
                        0 => "C".to_string(),
                        v if v < 0 => format!("L{}", -v),
                        v => format!("R{v}"),
                    };
                    pan_bar(ui, g, g.p(cx, top + 66.0), id.with("pan"), &mut pan, accent)
                        .on_hover_text(format!(
                            "PAN {label}: drag sideways, scroll, double-click to centre"
                        ));
                    p.text(
                        g.p(cx, top + 79.0),
                        Align2::CENTER_CENTER,
                        label,
                        g.mono(7.5),
                        DIM,
                    );
                    if pan != st.pan {
                        acts.push(Act::Pan(bus, ch, pan));
                    }
                }

                let ftop = top + FADER_TOP;
                let level = match bus {
                    Bus::Input if daemon_meters => Some(self.levels_in.get(ch).copied()),
                    Bus::Output if daemon_meters => Some(self.levels_out.get(ch).copied()),
                    Bus::Input => self.meters.as_ref().map(|m| Some(m.level(ch))),
                    // Capture channels 11-12 carry the monitor mix (MAIN).
                    Bus::MonitorOut if daemon_meters => {
                        Some(self.levels_in.get(MAIN_CAPTURE + ch).copied())
                    }
                    Bus::MonitorOut => self
                        .meters
                        .as_ref()
                        .map(|m| Some(m.level(MAIN_CAPTURE + ch))),
                    Bus::Output => None,
                }
                .map(|l| l.unwrap_or(0.0));
                ladder(ui, g, cx - 17.0, ftop, level, &mut self.holds[global + ch]);
                let mut db = st.volume_db;
                fader(ui, g, cx - 2.0, ftop, id.with("fader"), accent, &mut db).on_hover_text(
                    format!(
                        "VOLUME {} dB: drag the cap, scroll, double-click for 0 dB",
                        format_db(db)
                    ),
                );
                if (db - st.volume_db).abs() > f32::EPSILON {
                    acts.push(Act::Volume(bus, ch, db));
                }
                let readout = g.r(cx - 17.0, ftop + FADER_H + 12.0, 34.0, 14.0);
                p.rect_filled(readout, 3.0 * s, INSET);
                p.text(
                    readout.center(),
                    Align2::CENTER_CENTER,
                    format_db(db),
                    g.mono(8.0),
                    if db > 0.05 { WARN } else { TEXT },
                );
            }
            for pair in 0..n / 2 {
                let linked = self.mixer.links[i].get(pair).copied().unwrap_or(false);
                let c = g.p(
                    x0 + (2 * pair + 1) as f32 * STRIP_W,
                    top + FADER_TOP + FADER_H + 40.0,
                );
                if link_button(ui, g, c, base.with((i, "link", pair)), linked, accent)
                    .on_hover_text("Stereo link: fader and MUTE move both channels of the pair")
                    .clicked()
                {
                    acts.push(Act::Link(bus, pair));
                }
            }
            left += width + SECTION_GAP;
            global += n;
        }

        for a in acts {
            self.apply_act(a);
        }
    }

    /// The header bar: name, sample rate, front-panel LEDs, connection
    /// status and the device buttons.
    fn header(&mut self, ui: &mut Ui, g: Geo, w: f32, base: Id, acts: &mut Vec<Act>) {
        let s = g.s;
        let p = ui.painter().clone();
        let bar = g.r(MARGIN, MARGIN, w - 2.0 * MARGIN, HEADER_H);
        p.rect_filled(bar, 10.0 * s, CARD);
        p.rect_stroke(bar, 10.0 * s, Stroke::new(1.0, BORDER), StrokeKind::Inside);
        let cy = MARGIN + HEADER_H / 2.0;
        let x0 = MARGIN + 16.0;

        p.text(
            g.p(x0, cy - 6.0),
            Align2::LEFT_CENTER,
            "UA-101",
            g.font(19.0),
            TEXT,
        );
        p.text(
            g.p(x0 + 1.0, cy + 12.0),
            Align2::LEFT_CENTER,
            "CONTROL PANEL",
            g.font(7.5),
            DIM,
        );

        // Sample rate on an LCD.
        let rate = self
            .daemon_rate
            .filter(|_| self.using_daemon())
            .or_else(|| self.device.as_ref().map(|d| d.sample_rate()));
        let lx = x0 + 104.0;
        p.text(
            g.p(lx, cy - 17.0),
            Align2::LEFT_CENTER,
            "SAMPLE RATE",
            g.font(6.5),
            DIM,
        );
        let lcd = g.r(lx, cy - 11.0, 92.0, 26.0);
        p.rect_filled(lcd, 4.0 * s, LCD_BG);
        p.rect_stroke(lcd, 4.0 * s, Stroke::new(1.0, LCD_EDGE), StrokeKind::Inside);
        let text = rate
            .map(|r| format!("{:.1}", r as f64 / 1000.0))
            .unwrap_or_else(|| "--.-".into());
        p.text(
            g.p(lx + 56.0, cy + 2.0),
            Align2::RIGHT_CENTER,
            text,
            g.mono(15.0),
            LCD_FG,
        );
        p.text(
            g.p(lx + 60.0, cy + 4.0),
            Align2::LEFT_CENTER,
            "kHz",
            g.font(8.0),
            LCD_FG.gamma_multiply(0.7),
        );
        ui.interact(lcd, base.with("rate"), Sense::hover())
            .on_hover_text("Set by the sample rate switch on the unit's rear panel");

        // Front-panel state, read from the unit.
        let unknown = "Unknown until the switch moves: the unit reports changes only";
        let clock = self.front.clock.map(|c| c == 0);
        let soft_hover = if self.front.soft_ctrl == Some(false) {
            "SOFT CTRL is off: the unit ignores the monitor mixer from the computer"
        } else {
            "SOFT CTRL: the direct monitor follows the INPUT MONITOR mixer"
        };
        let groups: [LedGroup; 2] = [
            (
                "CLOCK",
                lx + 112.0,
                [
                    ("INT", clock, OK, "Internal clock"),
                    (
                        "DIGITAL",
                        clock.map(|c| !c),
                        WARN,
                        "Clock from the digital input",
                    ),
                ],
            ),
            (
                "DIRECT MONITOR",
                lx + 222.0,
                [
                    (
                        "MONO",
                        self.front.mono,
                        WARN,
                        "MONO: the direct monitor in mono",
                    ),
                    ("SOFT CTRL", self.front.soft_ctrl, OK, soft_hover),
                ],
            ),
        ];
        for (title, gx, items) in groups {
            p.text(
                g.p(gx, cy - 17.0),
                Align2::LEFT_CENTER,
                title,
                g.font(6.5),
                DIM,
            );
            let mut x = gx;
            for (k, (label, state, color, hover)) in items.into_iter().enumerate() {
                let c = g.p(x + 4.0, cy + 2.0);
                led(&p, c, 3.6 * s, (state == Some(true)).then_some(color));
                let t = p.text(
                    g.p(x + 11.0, cy + 2.0),
                    Align2::LEFT_CENTER,
                    label,
                    g.font(8.0),
                    if state.is_some() { TEXT } else { DIM },
                );
                ui.interact(
                    Rect::from_min_max(g.p(x, cy - 6.0), pos2(t.right(), g.p(0.0, cy + 10.0).y)),
                    base.with((title, k)),
                    Sense::hover(),
                )
                .on_hover_text(if state.is_none() { unknown } else { hover });
                x += (t.width() / s) + 24.0;
            }
        }

        // Right side, from the right edge: DEVICE, MEMORY, PATCH BAY,
        // CONNECT, then the status.
        let mut rx = w - MARGIN - 12.0;
        let mut place = |width: f32| {
            rx -= width;
            let r = g.r(rx, cy - 11.0, width, 22.0);
            rx -= 8.0;
            r
        };
        let device_rect = place(70.0);
        let memory_rect = place(70.0);
        let patch_rect = place(84.0);
        let connect_rect = (!self.using_daemon()).then(|| place(90.0));
        let status_right = rx;

        let device = button(
            ui,
            g,
            device_rect,
            base.with("device"),
            "DEVICE",
            None,
            true,
        );
        egui::Popup::menu(&device).show(|ui| self.device_menu(ui));
        if button(
            ui,
            g,
            memory_rect,
            base.with("memory"),
            "MEMORY",
            None,
            true,
        )
        .clicked()
        {
            self.dialogs.memory = true;
        }
        if button(
            ui,
            g,
            patch_rect,
            base.with("patch"),
            "PATCH BAY",
            None,
            true,
        )
        .clicked()
        {
            self.dialogs.patch_bay = true;
        }
        if let Some(rect) = connect_rect {
            let connected = self.link.is_some();
            let r = button(
                ui,
                g,
                rect,
                base.with("connect"),
                if connected { "DISCONNECT" } else { "CONNECT" },
                (!connected).then_some(theme::BLUE),
                self.device.is_some(),
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

        let (status, color) = if self.using_daemon() {
            if self.daemon_connected {
                ("UA101D", OK)
            } else {
                ("UA101D · NO DEVICE", WARN)
            }
        } else if self.link.is_some() {
            ("CONNECTED", OK)
        } else if self.device.is_some() {
            ("OFFLINE", DIM)
        } else {
            ("NO DEVICE", BAD)
        };
        let galley = p.layout_no_wrap(status.into(), g.font(8.0), color);
        let pw = galley.size().x / s + 26.0;
        let pill_rect = g.r(status_right - pw - 4.0, cy - 9.0, pw, 18.0);
        p.rect_filled(pill_rect, 9.0 * s, color.gamma_multiply(0.14));
        p.rect_stroke(
            pill_rect,
            9.0 * s,
            Stroke::new(1.0, color.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        led(
            &p,
            pos2(pill_rect.left() + 10.0 * s, pill_rect.center().y),
            3.0 * s,
            Some(color),
        );
        p.galley(
            pos2(
                pill_rect.left() + 17.0 * s,
                pill_rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
        if self.meters.is_some() {
            p.text(
                pos2(pill_rect.center().x, pill_rect.bottom() + 6.0 * s),
                Align2::CENTER_CENTER,
                "SIGNAL METERS",
                g.font(6.0),
                DIM,
            );
        }
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
    fn ladder_colours_follow_the_level() {
        assert_eq!(segment_color(0), METER_LOW);
        assert_eq!(segment_color(LADDER - 1), METER_HIGH);
        assert_eq!(level_frac(1.0), 1.0);
        assert_eq!(level_frac(0.0), 0.0);
    }
}
