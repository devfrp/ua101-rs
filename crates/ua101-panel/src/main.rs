//! UA-101 control panel, laid out like Roland's original one: monitor
//! mixer strips, a device panel with the unit's front-panel state, patch
//! bay and device memory dialogs.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(unix)]
mod daemon;
mod link;
mod meters;
mod patchbay;
mod theme;

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui::{self, Color32, RichText, Vec2};
use link::{ControlLink, Event};
use meters::Meters;
use ua101_usb::{
    DeviceInfo, StreamConfig, Ua101, list_devices,
    protocol::{
        mixer::{self, Bus, FrontPanel, MemoryOp},
        pcm::S24_MAX,
        sysex,
    },
};

const STRIP_WIDTH: f32 = 40.0;
/// Fader range in dB. The bottom position means -inf (muted).
const FADER_MIN_DB: f32 = -60.0;
const FADER_MAX_DB: f32 = 6.0;
/// Device value for 0 dB: the level heard as normal on hardware.
const VOLUME_UNITY: f32 = mixer::VOLUME_UNITY as f32;

/// Converts a fader position in dB to the 24-bit device gain.
fn volume_raw(db: f32) -> u32 {
    if db <= FADER_MIN_DB {
        0
    } else {
        (VOLUME_UNITY * 10f32.powf(db / 20.0))
            .round()
            .clamp(0.0, mixer::VOLUME_MAX as f32) as u32
    }
}

/// Converts a 24-bit device gain to a fader position in dB.
fn volume_db(raw: u32) -> f32 {
    if raw == 0 {
        FADER_MIN_DB
    } else {
        (20.0 * (raw as f32 / VOLUME_UNITY).log10()).clamp(FADER_MIN_DB, FADER_MAX_DB)
    }
}

/// Pan control (-64 left … 0 centre … +63 right) to the 16-bit device value.
fn pan_raw(pan: i32) -> u16 {
    (mixer::PAN_CENTER as i32 + pan * 256).clamp(0, 0x7fff) as u16
}

fn pan_from_raw(raw: u16) -> i32 {
    ((raw as i32 - mixer::PAN_CENTER as i32) / 256).clamp(-64, 63)
}

fn format_db(db: f32) -> String {
    if db <= FADER_MIN_DB {
        "-∞".into()
    } else if db.abs() < 0.05 {
        "0.0".into()
    } else {
        format!("{db:+.1}")
    }
}
const LOG_LINES: usize = 200;

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UA-101 Control Panel")
            .with_inner_size([1320.0, 640.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "UA-101 Control Panel",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Panel::new(cc.egui_ctx.clone())))
        }),
    )
}

#[derive(Clone, Copy)]
struct Strip {
    /// Audible (not muted). For OUTPUT strips: sent to MONITOR OUT.
    on: bool,
    pan: i32,
    /// Fader position in dB (`FADER_MIN_DB` = -inf).
    volume_db: f32,
}

/// Local mirror of the mixer, indexed by [`Bus::index`]. The unit does not
/// report mixer values, so this starts from defaults.
struct Mixer {
    strips: [Vec<Strip>; 3],
    links: [Vec<bool>; 3],
}

impl Mixer {
    fn new() -> Self {
        // Same starting point as the daemon.
        let init = mixer::MixerState::initial();
        let mk = |b: Bus| -> Vec<Strip> {
            init.channels[b.index() as usize][..b.channels() as usize]
                .iter()
                .map(|c| Strip {
                    on: !c.mute,
                    pan: pan_from_raw(c.pan),
                    volume_db: volume_db(c.volume),
                })
                .collect()
        };
        let lk = |b: Bus| init.links[b.index() as usize][..b.channels() as usize / 2].to_vec();
        Self {
            strips: [mk(Bus::MonitorOut), mk(Bus::Input), mk(Bus::Output)],
            links: [lk(Bus::MonitorOut), lk(Bus::Input), lk(Bus::Output)],
        }
    }
}

struct Tone {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<String>,
}

#[derive(Default)]
struct Dialogs {
    patch_bay: bool,
    memory: bool,
    reset: bool,
    about: bool,
    developer: bool,
    tone: bool,
}

