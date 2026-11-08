//! UA-101 control panel, laid out like Roland's original one: monitor
//! mixer strips, a device panel with the unit's front-panel state, patch
//! bay and device memory dialogs.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod link;
mod meters;
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
/// Device value for 0 dB. Hypothesis: a linear 16-bit gain with unity at
/// 0x4000, so the 16-bit maximum 0x7fff sits at +6 dB. Not yet verified on
/// hardware.
const VOLUME_UNITY: f32 = 16384.0;

/// Converts a fader position in dB to the 16-bit device value.
fn volume_raw(db: f32) -> u16 {
    if db <= FADER_MIN_DB {
        0
    } else {
        (VOLUME_UNITY * 10f32.powf(db / 20.0))
            .round()
            .clamp(0.0, 32767.0) as u16
    }
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
    on: bool,
    pan: i32,
    /// Fader position in dB (`FADER_MIN_DB` = -inf).
    volume_db: f32,
}

impl Default for Strip {
    fn default() -> Self {
        Self {
            on: false,
            pan: 0,
            volume_db: 0.0,
        }
    }
}

/// Local mirror of the mixer, indexed by [`Bus::index`]. The unit does not
/// report mixer values, so this starts from defaults.
struct Mixer {
    strips: [Vec<Strip>; 3],
    links: [Vec<bool>; 3],
}

impl Mixer {
    fn new() -> Self {
        let mk = |b: Bus| vec![Strip::default(); b.channels() as usize];
        let lk = |b: Bus| vec![b == Bus::MonitorOut; b.channels() as usize / 2];
        Self {
            strips: [mk(Bus::MonitorOut), mk(Bus::Input), mk(Bus::Output)],
            links: [lk(Bus::MonitorOut), lk(Bus::Input), lk(Bus::Output)],
        }
    }
}

