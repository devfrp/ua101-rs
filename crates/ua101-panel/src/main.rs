//! UA-101 control panel, laid out like Roland's original one: monitor
//! mixer strips, a device panel with the unit's front-panel state, patch
//! bay and device memory dialogs.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod link;
mod meters;

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
/// Fader range until the volume scale is calibrated on hardware.
const VOLUME_MAX: u16 = 0x7fff;
const LOG_LINES: usize = 200;

const LED_ON: Color32 = Color32::from_rgb(0x40, 0xe0, 0x40);
const LED_HOT: Color32 = Color32::from_rgb(0xff, 0x40, 0x30);
const LED_OFF: Color32 = Color32::from_rgb(0x30, 0x34, 0x30);
const AMBER: Color32 = Color32::from_rgb(0xff, 0xb0, 0x20);

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UA-101 Control Panel")
            .with_inner_size([1180.0, 560.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "UA-101 Control Panel",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(Panel::new(cc.egui_ctx.clone())))
        }),
    )
}

#[derive(Clone, Copy, Default)]
struct Strip {
    on: bool,
    solo: bool,
    pan: i32,
    volume: u16,
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

    fn mixer_ui(&mut self, ui: &mut egui::Ui) {
        let mut sends = Vec::new();
        let meters = self.meters.as_ref();
        // Leave room for labels, buttons and the LINK row under the fader.
        let fader_height = (ui.available_height() - 230.0).clamp(80.0, 420.0);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for bus in [Bus::Input, Bus::Output, Bus::MonitorOut] {
                    let strips = &mut self.mixer.strips[bus.index() as usize];
                    let links = &mut self.mixer.links[bus.index() as usize];
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(match bus {
                                    Bus::Input => "INPUT MONITOR",
                                    Bus::Output => "OUTPUT MONITOR",
                                    Bus::MonitorOut => "MONITOR OUT",
                                })
                                .size(11.0)
                                .strong(),
                            );
                            ui.horizontal_top(|ui| {
                                for ch in 0..strips.len() {
                                    let level = match (bus, meters) {
                                        (Bus::Input, Some(m)) => m.level(ch),
                                        _ => 0.0,
                                    };
                                    let linked = links[ch / 2];
                                    let mut s = strips[ch];
                                    let changed =
                                        strip_ui(ui, bus, ch, &mut s, level, fader_height);
                                    if let Some(what) = changed {
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
                                                    st.volume = s.volume;
                                                    sends.push(mixer::set_volume(
                                                        bus, t as u8, s.volume,
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                            // LINK buttons, one per pair.
                            ui.horizontal(|ui| {
                                for (pair, l) in links.iter_mut().enumerate() {
                                    let r = ui.add_sized(
                                        Vec2::new(
                                            STRIP_WIDTH * 2.0 + ui.spacing().item_spacing.x,
                                            20.0,
                                        ),
                                        egui::Button::new(RichText::new("LINK").color(if *l {
                                            AMBER
                                        } else {
                                            Color32::GRAY
                                        }))
                                        .selected(*l),
                                    );
                                    if r.clicked() {
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
    }

    fn device_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for i in 0..self.devices.len().max(1) {
                let label = format!("{}", i + 1);
                if ui.selectable_label(self.selected == i, label).clicked() && i != self.selected {
                    self.selected = i;
                    self.detect();
                }
            }
            ui.separator();
            let rate = self
                .device
                .as_ref()
                .map(|d| format!("{:.1} kHz", d.sample_rate() as f64 / 1000.0))
                .unwrap_or_else(|| "—".into());
            panel_box(ui, "SAMPLE RATE", &rate);
            let clock = if self.front.clock.is_some() {
                self.front.clock_label()
            } else {
                "—"
            };
            panel_box(ui, "CLOCK", clock);
            ui.separator();
            ui.vertical(|ui| {
                ui.label(RichText::new("DIRECT MONITOR").small());
                ui.horizontal(|ui| {
                    lamp(ui, "MONO", self.front.mono);
                    lamp(ui, "SOFT CTRL", self.front.soft_ctrl);
                });
            });
            ui.separator();
            if self.link.is_some() {
                if ui.button("DISCONNECT").clicked() {
                    self.disconnect();
                    self.status = "Disconnected; the card is back with the OS driver.".into();
                }
            } else if ui
                .add_enabled(self.device.is_some(), egui::Button::new("CONNECT"))
                .on_hover_text(
                    "Takes the MIDI interface. On Linux this detaches snd-ua101 \
                     (the card leaves PipeWire/ALSA) until you disconnect.",
                )
                .clicked()
            {
                self.connect();
            }
            if self.front.soft_ctrl == Some(false) {
                ui.colored_label(
                    AMBER,
                    "SOFT CTRL is off: the unit ignores software monitor control",
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

/// Draws one channel strip; returns what the user changed.
fn strip_ui(
    ui: &mut egui::Ui,
    bus: Bus,
    ch: usize,
    s: &mut Strip,
    level: f32,
    fader_height: f32,
) -> Option<Change> {
    let mut change = None;
    let button = Vec2::new(STRIP_WIDTH - 6.0, 20.0);
    ui.allocate_ui_with_layout(
        Vec2::new(STRIP_WIDTH, fader_height + 170.0),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_width(STRIP_WIDTH);
            ui.spacing_mut().item_spacing.y = 4.0;
            let name = match bus {
                Bus::MonitorOut => if ch == 0 { "L" } else { "R" }.to_string(),
                _ => format!("{}", ch + 1),
            };
            ui.label(RichText::new(name).strong());

            let color = if level > 0.89 {
                LED_HOT
            } else if level > 0.003 {
                LED_ON
            } else {
                LED_OFF
            };
            let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, color);
            ui.label(RichText::new("SIGNAL").size(8.0));

            let on_text =
                RichText::new("ON")
                    .size(11.0)
                    .color(if s.on { LED_ON } else { Color32::GRAY });
            if ui
                .add_sized(button, egui::Button::new(on_text).selected(s.on))
                .on_hover_text("MONITOR on/off (address inferred: 00 0b ch 02)")
                .clicked()
            {
                s.on = !s.on;
                change = Some(Change::On);
            }
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("SOLO").size(8.5))
                    .selected(s.solo)
                    .min_size(button),
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

            let mut v = s.volume;
            ui.spacing_mut().slider_width = fader_height;
            ui.add(
                egui::Slider::new(&mut v, 0..=VOLUME_MAX)
                    .vertical()
                    .show_value(false),
            )
            .on_hover_text(format!(
                "VOLUME raw {v:#06x} (scale not calibrated; address 00 0b ch 03)"
            ));
            if v != s.volume {
                s.volume = v;
                change = Some(Change::Volume);
            }
            ui.label(
                RichText::new(format!("{:.0}%", v as f32 * 100.0 / VOLUME_MAX as f32)).size(9.0),
            );
        },
    );
    change
}

fn panel_box(ui: &mut egui::Ui, title: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(title).small());
        egui::Frame::new()
            .fill(Color32::from_rgb(0x10, 0x18, 0x10))
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.label(RichText::new(value).monospace().color(LED_ON));
            });
    });
}

fn lamp(ui: &mut egui::Ui, label: &str, state: Option<bool>) {
    let color = match state {
        Some(true) => AMBER,
        Some(false) => LED_OFF,
        None => Color32::from_gray(0x50),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, color);
        ui.label(label);
    })
    .response
    .on_hover_text(match state {
        None => "Unknown until the switch moves (the unit reports changes only)",
        Some(true) => "On",
        Some(false) => "Off",
    });
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
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(&self.status);
        });
        egui::Panel::bottom("device").show(ui, |ui| {
            ui.add_space(4.0);
            self.device_panel(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
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