struct Panel {
    ctx: egui::Context,
    devices: Vec<DeviceInfo>,
    selected: usize,
    device: Option<Arc<Ua101>>,
    link: Option<ControlLink>,
    meters: Option<Meters>,
    front: FrontPanel,
    mixer: Mixer,
    patchbay: patchbay::PatchbayUi,
    dialogs: Dialogs,
    always_on_top: bool,
    status: String,
    log: VecDeque<String>,
    dev_address: String,
    dev_data: String,
    tone: Option<Tone>,
    tone_freq: f64,
    tone_db: f64,
    tone_output: usize,
    /// Connection to `ua101d`, when it runs. The panel then never takes
    /// the device itself.
    #[cfg(unix)]
    daemon: Option<daemon::DaemonLink>,
    daemon_connected: bool,
    daemon_rate: Option<u32>,
    levels_in: Vec<f32>,
    levels_out: Vec<f32>,
}

impl Panel {
    fn new(ctx: egui::Context) -> Self {
        let mut p = Self {
            ctx,
            devices: Vec::new(),
            selected: 0,
            device: None,
            link: None,
            meters: None,
            front: FrontPanel::default(),
            mixer: Mixer::new(),
            patchbay: patchbay::PatchbayUi::new(Bus::Output.channels() as usize),
            dialogs: Dialogs::default(),
            always_on_top: false,
            status: String::new(),
            log: VecDeque::new(),
            dev_address: "00 01 00 04".into(),
            dev_data: "01".into(),
            tone: None,
            tone_freq: 440.0,
            tone_db: -24.0,
            tone_output: 0,
            #[cfg(unix)]
            daemon: None,
            daemon_connected: false,
            daemon_rate: None,
            levels_in: Vec::new(),
            levels_out: Vec::new(),
        };
        p.detect();
        p
    }