/// Output source in the patch bay (encoding inferred).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Wave,
    Input,
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
    patch: Vec<Source>,
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
    /// OUTPUT TO MAIN switch and the strip states it saved when turned off.
    outputs_to_main: bool,
    saved_output_on: Vec<bool>,
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
            patch: vec![Source::Wave; 10],
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
            outputs_to_main: true,
            saved_output_on: Vec::new(),
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

    fn detect(&mut self) {
        self.disconnect();
        self.device = None;
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
        match &self.link {
            Some(l) => l.send(msg),
            None => self.status = "Not connected: press CONNECT first.".into(),
        }
    }

    fn poll_link(&mut self) {
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
                    (&mut self.dialogs.patch_bay, "Set PATCH BAY…"),
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
                if self.link.is_some() {
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

    /// OUTPUT TO MAIN: when off, every OUTPUT (WAVE) strip is switched off
    /// in the monitor mix so playback is not sent straight to the main
    /// outputs; the per-strip settings come back when it is switched on.
    fn set_outputs_to_main(&mut self, on: bool) {
        let idx = Bus::Output.index() as usize;
        let mut sends = Vec::new();
        if on {
            for (ch, st) in self.mixer.strips[idx].iter_mut().enumerate() {
                st.on = self.saved_output_on.get(ch).copied().unwrap_or(true);
                sends.push(mixer::set_on(Bus::Output, ch as u8, st.on));
            }
        } else {
            self.saved_output_on = self.mixer.strips[idx].iter().map(|s| s.on).collect();
            for (ch, st) in self.mixer.strips[idx].iter_mut().enumerate() {
                st.on = false;
                sends.push(mixer::set_on(Bus::Output, ch as u8, false));
            }
        }
        self.outputs_to_main = on;
        self.status = if on {
            "OUTPUT TO MAIN on: playback is monitored on MONITOR OUT again.".into()
        } else {
            "OUTPUT TO MAIN off: playback is no longer sent directly to MONITOR OUT.".into()
        };
        for m in sends {
            self.send(m);
        }
    }

    fn mixer_ui(&mut self, ui: &mut egui::Ui) {
        let mut sends = Vec::new();
        let mut toggle_main = None;
        let meters = self.meters.as_ref();
        let metering = meters.is_some();
        // Room for the strip header, buttons and the LINK row.
        let fader_height = (ui.available_height() - 250.0).clamp(80.0, 440.0);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for bus in [Bus::Input, Bus::Output, Bus::MonitorOut] {
                    let color = theme::section_color(bus);
                    let strips = &mut self.mixer.strips[bus.index() as usize];
                    let links = &mut self.mixer.links[bus.index() as usize];
                    let outputs_off = bus == Bus::Output && !self.outputs_to_main;
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
                                    {
                                        {
                                            ui.label(
                                                RichText::new("OUTPUT TO MAIN").size(11.0).color(
                                                    if self.outputs_to_main {
                                                        theme::ORANGE
                                                    } else {
                                                        theme::TEXT_DIM
                                                    },
                                                ),
                                            );
                                            let mut on = self.outputs_to_main;
                                            if ui
                                                .add(theme::Toggle::new(&mut on, theme::ORANGE))
                                                .on_hover_text(
                                                    "Send the computer's outputs (WAVE OUT) \
                                                     directly to MONITOR OUT. Turn off to keep \
                                                     playback out of the main mix.",
                                                )
                                                .changed()
                                            {
                                                toggle_main = Some(on);
                                            }
                                        }
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                for ch in 0..strips.len() {
                                    let level = match (bus, meters) {
                                        (Bus::Input, Some(m)) => m.level(ch),
                                        _ => 0.0,
                                    };
                                    let linked = links[ch / 2];
                                    let mut s = strips[ch];
                                    let ctx = StripCtx {
                                        bus,
                                        ch,
                                        level,
                                        metering: metering && bus == Bus::Input,
                                        fader_height,
                                        accent: color,
                                        on_enabled: !outputs_off,
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
                                                    sends.push(mixer::set_on(bus, t as u8, s.on));
                                                }
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
            self.set_outputs_to_main(on);
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
                .device
                .as_ref()
                .map(|d| format!("{:.1} kHz", d.sample_rate() as f64 / 1000.0))
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
        egui::Window::new("PATCH BAY")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Output source for each OUTPUT. Experimental: value encoding inferred.");
                egui::Grid::new("patch").num_columns(3).show(ui, |ui| {
                    for (o, src) in self.patch.iter_mut().enumerate() {
                        ui.label(format!("OUTPUT {}", o + 1));
                        let before = *src;
                        ui.selectable_value(src, Source::Wave, format!("WAVE OUT {}", o + 1));
                        ui.selectable_value(src, Source::Input, format!("INPUT {}", o + 1));
                        if *src != before {
                            sends
                                .push(mixer::set_patch_bay(o as u8, (*src == Source::Input) as u8));
                        }
                        ui.end_row();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Hard Thru").clicked() {
                        for (o, s) in self.patch.iter_mut().enumerate() {
                            *s = Source::Input;
                            sends.push(mixer::set_patch_bay(o as u8, 1));
                        }
                    }
                    if ui.button("Initial setting").clicked() {
                        for (o, s) in self.patch.iter_mut().enumerate() {
                            *s = Source::Wave;
                            sends.push(mixer::set_patch_bay(o as u8, 0));
                        }
                    }
                });
            });
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
    Volume,
}

struct StripCtx {
    bus: Bus,
    ch: usize,
    level: f32,
    metering: bool,
    fader_height: f32,
    accent: Color32,
    on_enabled: bool,
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

            let on_text = RichText::new("ON").size(10.0).strong().color(if s.on {
                Color32::BLACK
            } else {
                theme::TEXT_DIM
            });
            let mut on_btn = egui::Button::new(on_text).min_size(button);
            if s.on {
                on_btn = on_btn.fill(theme::GREEN);
            }
            let hover = if c.on_enabled {
                "Monitor ON/OFF (address inferred: 00 0b ch 02)"
            } else {
                "OUTPUT TO MAIN is off"
            };
            if ui
                .add_enabled(c.on_enabled, on_btn)
                .on_hover_text(hover)
                .on_disabled_hover_text(hover)
                .clicked()
            {
                s.on = !s.on;
                change = Some(Change::On);
            }
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("SOLO").size(9.0)).min_size(button),
            )
            .on_disabled_hover_text("SOLO address not identified yet");
            ui.add_enabled(
                false,
                egui::DragValue::new(&mut s.pan)
                    .range(-64..=63)
                    .custom_formatter(|v, _| match v as i32 {
                        0 => "C".into(),
                        v if v < 0 => format!("L{}", -v),
                        v => format!("R{v}"),
                    }),
            )
            .on_disabled_hover_text("PAN address not identified yet");

            let mut db = s.volume_db;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                theme::meter(ui, c.fader_height, c.level, c.metering);
                ui.spacing_mut().slider_width = c.fader_height;
                let r = ui
                    .add(
                        egui::Slider::new(&mut db, FADER_MIN_DB..=FADER_MAX_DB)
                            .vertical()
                            .show_value(false),
                    )
                    .on_hover_text(format!(
                        "VOLUME {} dB (device value {:#06x}, unity 0x4000 assumed). \
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
                if self.device.is_none() {
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
