//! Audio bridge between the USB streams and the `snd-aloop` loopback card.
//!
//! The loopback card runs on the system clock and the UA-101 on its own
//! crystal, so the two drift apart slowly (well under 1 frame per second
//! measured on one unit). Each pump keeps its USB ring near a target fill:
//! [`DriftControl`] follows a smoothed fill and, when it leaves a band
//! around the target, drops or repeats one frame, at most once every few
//! periods. Scheduling jitter moves the instantaneous fill by whole
//! periods, so reacting to it directly glitched every period for seconds.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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

/// Resamples interleaved `src` to `out_frames` frames by linear
/// interpolation, keeping its first and last frames. A drift correction
/// stretches one period by a single frame this way (0.4 % for 5.8 ms at
/// 256 frames) instead of cutting or doubling a frame, which leaves a step
/// in the waveform that can be heard as a click.
fn stretch(src: &[i32], channels: usize, out_frames: usize, dst: &mut Vec<i32>) {
    dst.clear();
    let in_frames = src.len() / channels.max(1);
    if in_frames < 2 || out_frames < 2 {
        dst.extend_from_slice(&src[..(out_frames.min(in_frames) * channels)]);
        return;
    }
    let step = (in_frames - 1) as f64 / (out_frames - 1) as f64;
    for j in 0..out_frames {
        let pos = j as f64 * step;
        let i0 = (pos as usize).min(in_frames - 2);
        let frac = pos - i0 as f64;
        for c in 0..channels {
            let a = src[i0 * channels + c] as f64;
            let b = src[(i0 + 1) * channels + c] as f64;
            dst.push((a + (b - a) * frac).round() as i32);
        }
    }
}

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

/// Glitch counters of one pump, read by the daemon to report them.
#[derive(Default)]
pub struct PumpStats {
    /// ALSA over/underruns on the loopback side.
    pub xruns: AtomicU64,
    /// Frames dropped to follow the device clock.
    pub drops: AtomicU64,
    /// Frames repeated to follow the device clock.
    pub repeats: AtomicU64,
}

impl PumpStats {
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a pump does to one period to follow the device clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    None,
    /// The ring is filling up: drop one frame.
    Drop,
    /// The ring is draining: repeat one frame.
    Repeat,
}

/// Clock-drift controller on the USB ring fill, updated once per period.
///
/// It reacts to the fill averaged over about a second, not to the fill of
/// one period, and corrects at most once every `MIN_GAP` periods (about
/// 500 ppm at 256-frame periods, far above a crystal's drift).
pub struct DriftControl {
    target: f32,
    band: f32,
    alpha: f32,
    avg: Option<f32>,
    cooldown: u32,
}

impl DriftControl {
    const MIN_GAP: u32 = 8;

    /// `target` and `band` in frames; `periods_per_second` sets the
    /// averaging time (about one second).
    pub fn new(target: usize, band: usize, periods_per_second: f32) -> Self {
        Self {
            target: target as f32,
            band: band as f32,
            alpha: (1.0 / periods_per_second.max(1.0)).min(1.0),
            avg: None,
            cooldown: 0,
        }
    }

    /// Feeds the fill measured this period; returns the correction to apply.
    pub fn update(&mut self, fill: usize) -> Correction {
        let f = fill as f32;
        let avg = self.avg.map_or(f, |a| a + self.alpha * (f - a));
        self.avg = Some(avg);
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return Correction::None;
        }
        let c = if avg > self.target + self.band {
            Correction::Drop
        } else if avg < self.target - self.band {
            Correction::Repeat
        } else {
            return Correction::None;
        };
        self.cooldown = Self::MIN_GAP;
        // The correction moves the fill by one frame.
        self.avg = Some(avg + if c == Correction::Drop { -1.0 } else { 1.0 });
        c
    }
}