    fn log(&mut self, line: String) {
        if self.log.len() >= LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    fn using_daemon(&self) -> bool {
        #[cfg(unix)]
        {
            self.daemon.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    fn has_device(&self) -> bool {
        self.device.is_some() || (self.using_daemon() && self.daemon_connected)
    }

    fn detect(&mut self) {
        self.disconnect();
        self.device = None;
        #[cfg(unix)]
        {
            if self.daemon.is_none() {
                self.daemon = daemon::DaemonLink::connect(self.ctx.clone());
            }
            if self.daemon.is_some() {
                self.status = "Connected to ua101d: the card stays available to the system.".into();
                return;
            }
        }
        match list_devices() {
            Ok(d) => self.devices = d,
            Err(e) => {
                self.status = format!("USB error: {e}");
                return;
            }
        }
        self.selected = self.selected.min(self.devices.len().saturating_sub(1));
        let Some(info) = self.devices.get(self.selected).cloned() else {
            self.status = "No UA-101 connected".into();
            return;
        };
        match Ua101::open(info) {
            Ok(d) => {
                self.device = Some(Arc::new(d));
                self.status = "UA-101 found. Press CONNECT to control the mixer.".into();
            }
            Err(e) => {
                self.status = format!("Cannot open the UA-101: {e} (Linux: install the udev rule)")
            }
        }
    }

    fn connect(&mut self) {
        let Some(dev) = &self.device else { return };
        match dev.open_midi() {
            Ok(port) => {
                self.link = Some(ControlLink::start(port, self.ctx.clone()));
                self.status =
                    "Connected. Move a switch on the unit to refresh the device panel.".into();
            }
            Err(e) => self.status = format!("Cannot open the control port: {e}"),
        }
    }

    fn disconnect(&mut self) {
        self.stop_tone();
        if let Some(m) = self.meters.take() {
            let _ = m.stop();
        }
        self.link = None;
    }

    fn send(&mut self, msg: sysex::Message) {
        #[cfg(unix)]
        if let Some(d) = &mut self.daemon {
            if let Ok(dt) = sysex::parse_dt1(msg.as_bytes()) {
                self.log
                    .push_back(format!("TX {} = {}", hex(&dt.address), hex(dt.data)));
                if !d.write(dt.address, dt.data) {
                    self.status = "Lost the connection to ua101d".into();
                    self.daemon = None;
                }
            }
            return;
        }
        match &self.link {
            Some(l) => l.send(msg),
            None => self.status = "Not connected: press CONNECT first.".into(),
        }
    }

    /// Sends a patch bay change to `ua101d`, which routes the audio.
    #[cfg_attr(not(unix), allow(unused_variables))]
    fn patch_command(&mut self, c: patchbay::PatchChange) {
        use patchbay::PatchChange;
        let line = match c {
            PatchChange::Enable(true) => "patch on".to_string(),
            PatchChange::Enable(false) => "patch off".to_string(),
            PatchChange::Clear => "patch clear".to_string(),
            PatchChange::Route(o, i) => format!("route {} {}", o + 1, i.map_or(0, |i| i + 1)),
        };
        self.log(format!("TX {line}"));
        #[cfg(unix)]
        if let Some(d) = &mut self.daemon
            && !d.command(&line)
        {
            self.status = "Lost the connection to ua101d".into();
            self.daemon = None;
        }
    }

    /// Updates the local mixer mirror from a parameter value.
    fn apply_state(&mut self, a: [u8; 4], d: &[u8]) {
        // Complete mixer block read from the unit.
        if a == mixer::BLOCK_ADDRESS {
            if let Some(m) = mixer::MixerState::from_block(d) {
                for bus in Bus::ALL {
                    let i = bus.index() as usize;
                    for (ch, st) in self.mixer.strips[i].iter_mut().enumerate() {
                        let c = m.channels[i][ch];
                        st.on = !c.mute;
                        st.pan = pan_from_raw(c.pan);
                        st.volume_db = volume_db(c.volume);
                    }
                    for (p, l) in self.mixer.links[i].iter_mut().enumerate() {
                        *l = m.links[i][p];
                    }
                }
            }
            return;
        }
        if a[0] != 0 || !(0x03..=0x05).contains(&a[1]) || d.is_empty() {
            return;
        }
        let bus = (a[1] - 0x03) as usize;
        let ch = a[2] as usize;
        match a[3] {
            mixer::slot::MUTE => {
                if let Some(s) = self.mixer.strips[bus].get_mut(ch) {
                    s.on = d[0] == 0;
                }
            }
            mixer::slot::LINK => {
                if let Some(l) = self.mixer.links[bus].get_mut(ch / 2) {
                    *l = d[0] != 0;
                }
            }
            mixer::slot::PAN if d.len() >= 4 => {
                if let Some(s) = self.mixer.strips[bus].get_mut(ch) {
                    s.pan = pan_from_raw(mixer::from_nibbles(&d[..4]) as u16);
                }
            }
            mixer::slot::VOLUME if d.len() >= 6 => {
                if let Some(s) = self.mixer.strips[bus].get_mut(ch) {
                    s.volume_db = volume_db(mixer::from_nibbles(&d[..6]));
                }
            }
            _ => {}
        }
    }

    #[cfg(unix)]
    fn poll_daemon(&mut self) {
        let mut events = Vec::new();
        if let Some(d) = &self.daemon {
            while let Ok(e) = d.events.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                daemon::DaemonEvent::Info { rate, connected } => {
                    self.daemon_connected = connected;
                    self.daemon_rate = rate;
                    self.status = if connected {
                        "ua101d: UA-101 running; the card is available to the system.".into()
                    } else {
                        "ua101d is running but no UA-101 is connected.".into()
                    };
                }
                daemon::DaemonEvent::Data(a, d) => {
                    self.log(format!("RX {} = {}", hex(&a), hex(&d)));
                    self.front.apply(a, &d);
                    self.apply_state(a, &d);
                }
                daemon::DaemonEvent::Levels { inputs, outputs } => {
                    let decay = |cur: &mut Vec<f32>, new: Vec<f32>| {
                        cur.resize(new.len(), 0.0);
                        for (c, n) in cur.iter_mut().zip(new) {
                            *c = n.max(*c * 0.8);
                        }
                    };
                    decay(&mut self.levels_in, inputs);
                    decay(&mut self.levels_out, outputs);
                }
                daemon::DaemonEvent::Patch { on, routes } => {
                    self.patchbay.set_state(on, &routes);
                }
                daemon::DaemonEvent::Error(e) => self.status = format!("ua101d: {e}"),
                daemon::DaemonEvent::Closed => {
                    self.daemon = None;
                    self.daemon_connected = false;
                    self.status = "ua101d stopped. Rescan to use the device directly.".into();
                }
            }
        }
    }

    fn poll_link(&mut self) {
        #[cfg(unix)]
        self.poll_daemon();
        let mut events = Vec::new();
        if let Some(l) = &self.link {
            while let Ok(e) = l.events.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                Event::Data(a, d) => {
                    self.log(format!("RX {} = {}", hex(&a), hex(&d)));
                    self.front.apply(a, &d);
                    self.apply_state(a, &d);
                }
                Event::Sent(m) => self.log(format!("TX {m:?}")),
                Event::Error(err) => {
                    self.status = format!("Connection lost: {err}");
                    self.link = None;
                }
            }
        }
        if self.link.as_ref().is_some_and(|l| !l.is_alive()) {
            self.link = None;
        }
    }

    fn toggle_meters(&mut self) {
        if let Some(m) = self.meters.take() {
            let err = m.stop();
            if !err.is_empty() {
                self.status = err;
            }
            return;
        }
        let Some(dev) = self.device.clone() else {
            return;
        };
        match Meters::start(&dev, self.ctx.clone()) {
            Ok(m) => self.meters = Some(m),
            Err(e) => self.status = format!("Cannot start the meters: {e}"),
        }
    }

    fn start_tone(&mut self) {
        let Some(dev) = self.device.clone() else {
            return;
        };
        if self.meters.is_some() {
            self.status = "Stop the SIGNAL meters before playing a tone.".into();
            return;
        }
        let mut stream = match dev.start_stream(StreamConfig::default()) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Cannot start audio: {e}");
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, freq, output) = (stop.clone(), self.tone_freq, self.tone_output);
        let amp = 10f64.powf(self.tone_db / 20.0).min(1.0) * S24_MAX as f64;
        let thread = thread::spawn(move || {
            let channels = stream.playback_format().channels as usize;
            let rate = stream.sample_rate() as f64;
            let mut n = 0u64;
            while !flag.load(Ordering::Relaxed) && stream.is_running() {
                let tx = stream.playback();
                let frames = tx.slots() / channels;
                if frames == 0 {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                for _ in 0..frames {
                    let s = (amp * (std::f64::consts::TAU * freq * n as f64 / rate).sin()) as i32;
                    for c in 0..channels {
                        let _ = tx.push(if output == 0 || output == c + 1 { s } else { 0 });
                    }
                    n += 1;
                }
            }
            match stream.stop() {
                Ok(st) => format!("Tone stopped ({} frames)", st.playback_frames),
                Err(e) => format!("Audio stopped: {e}"),
            }
        });
        self.tone = Some(Tone { stop, thread });
    }

    fn stop_tone(&mut self) {
        if let Some(t) = self.tone.take() {
            t.stop.store(true, Ordering::Relaxed);
            self.status = t
                .thread
                .join()
                .unwrap_or_else(|_| "Audio thread crashed".into());
        }
    }

    // ----------------------------------------------------------------- UI

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Driver", |ui| {
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
                if ui.button("About Driver & Control Panel…").clicked() {
                    self.dialogs.about = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Device", |ui| {
                for (flag, label) in [
                    (&mut self.dialogs.patch_bay, "PATCH BAY…"),
                    (&mut self.dialogs.memory, "Load / Store Memory of Device…"),
                    (&mut self.dialogs.reset, "Reset Device…"),
                    (&mut self.dialogs.tone, "Test tone…"),
                ] {
                    if ui.button(label).clicked() {
                        *flag = true;
                        ui.close();
                    }
                }
                ui.separator();
                ui.label("Select Device");
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
                    ui.close();
                }
            });
            ui.menu_button("Panel", |ui| {
                let meters_label = if self.meters.is_some() {
                    "Stop SIGNAL meters"
                } else {
                    "Start SIGNAL meters"
                };
                if ui
                    .add_enabled(cfg!(target_os = "linux"), egui::Button::new(meters_label))
                    .clicked()
                {
                    self.toggle_meters();
                    ui.close();
                }
                if ui.button("Reset setting").clicked() {
                    self.mixer = Mixer::new();
                    ui.close();
                }
                if ui.button("Developer: SysEx console…").clicked() {
                    self.dialogs.developer = true;
                    ui.close();
                }
            });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("UA-101")
                    .size(18.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                RichText::new("Control Panel")
                    .size(13.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(12.0);
            self.menu_bar(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.using_daemon() {
                    let (text, color) = if self.daemon_connected {
                        ("UA101D", theme::GREEN)
                    } else {
                        ("UA101D · NO DEVICE", theme::YELLOW)
                    };
                    theme::status_pill(ui, text, color);
                } else if self.link.is_some() {
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                        self.status = "Disconnected; the card is back with the OS driver.".into();
                    }
                    ui.add_space(8.0);
                    theme::status_pill(ui, "CONNECTED", theme::GREEN);
                } else {
                    let connect =
                        egui::Button::new(RichText::new("Connect").strong().color(Color32::WHITE))
                            .fill(theme::BLUE.linear_multiply(0.8));
                    if ui
                        .add_enabled(self.device.is_some(), connect)
                        .on_hover_text(
                            "Takes the MIDI interface. On Linux this detaches snd-ua101 \
                             (the card leaves PipeWire/ALSA) until you disconnect.",
                        )
                        .clicked()
                    {
                        self.connect();
                    }
                    let (text, color) = if self.device.is_some() {
                        ("OFFLINE", theme::TEXT_DIM)
                    } else {
                        ("NO DEVICE", theme::RED)
                    };
                    ui.add_space(8.0);
                    theme::status_pill(ui, text, color);
                }
                if self.meters.is_some() {
                    ui.add_space(6.0);
                    theme::status_pill(ui, "METERS", theme::BLUE);
                }
            });
        });
    }

    /// Sends every OUTPUT (WAVE) strip to MONITOR OUT, or none of them.
    fn set_all_main(&mut self, on: bool) {
        let idx = Bus::Output.index() as usize;
        let mut sends = Vec::new();
        for (ch, st) in self.mixer.strips[idx].iter_mut().enumerate() {
            st.on = on;
            sends.push(mixer::set_mute(Bus::Output, ch as u8, !on));
        }
        self.status = if on {
            "All outputs are sent to MONITOR OUT.".into()
        } else {
            "No output is sent to MONITOR OUT.".into()
        };
        for m in sends {
            self.send(m);
        }
    }

    fn mixer_ui(&mut self, ui: &mut egui::Ui) {
        let mut sends = Vec::new();
        let mut toggle_main = None;
        let meters = self.meters.as_ref();
        let daemon_meters = self.using_daemon() && self.daemon_connected;
        let metering = meters.is_some() || daemon_meters;
        let (levels_in, levels_out) = (&self.levels_in, &self.levels_out);
        // Room for the strip header, buttons and the LINK row.
        let fader_height = (ui.available_height() - 250.0).clamp(80.0, 440.0);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for bus in [Bus::Input, Bus::Output, Bus::MonitorOut] {
                    let color = theme::section_color(bus);
                    let strips = &mut self.mixer.strips[bus.index() as usize];
                    let links = &mut self.mixer.links[bus.index() as usize];
                    theme::card().show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(
                                    Vec2::new(4.0, 16.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(r, 2.0, color);
                                ui.label(
                                    RichText::new(match bus {
                                        Bus::Input => "INPUT MONITOR",
                                        Bus::Output => "OUTPUT MONITOR",
                                        Bus::MonitorOut => "MONITOR OUT",
                                    })
                                    .size(12.0)
                                    .strong(),
                                );
                                if bus == Bus::Output {
                                    ui.add_space(16.0);
                                    ui.label(
                                        RichText::new("TO MAIN:").size(10.0).color(theme::TEXT_DIM),
                                    );
                                    if ui
                                        .small_button("ALL")
                                        .on_hover_text("Send every output to MONITOR OUT")
                                        .clicked()
                                    {
                                        toggle_main = Some(true);
                                    }
                                    if ui
                                        .small_button("NONE")
                                        .on_hover_text("Keep every output out of MONITOR OUT")
                                        .clicked()
                                    {
                                        toggle_main = Some(false);
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for ch in 0..strips.len() {
                                    let level = match (bus, meters) {
                                        (Bus::Input, _) if daemon_meters => {
                                            levels_in.get(ch).copied().unwrap_or(0.0)
                                        }
                                        (Bus::Output, _) if daemon_meters => {
                                            levels_out.get(ch).copied().unwrap_or(0.0)
                                        }
                                        (Bus::Input, Some(m)) => m.level(ch),
                                        _ => 0.0,
                                    };
                                    let linked = links[ch / 2];
                                    let mut s = strips[ch];
                                    let ctx = StripCtx {
                                        bus,
                                        ch,
                                        level,
                                        metering: metering
                                            && (bus == Bus::Input
                                                || (bus == Bus::Output && daemon_meters)),
                                        fader_height,
                                        accent: color,
                                    };
                                    if let Some(what) = strip_ui(ui, &ctx, &mut s) {
                                        let partner = ch ^ 1;
                                        let mut targets = vec![ch];
                                        if linked && partner < strips.len() {
                                            targets.push(partner);
                                        }
                                        for &t in &targets {
                                            let st = &mut strips[t];
                                            match what {
                                                Change::On => {
                                                    st.on = s.on;
                                                    sends
                                                        .push(mixer::set_mute(bus, t as u8, !s.on));
                                                }
                                                // Pan stays per channel when linked.
                                                Change::Pan if t == ch => {
                                                    st.pan = s.pan;
                                                    sends.push(mixer::set_pan(
                                                        bus,
                                                        t as u8,
                                                        pan_raw(s.pan),
                                                    ));
                                                }
                                                Change::Pan => {}
                                                Change::Volume => {
                                                    st.volume_db = s.volume_db;
                                                    sends.push(mixer::set_volume(
                                                        bus,
                                                        t as u8,
                                                        volume_raw(s.volume_db),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for (pair, l) in links.iter_mut().enumerate() {
                                    let text = RichText::new("LINK").size(10.0).color(if *l {
                                        Color32::WHITE
                                    } else {
                                        theme::TEXT_DIM
                                    });
                                    let mut b = egui::Button::new(text);
                                    if *l {
                                        b = b.fill(color.linear_multiply(0.7));
                                    }
                                    if ui
                                        .add_sized(Vec2::new(STRIP_WIDTH * 2.0 + 4.0, 20.0), b)
                                        .on_hover_text("Stereo-link ON and VOLUME of this pair")
                                        .clicked()
                                    {
                                        *l = !*l;
                                        sends.push(mixer::set_link(bus, pair as u8, *l));
                                    }
                                }
                            });
                        });
                    });
                }
            });
        });
        for m in sends {
            self.send(m);
        }
        if let Some(on) = toggle_main {
            self.set_all_main(on);
        }
    }

    fn device_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("DEVICE").size(9.0).color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    for i in 0..self.devices.len().max(1) {
                        if ui
                            .selectable_label(self.selected == i, format!(" {} ", i + 1))
                            .clicked()
                            && i != self.selected
                        {
                            self.selected = i;
                            self.detect();
                        }
                    }
                });
            });
            ui.add_space(8.0);
            let rate = self
                .daemon_rate
                .filter(|_| self.using_daemon())
                .or_else(|| self.device.as_ref().map(|d| d.sample_rate()))
                .map(|r| format!("{:.1} kHz", r as f64 / 1000.0))
                .unwrap_or_else(|| "--.- kHz".into());
            theme::lcd(ui, "SAMPLE RATE", &rate);
            let clock = if self.front.clock.is_some() {
                self.front.clock_label()
            } else {
                "--------"
            };
            theme::lcd(ui, "CLOCK", clock);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("DIRECT MONITOR")
                        .size(9.0)
                        .color(theme::TEXT_DIM),
                );
                ui.horizontal(|ui| {
                    let hint = "Unknown until the switch moves (the unit reports changes only)";
                    let r = theme::lamp(ui, "MONO", self.front.mono, theme::YELLOW);
                    if self.front.mono.is_none() {
                        r.on_hover_text(hint);
                    }
                    let r = theme::lamp(ui, "SOFT CTRL", self.front.soft_ctrl, theme::GREEN);
                    if self.front.soft_ctrl.is_none() {
                        r.on_hover_text(hint);
                    }
                });
            });
            if self.front.soft_ctrl == Some(false) {
                ui.add_space(8.0);
                ui.colored_label(
                    theme::YELLOW,
                    "SOFT CTRL is off: the unit ignores monitor control from the computer",
                );
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut sends = Vec::new();

        let mut open = self.dialogs.patch_bay;
        let mut patch_changes = Vec::new();
        egui::Window::new("PATCH BAY")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                let inputs = if self.levels_in.is_empty() {
                    12
                } else {
                    self.levels_in.len()
                };
                let available = self.using_daemon() && self.daemon_connected;
                patch_changes = self.patchbay.show(ui, inputs, &self.levels_in, available);
            });
        for c in patch_changes {
            self.patch_command(c);
        }
        self.dialogs.patch_bay = open;

        let mut open = self.dialogs.memory;
        egui::Window::new("Memory of Device")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Experimental: addresses from the driver, effect not yet verified.");
                ui.horizontal(|ui| {
                    if ui.button("Load Memory of Device").clicked() {
                        sends.push(mixer::memory(MemoryOp::Load, 0));
                    }
                    if ui.button("Store Memory of Device").clicked() {
                        sends.push(mixer::memory(MemoryOp::Store, 0));
                    }
                });
            });
        self.dialogs.memory = open;

        let mut open = self.dialogs.reset;
        egui::Window::new("Reset Device")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Restore the device settings to the initial condition.");
                ui.label("Experimental: address from the driver, effect not yet verified.");
                if ui.button("Reset").clicked() {
                    sends.push(mixer::memory(MemoryOp::Reset, 0));
                    self.mixer = Mixer::new();
                }
            });
        self.dialogs.reset = open;

        let mut open = self.dialogs.about;
        egui::Window::new("About")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("ua101-panel {}", env!("CARGO_PKG_VERSION")));
                ui.label("Rust user-space driver for the Roland/EDIROL UA-101.");
                if let Some(d) = &self.device {
                    ui.label(format!(
                        "Device: {} (bcdDevice {:04x})",
                        d.info().serial_number().unwrap_or("?"),
                        d.info().device_version()
                    ));
                }
            });
        self.dialogs.about = open;

        let mut open = self.dialogs.developer;
        egui::Window::new("SysEx console")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("Send a DT1 on the control cable (cable 1).");
                ui.horizontal(|ui| {
                    ui.label("Address");
                    ui.text_edit_singleline(&mut self.dev_address);
                    ui.label("Data");
                    ui.text_edit_singleline(&mut self.dev_data);
                });
                if ui.button("Send DT1").clicked() {
                    match (parse_hex(&self.dev_address), parse_hex(&self.dev_data)) {
                        (Some(a), Some(d)) if a.len() == 4 && !d.is_empty() => {
                            sends.push(sysex::dt1([a[0], a[1], a[2], a[3]], &d));
                        }
                        _ => self.status = "Address must be 4 hex bytes, data at least 1".into(),
                    }
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.monospace(l);
                        }
                    });
            });
        self.dialogs.developer = open;

        let mut open = self.dialogs.tone;
        let mut toggle = false;
        egui::Window::new("Test tone")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                if !cfg!(target_os = "linux") {
                    ui.label("Audio streaming is only available on Linux for now.");
                    return;
                }
                ui.add_enabled_ui(self.tone.is_none(), |ui| {
                    ui.add(
                        egui::Slider::new(&mut self.tone_freq, 20.0..=20_000.0)
                            .logarithmic(true)
                            .text("Hz"),
                    );
                    ui.add(egui::Slider::new(&mut self.tone_db, -60.0..=-6.0).text("dBFS"));
                    egui::ComboBox::from_label("Output")
                        .selected_text(if self.tone_output == 0 {
                            "All".to_string()
                        } else {
                            format!("OUTPUT {}", self.tone_output)
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tone_output, 0, "All");
                            for o in 1..=10 {
                                ui.selectable_value(
                                    &mut self.tone_output,
                                    o,
                                    format!("OUTPUT {o}"),
                                );
                            }
                        });
                });
                if ui
                    .button(if self.tone.is_some() { "Stop" } else { "Play" })
                    .clicked()
                {
                    toggle = true;
                }
            });
        self.dialogs.tone = open;
        if toggle {
            if self.tone.is_some() {
                self.stop_tone();
            } else {
                self.start_tone();
            }
        }

        for m in sends {
            self.send(m);
        }
    }
}

