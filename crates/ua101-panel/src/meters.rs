//! SIGNAL lamps: per-channel peak levels computed from the capture stream,
//! as Roland's driver does from the audio it moves.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use ua101_usb::{StreamConfig, Ua101};

/// Peak level per capture channel, 0.0–1.0, decaying over time.
pub type Levels = Arc<Mutex<Vec<f32>>>;

pub struct Meters {
    pub levels: Levels,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<String>>,
}

impl Meters {
    pub fn start(device: &Ua101, ctx: egui::Context) -> Result<Self, String> {
        let mut stream = device
            .start_stream(StreamConfig::default())
            .map_err(|e| e.to_string())?;
        let channels = stream.capture_format().channels as usize;
        let levels: Levels = Arc::new(Mutex::new(vec![0.0; channels]));
        let stop = Arc::new(AtomicBool::new(false));
        let (lv, flag) = (levels.clone(), stop.clone());
        let thread = thread::spawn(move || {
            let mut peaks = vec![0f32; channels];
            while !flag.load(Ordering::Relaxed) && stream.is_running() {
                let rx = stream.capture();
                let n = rx.slots() / channels * channels;
                if n == 0 {
                    thread::sleep(Duration::from_millis(15));
                    continue;
                }
                peaks.fill(0.0);
                if let Ok(chunk) = rx.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    for (i, &s) in a.iter().chain(b).enumerate() {
                        let v = (s.unsigned_abs() as f32) / 8_388_608.0;
                        let c = i % channels;
                        if v > peaks[c] {
                            peaks[c] = v;
                        }
                    }
                    chunk.commit_all();
                }
                if let Ok(mut l) = lv.lock() {
                    for (l, &p) in l.iter_mut().zip(&peaks) {
                        *l = p.max(*l * 0.85);
                    }
                }
                ctx.request_repaint();
                thread::sleep(Duration::from_millis(30));
            }
            match stream.stop() {
                Ok(_) => String::new(),
                Err(e) => e.to_string(),
            }
        });
        Ok(Self {
            levels,
            stop,
            thread: Some(thread),
        })
    }

    pub fn level(&self, channel: usize) -> f32 {
        self.levels
            .lock()
            .ok()
            .and_then(|l| l.get(channel).copied())
            .unwrap_or(0.0)
    }

    /// Stops the stream; returns an error message if it failed.
    pub fn stop(mut self) -> String {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Meters {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
