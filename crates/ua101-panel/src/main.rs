//! UA-101 control panel: device status, direct monitoring and a test tone.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use ua101_usb::{
    StreamConfig, Ua101, list_devices,
    protocol::{descriptors::StreamFormat, monitor::MonitorTarget, pcm::S24_MAX},
};

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UA-101 Panel")
            .with_inner_size([560.0, 520.0])
            .with_min_inner_size([420.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "UA-101 Panel",
        options,
        Box::new(|_cc| Ok(Box::new(Panel::new()))),
    )
}

struct Tone {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<String>,
}

struct Panel {
    device: Option<Ua101>,
    status: String,
    monitor: Vec<bool>,
    tone_freq: f64,
    tone_level_db: f64,
    tone_output: usize, // 0 = all outputs
    tone: Option<Tone>,
}

impl Panel {
    fn new() -> Self {
        let mut p = Self {
            device: None,
            status: String::new(),
            monitor: Vec::new(),
            tone_freq: 440.0,
            tone_level_db: -18.0,
            tone_output: 0,
            tone: None,
        };
        p.detect();
        p
    }

    fn detect(&mut self) {
        self.stop_tone();
        self.device = None;
        match list_devices().map(|d| d.into_iter().next()) {
            Ok(Some(info)) => match Ua101::open(info) {
                Ok(dev) => {
                    let inputs = dev.capture_format().map(|f| f.channels).unwrap_or(10);
                    self.monitor = vec![false; inputs as usize];
                    self.status = "UA-101 connected".into();
                    self.device = Some(dev);
                }
                Err(e) => {
                    self.status = format!(
                        "UA-101 found but cannot be opened: {e}. On Linux, install the udev rule."
                    );
                }
            },
            Ok(None) => self.status = "No UA-101 connected".into(),
            Err(e) => self.status = format!("USB error: {e}"),
        }
    }

    fn set_monitor(&mut self, target: MonitorTarget, on: bool) {
        let Some(dev) = &self.device else { return };
        match dev.set_direct_monitor(target, on) {
            Ok(()) => {
                match target {
                    MonitorTarget::All => self.monitor.iter_mut().for_each(|m| *m = on),
                    MonitorTarget::Input(i) => {
                        if let Some(m) = self.monitor.get_mut(i as usize) {
                            *m = on;
                        }
                    }
                }
                self.status = format!(
                    "Direct monitor {target:?}: {}",
                    if on { "on" } else { "off" }
                );
            }
            Err(e) => self.status = format!("Direct monitor failed: {e}"),
        }
    }

    fn start_tone(&mut self) {
        let Some(dev) = &self.device else { return };
        let mut stream = match dev.start_stream(StreamConfig::default()) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Cannot start audio: {e}");
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let freq = self.tone_freq;
        let amp = 10f64.powf(self.tone_level_db / 20.0).min(1.0) * S24_MAX as f64;
        let output = self.tone_output;
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
                        let v = if output == 0 || output == c + 1 { s } else { 0 };
                        let _ = tx.push(v);
                    }
                    n += 1;
                }
            }
            match stream.stop() {
                Ok(st) => format!(
                    "Tone stopped ({} frames played, {} underruns)",
                    st.playback_frames, st.playback_underruns
                ),
                Err(e) => format!("Audio stopped: {e}"),
            }
        });
        self.tone = Some(Tone { stop, thread });
        self.status = "Playing test tone".into();
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
}

fn format_row(ui: &mut egui::Ui, name: &str, f: Result<StreamFormat, impl std::fmt::Display>) {
    ui.label(name);
    match f {
        Ok(f) => ui.label(format!(
            "{} ch · {}-bit · {} Hz · EP 0x{:02x}",
            f.channels, f.bit_resolution, f.sample_rate, f.endpoint.address
        )),
        Err(e) => ui.label(format!("unavailable: {e}")),
    };
    ui.end_row();
}

impl eframe::App for Panel {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("UA-101");
                if ui.button("Detect").clicked() {
                    self.detect();
                }
            });
            ui.separator();

            let mut monitor_action = None;
            let mut tone_toggle = false;
            if let Some(dev) = &self.device {
                let info = dev.info();
                egui::Grid::new("device").num_columns(2).show(ui, |ui| {
                    ui.label("Serial");
                    ui.label(info.serial_number().unwrap_or("?"));
                    ui.end_row();
                    ui.label("Speed");
                    ui.label(format!("{:?}", dev.speed()));
                    ui.end_row();
                    ui.label("Sample rate");
                    ui.label(format!("{} Hz (hardware switch)", dev.sample_rate()));
                    ui.end_row();
                    format_row(ui, "Playback", dev.playback_format());
                    format_row(ui, "Capture", dev.capture_format());
                });

                ui.add_space(8.0);
                ui.heading("Direct monitor");
                ui.label("Experimental: request encoding inferred from Roland's ASIO driver.");
                ui.horizontal(|ui| {
                    if ui.button("All on").clicked() {
                        monitor_action = Some((MonitorTarget::All, true));
                    }
                    if ui.button("All off").clicked() {
                        monitor_action = Some((MonitorTarget::All, false));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for (i, on) in self.monitor.iter().enumerate() {
                        let mut v = *on;
                        if ui.toggle_value(&mut v, format!("IN {}", i + 1)).changed() {
                            monitor_action = Some((MonitorTarget::Input(i as u16), v));
                        }
                    }
                });

                ui.add_space(8.0);
                ui.heading("Test tone");
                let outputs = dev.playback_format().map(|f| f.channels).unwrap_or(10) as usize;
                let streaming_supported = cfg!(target_os = "linux");
                ui.add_enabled_ui(streaming_supported && self.tone.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut self.tone_freq, 20.0..=20_000.0)
                                .logarithmic(true)
                                .text("Hz"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut self.tone_level_db, -60.0..=0.0).text("dBFS"),
                        );
                    });
                    egui::ComboBox::from_label("Output")
                        .selected_text(if self.tone_output == 0 {
                            "All".to_string()
                        } else {
                            format!("OUT {}", self.tone_output)
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tone_output, 0, "All");
                            for o in 1..=outputs {
                                ui.selectable_value(&mut self.tone_output, o, format!("OUT {o}"));
                            }
                        });
                });
                if streaming_supported {
                    let label = if self.tone.is_some() { "Stop" } else { "Play" };
                    if ui.button(label).clicked() {
                        tone_toggle = true;
                    }
                } else {
                    ui.label("Audio streaming is only available on Linux for now.");
                }
            }

            if let Some((target, on)) = monitor_action {
                self.set_monitor(target, on);
            }
            if tone_toggle {
                if self.tone.is_some() {
                    self.stop_tone();
                } else {
                    self.start_tone();
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.label(&self.status);
                ui.separator();
            });
        });
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.stop_tone();
    }
}