#[derive(Clone, Copy)]
enum Change {
    On,
    Pan,
    Volume,
}

struct StripCtx {
    bus: Bus,
    ch: usize,
    level: f32,
    metering: bool,
    fader_height: f32,
    accent: Color32,
}

/// Draws one channel strip; returns what the user changed.
fn strip_ui(ui: &mut egui::Ui, c: &StripCtx, s: &mut Strip) -> Option<Change> {
    let mut change = None;
    let button = Vec2::new(STRIP_WIDTH - 6.0, 20.0);
    theme::strip_frame().show(ui, |ui| {
        ui.set_width(STRIP_WIDTH - 6.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.spacing_mut().button_padding = Vec2::new(2.0, 2.0);
            let name = match c.bus {
                Bus::MonitorOut => if c.ch == 0 { "L" } else { "R" }.to_string(),
                _ => format!("{}", c.ch + 1),
            };
            ui.label(RichText::new(name).size(13.0).strong().color(c.accent));

            // OUTPUT strips: MAIN (lit = sent to MONITOR OUT). Others: MUTE
            // (lit = muted). Both drive the MUTE parameter (00 0b ch 02).
            let (label, lit_when_on, fill, hover) = if c.bus == Bus::Output {
                (
                    "MAIN",
                    true,
                    theme::ORANGE,
                    "Send this output to MONITOR OUT",
                )
            } else {
                (
                    "MUTE",
                    false,
                    theme::RED,
                    "Mute this channel in the monitor mix",
                )
            };
            let lit = s.on == lit_when_on;
            let text = RichText::new(label).size(10.0).strong().color(if lit {
                Color32::BLACK
            } else {
                theme::TEXT_DIM
            });
            let mut btn = egui::Button::new(text).min_size(button);
            if lit {
                btn = btn.fill(fill);
            }
            if ui.add(btn).on_hover_text(hover).clicked() {
                s.on = !s.on;
                change = Some(Change::On);
            }
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("SOLO").size(9.0)).min_size(button),
            )
            .on_disabled_hover_text("SOLO address not identified yet");
            let mut pan = s.pan;
            let r = ui
                .add_enabled(
                    c.bus != Bus::MonitorOut,
                    egui::DragValue::new(&mut pan)
                        .range(-64..=63)
                        .speed(0.5)
                        .custom_formatter(|v, _| match v as i32 {
                            0 => "C".into(),
                            v if v < 0 => format!("L{}", -v),
                            v => format!("R{v}"),
                        }),
                )
                .on_hover_text("PAN: drag left/right, double-click to centre");
            if r.double_clicked() {
                pan = 0;
            }
            if pan != s.pan {
                s.pan = pan;
                change = Some(Change::Pan);
            }

            let mut db = s.volume_db;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if c.bus != Bus::MonitorOut {
                    theme::meter(ui, c.fader_height, c.level, c.metering);
                }
                ui.spacing_mut().slider_width = c.fader_height;
                let r = ui
                    .add(
                        egui::Slider::new(&mut db, FADER_MIN_DB..=FADER_MAX_DB)
                            .vertical()
                            .show_value(false),
                    )
                    .on_hover_text(format!(
                        "VOLUME {} dB (device gain {:#08x}; 0 dB = 0x100000, the normal level). \
                         Double-click: 0 dB",
                        format_db(db),
                        volume_raw(db)
                    ));
                if r.double_clicked() {
                    db = 0.0;
                }
                // Snap to 0 dB near unity, like a detented fader.
                if r.dragged() && db.abs() < 0.4 {
                    db = 0.0;
                }
            });
            if (db - s.volume_db).abs() > f32::EPSILON {
                s.volume_db = db;
                change = Some(Change::Volume);
            }
            let color = if db > 0.05 {
                theme::YELLOW
            } else {
                theme::TEXT_DIM
            };
            ui.label(
                RichText::new(format_db(db))
                    .size(10.0)
                    .monospace()
                    .color(color),
            );
        });
    });
    change
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    s.split_whitespace()
        .map(|b| {
            u8::from_str_radix(b.trim_start_matches("0x"), 16)
                .ok()
                .filter(|v| *v < 0x80)
        })
        .collect()
}

impl eframe::App for Panel {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_link();
        let ctx = ui.ctx().clone();
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.header(ui));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ui, |ui| {
                ui.label(
                    RichText::new(&self.status)
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
            });
        egui::Panel::bottom("device")
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.device_panel(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ui, |ui| {
                if !self.has_device() {
                    ui.centered_and_justified(|ui| {
                        if ui.button("No UA-101 found — Rescan").clicked() {
                            self.detect();
                        }
                    });
                    return;
                }
                self.mixer_ui(ui);
            });
        self.dialogs(&ctx);
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.disconnect();
    }
}
