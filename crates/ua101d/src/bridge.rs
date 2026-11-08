//! Audio bridge between the USB streams and the `snd-aloop` loopback card.
//!
//! The loopback card runs on the system clock and the UA-101 on its own
//! crystal, so the two drift apart by a few frames per second. Each pump
//! keeps its USB ring near a target fill and, when the fill leaves a band
//! around it, drops or repeats one frame per period. This is a simple
//! first approach; a resampler would avoid the occasional tiny glitch.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::alsa::Pcm;

/// Per-channel peak levels (linear, 0.0–1.0) since the last read.
#[derive(Default)]
pub struct Levels {
    pub inputs: Vec<f32>,
    pub outputs: Vec<f32>,
}

pub type SharedLevels = Arc<Mutex<Levels>>;

fn accumulate_peaks(peaks: &mut Vec<f32>, samples: &[i32], channels: usize) {
    if peaks.len() != channels {
        peaks.resize(channels, 0.0);
    }
    for (i, &s) in samples.iter().enumerate() {
        let v = s.unsigned_abs() as f32 / 8_388_608.0;
        let p = &mut peaks[i % channels];
        if v > *p {
            *p = v;
        }
    }
}

pub struct PumpConfig {
    pub period: usize,
    /// Target fill of the USB ring, in periods.
    pub target_periods: usize,
}

/// Applications → loopback → UA-101 outputs.
pub fn to_device(
    pcm: Pcm,
    mut tx: rtrb::Producer<i32>,
    stop: Arc<AtomicBool>,
    levels: SharedLevels,
    cfg: PumpConfig,
) -> std::io::Result<()> {
    let channels = pcm.config().channels as usize;
    let capacity = tx.buffer().capacity();
    let target = cfg.target_periods * cfg.period;
    let band = 2 * cfg.period;
    let mut buf = vec![0i32; cfg.period * channels];

    // Prime the device side with silence so it starts at the target fill.
    for _ in 0..target * channels {
        let _ = tx.push(0);
    }
    while !stop.load(Ordering::Relaxed) {
        let frames = match pcm.read(&mut buf) {
            Ok(f) => f,
            Err(e) if is_peer_mismatch(&e) => {
                // The application side of the loopback is configured with a
                // different format; wait for it to match.
                wait_for_peer(&pcm, &stop, "to-device");
                continue;
            }
            Err(e) => return Err(e),
        };
        let mut n = frames * channels;
        for s in &mut buf[..n] {
            *s >>= 8; // S32_LE from applications → 24-bit
        }
        if let Ok(mut l) = levels.lock() {
            accumulate_peaks(&mut l.outputs, &buf[..n], channels);
        }
        let fill = (capacity - tx.slots()) / channels;
        if fill > target + band && n >= channels {
            n -= channels; // device clock slower: drop one frame
        }
        let repeat = fill + frames < target.saturating_sub(band) && n >= channels;
        for &s in &buf[..n] {
            if tx.push(s).is_err() {
                break;
            }
        }
        if repeat {
            // Device clock faster: repeat the last frame.
            for c in 0..channels {
                let _ = tx.push(buf[n - channels + c]);
            }
        }
    }
    Ok(())
}

/// UA-101 inputs → loopback → applications.
pub fn from_device(
    pcm: Pcm,
    mut rx: rtrb::Consumer<i32>,
    stop: Arc<AtomicBool>,
    levels: SharedLevels,
    cfg: PumpConfig,
) -> std::io::Result<()> {
    let channels = pcm.config().channels as usize;
    let target = cfg.target_periods * cfg.period;
    let band = 2 * cfg.period;
    let mut buf = vec![0i32; (cfg.period + 1) * channels];

    // Wait until the device has delivered the target amount.
    let started = Instant::now();
    while rx.slots() / channels < target && !stop.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(2) {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    while !stop.load(Ordering::Relaxed) {
        let fill = rx.slots() / channels;
        if fill < cfg.period {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let (take, out) = if fill > target + band {
            (cfg.period + 1, cfg.period) // drop one frame
        } else if fill < target.saturating_sub(band) {
            (cfg.period - 1, cfg.period) // repeat one frame
        } else {
            (cfg.period, cfg.period)
        };
        let n = take * channels;
        match rx.read_chunk(n) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                buf[..a.len()].copy_from_slice(a);
                buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            Err(_) => continue,
        }
        if take < out {
            let last = (take - 1) * channels;
            buf.copy_within(last..last + channels, take * channels);
        }
        let m = out * channels;
        if let Ok(mut l) = levels.lock() {
            accumulate_peaks(&mut l.inputs, &buf[..m], channels);
        }
        for s in &mut buf[..m] {
            *s <<= 8; // 24-bit → S32_LE for applications
        }
        if let Err(e) = pcm.write(&buf[..m]) {
            if is_peer_mismatch(&e) {
                wait_for_peer(&pcm, &stop, "from-device");
            } else {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// `snd-aloop` reports EIO when the two ends of the loopback cable use
/// different formats (e.g. PipeWire opened the application side as stereo).
fn is_peer_mismatch(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EIO)
}

fn wait_for_peer(pcm: &Pcm, stop: &AtomicBool, side: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "{side}: the application side of the loopback card uses another format; \
             set its PipeWire profile to pro-audio (see packaging/linux/51-ua101.conf)"
        );
    }
    for _ in 0..10 {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = pcm.prepare();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_per_channel() {
        let mut p = Vec::new();
        accumulate_peaks(&mut p, &[0, 4_194_304, -8_388_608, 0], 2);
        assert_eq!(p, vec![1.0, 0.5]);
    }
}