pub struct PumpConfig {
    pub period: usize,
    /// Target fill of the USB ring, in periods.
    pub target_periods: usize,
    pub stats: Arc<PumpStats>,
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
    let mut drift = DriftControl::new(target, band, pcm.config().rate as f32 / cfg.period as f32);
    let mut buf = vec![0i32; cfg.period * channels];
    let mut stretched = Vec::with_capacity((cfg.period + 1) * channels);

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
        cfg.stats.xruns.store(pcm.xruns(), Ordering::Relaxed);
        let n = frames * channels;
        for s in &mut buf[..n] {
            *s >>= 8; // S32_LE from applications → 24-bit
        }
        if let Ok(mut l) = levels.lock() {
            accumulate_peaks(&mut l.outputs, &buf[..n], channels);
        }
        let fill = (capacity - tx.slots()) / channels;
        let correction = if n >= channels {
            drift.update(fill)
        } else {
            Correction::None
        };
        let samples = match correction {
            Correction::Drop => {
                // Device clock slower: one frame less.
                PumpStats::bump(&cfg.stats.drops);
                stretch(&buf[..n], channels, frames - 1, &mut stretched);
                &stretched[..]
            }
            Correction::Repeat => {
                // Device clock faster: one frame more.
                PumpStats::bump(&cfg.stats.repeats);
                stretch(&buf[..n], channels, frames + 1, &mut stretched);
                &stretched[..]
            }
            Correction::None => &buf[..n],
        };
        for &s in samples {
            if tx.push(s).is_err() {
                break;
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
    let mut drift = DriftControl::new(target, band, pcm.config().rate as f32 / cfg.period as f32);
    let mut buf = vec![0i32; (cfg.period + 1) * channels];
    let mut stretched = Vec::with_capacity(cfg.period * channels);

    // Fill the loopback buffer with silence first. Otherwise its empty
    // buffer swallows the ring's target at once and the ring takes seconds
    // of repeated frames to get back to it.
    let periods = pcm.config().periods.max(2) as usize;
    let silence = vec![0i32; (periods - 1) * cfg.period * channels];
    if let Err(e) = pcm.write(&silence) {
        if !is_peer_mismatch(&e) {
            return Err(e);
        }
    }

    // Wait until the device has delivered the target amount.
    let started = Instant::now();
    while rx.slots() / channels < target && !stop.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(2) {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    // The USB stream started before this pump, so the ring may already hold
    // much more: cut it to the target once, before anyone listens.
    let excess = (rx.slots() / channels).saturating_sub(target);
    if excess > 0 {
        if let Ok(chunk) = rx.read_chunk(excess * channels) {
            chunk.commit_all();
        }
    }
    while !stop.load(Ordering::Relaxed) {
        let fill = rx.slots() / channels;
        if fill < cfg.period {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let (take, out) = match drift.update(fill) {
            Correction::Drop if fill > cfg.period => {
                PumpStats::bump(&cfg.stats.drops);
                (cfg.period + 1, cfg.period) // drop one frame
            }
            Correction::Repeat => {
                PumpStats::bump(&cfg.stats.repeats);
                (cfg.period - 1, cfg.period) // repeat one frame
            }
            _ => (cfg.period, cfg.period),
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
        let out_buf: &mut [i32] = if take != out {
            stretch(&buf[..take * channels], channels, out, &mut stretched);
            &mut stretched[..]
        } else {
            &mut buf[..out * channels]
        };
        if let Ok(mut l) = levels.lock() {
            accumulate_peaks(&mut l.inputs, out_buf, channels);
        }
        for s in out_buf.iter_mut() {
            *s <<= 8; // 24-bit → S32_LE for applications
        }
        let written = pcm.write(out_buf);
        cfg.stats.xruns.store(pcm.xruns(), Ordering::Relaxed);
        if let Err(e) = written {
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

    const PERIOD: usize = 256;
    const TARGET: usize = 4 * PERIOD;
    const BAND: usize = 2 * PERIOD;
    const PPS: f32 = 44_100.0 / PERIOD as f32;

    fn control() -> DriftControl {
        DriftControl::new(TARGET, BAND, PPS)
    }

    #[test]
    fn steady_fill_needs_no_correction() {
        let mut d = control();
        assert!((0..10_000).all(|_| d.update(TARGET) == Correction::None));
    }

    #[test]
    fn scheduling_jitter_is_not_drift() {
        // Bursts of up to 3 periods either side of the target, as a late
        // thread produces: the average stays put, so nothing is corrected.
        let mut d = control();
        let pattern = [0isize, 3, -2, 1, -3, 2, 0, -1];
        let n = (0..20_000)
            .map(|i| {
                let fill = TARGET as isize + pattern[i % pattern.len()] * PERIOD as isize;
                d.update(fill as usize)
            })
            .filter(|c| *c != Correction::None)
            .count();
        assert_eq!(n, 0);
    }

    #[test]
    fn real_drift_is_followed_at_its_own_rate() {
        // The device consumes 200 ppm faster than the loopback delivers:
        // the ring loses 8.8 frames per second until repeats balance it.
        let mut d = control();
        let mut fill = TARGET as f32;
        let mut repeats = 0;
        let seconds = 600.0;
        for _ in 0..(seconds * PPS) as usize {
            fill -= 44_100.0 * 200e-6 / PPS;
            if d.update(fill as usize) == Correction::Repeat {
                fill += 1.0;
                repeats += 1;
            }
        }
        let rate = repeats as f32 / seconds;
        assert!((rate - 8.82).abs() < 1.0, "{rate} repeats/s");
        assert!(fill > (TARGET - BAND) as f32 - PERIOD as f32, "fill {fill}");
    }

    #[test]
    fn corrections_are_spaced() {
        let mut d = control();
        let out: Vec<_> = (0..100).map(|_| d.update(TARGET * 4)).collect();
        let drops: Vec<_> = out
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == Correction::Drop)
            .map(|(i, _)| i)
            .collect();
        assert!(
            drops
                .windows(2)
                .all(|w| w[1] - w[0] > DriftControl::MIN_GAP as usize)
        );
        assert!(!drops.is_empty());
    }

    /// Largest jump between consecutive samples of channel 0.
    fn max_step(x: &[i32], channels: usize) -> i64 {
        x.chunks(channels)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1][0] as i64 - w[0][0] as i64).abs())
            .max()
            .unwrap_or(0)
    }

    fn sine(frames: usize, channels: usize) -> Vec<i32> {
        (0..frames)
            .flat_map(|i| {
                let v = (4_000_000.0
                    * (i as f64 * std::f64::consts::TAU * 5_000.0 / 44_100.0).sin())
                    as i32;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    #[test]
    fn stretch_keeps_the_ends_and_the_length() {
        let src = sine(257, 2);
        let mut out = Vec::new();
        stretch(&src, 2, 256, &mut out);
        assert_eq!(out.len(), 256 * 2);
        assert_eq!(&out[..2], &src[..2]);
        assert_eq!(&out[out.len() - 2..], &src[src.len() - 2..]);
        stretch(&src[..255 * 2], 2, 256, &mut out);
        assert_eq!(out.len(), 256 * 2);
    }

    #[test]
    fn stretching_leaves_no_step_in_the_waveform() {
        // Removing a frame by cutting it makes a jump up to twice a normal
        // step; stretching keeps every step at the signal's own size.
        let src = sine(257, 1);
        let normal = max_step(&src, 1);
        let mut cut = src.clone();
        cut.remove(128);
        let mut out = Vec::new();
        stretch(&src, 1, 256, &mut out);
        assert!(max_step(&cut, 1) > normal * 3 / 2);
        assert!(max_step(&out, 1) <= normal + normal / 100);
    }

    #[test]
    fn peaks_per_channel() {
        let mut p = Vec::new();
        accumulate_peaks(&mut p, &[0, 4_194_304, -8_388_608, 0], 2);
        assert_eq!(p, vec![1.0, 0.5]);
    }
}
