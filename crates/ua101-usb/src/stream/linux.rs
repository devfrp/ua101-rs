//! Linux streaming engine on top of `usbfs` isochronous URBs.

#![allow(unsafe_code)]

use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use log::{debug, warn};
use ua101_protocol::{
    INTF_CAPTURE, INTF_PLAYBACK, STREAMING_ALT_SETTING,
    descriptors::StreamFormat,
    packets::{FrameClock, packets_per_second},
    pcm::{self, WireFormat},
};

use super::{Counters, StreamConfig};
use crate::{
    Error, Result, Ua101,
    usbfs::{IsoUrb, MAX_ISO_PACKETS, UsbFs},
};

/// How long playback waits for the first capture packets before falling
/// back to the nominal rate.
const FEEDBACK_WARMUP: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Capture,
    Playback,
}

struct Slot {
    urb: Box<IsoUrb>,
    buffer: Vec<u8>,
    kind: Kind,
    in_flight: bool,
}

pub(crate) struct Engine {
    fs: UsbFs,
    slots: Vec<Slot>,
    playback: StreamFormat,
    capture: StreamFormat,
    play_wire: WireFormat,
    cap_wire: WireFormat,
    packets_per_urb: usize,
    play_max_frames: u32,
    clock: FrameClock,
    feedback: VecDeque<u32>,
    feedback_enabled: bool,
    feedback_cap: usize,
    /// Set once the application has queued playback data; silence sent
    /// before that is not an underrun.
    playback_primed: bool,
    cap_tx: rtrb::Producer<i32>,
    play_rx: rtrb::Consumer<i32>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    scratch: Vec<i32>,
    lengths: Vec<u32>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        device: &Ua101,
        playback: StreamFormat,
        capture: StreamFormat,
        config: &StreamConfig,
        cap_tx: rtrb::Producer<i32>,
        play_rx: rtrb::Consumer<i32>,
        stop: Arc<AtomicBool>,
        counters: Arc<Counters>,
    ) -> Result<Self> {
        let play_wire = WireFormat::from_subframe_size(playback.subframe_size)
            .ok_or(Error::UnsupportedSubframe(playback.subframe_size))?;
        let cap_wire = WireFormat::from_subframe_size(capture.subframe_size)
            .ok_or(Error::UnsupportedSubframe(capture.subframe_size))?;

        let info = device.info();
        let mut fs = UsbFs::open(info.busnum(), info.device_address())?;
        fs.set_reattach(config.reattach_kernel_driver);
        for intf in [INTF_PLAYBACK, INTF_CAPTURE] {
            if fs.detach_kernel_driver(intf as u32)? {
                debug!("detached kernel driver from interface {intf}");
            }
            fs.claim_interface(intf as u32)?;
            fs.set_interface(intf as u32, STREAMING_ALT_SETTING as u32)?;
        }

        let high_speed = device.is_high_speed();
        let pps = packets_per_second(high_speed, playback.endpoint.interval);
        let clock = FrameClock::new(playback.sample_rate, pps);
        let play_max_frames = playback
            .max_frames_per_packet()
            .min(clock.max_packet_frames() + 1)
            .max(1);
        let packets_per_urb = config.packets_per_urb.clamp(1, MAX_ISO_PACKETS);
        let urbs = config.urbs.max(2);
        let feedback_enabled = capture.endpoint.interval == playback.endpoint.interval
            && capture.sample_rate == playback.sample_rate;

        let cap_packet_bytes = capture.endpoint.bytes_per_interval() as usize;
        let play_packet_bytes = play_max_frames as usize * playback.frame_bytes();
        let mut slots = Vec::with_capacity(urbs * 2);
        for _ in 0..urbs {
            let tag = slots.len();
            slots.push(Slot {
                urb: IsoUrb::new(capture.endpoint.address, tag),
                buffer: vec![0; cap_packet_bytes * packets_per_urb],
                kind: Kind::Capture,
                in_flight: false,
            });
        }
        for _ in 0..urbs {
            let tag = slots.len();
            slots.push(Slot {
                urb: IsoUrb::new(playback.endpoint.address, tag),
                buffer: vec![0; play_packet_bytes * packets_per_urb],
                kind: Kind::Playback,
                in_flight: false,
            });
        }

        debug!(
            "stream: {} Hz, {pps} packets/s, play {}ch x{}B (max {play_max_frames} frames/packet), \
             capture {}ch x{}B, feedback {feedback_enabled}",
            playback.sample_rate,
            playback.channels,
            playback.subframe_size,
            capture.channels,
            capture.subframe_size
        );

        let max_channels = playback.channels.max(capture.channels) as usize;
        Ok(Self {
            fs,
            slots,
            playback,
            capture,
            play_wire,
            cap_wire,
            packets_per_urb,
            play_max_frames,
            clock,
            feedback: VecDeque::with_capacity(urbs * packets_per_urb * 4),
            feedback_enabled,
            feedback_cap: urbs * packets_per_urb * 4,
            playback_primed: false,
            cap_tx,
            play_rx,
            stop,
            counters,
            scratch: vec![0; cap_packet_bytes.max(play_packet_bytes) * max_channels.max(1)],
            lengths: vec![0; packets_per_urb],
        })
    }

    pub(crate) fn run(mut self) -> Result<()> {
        let result = self.stream_loop();
        if let Err(e) = &result {
            warn!("stream stopped: {e}");
        }
        self.cancel_all();
        result
    }

    fn stream_loop(&mut self) -> Result<()> {
        for i in 0..self.slots.len() {
            if self.slots[i].kind == Kind::Capture {
                self.submit_capture(i)?;
            }
        }
        let started = Instant::now();
        let warmup_packets = self.packets_per_urb * 2;
        let mut playback_started = false;

        while !self.stop.load(Ordering::Acquire) {
            if !playback_started
                && (self.feedback.len() >= warmup_packets
                    || !self.feedback_enabled
                    || started.elapsed() >= FEEDBACK_WARMUP)
            {
                for i in 0..self.slots.len() {
                    if self.slots[i].kind == Kind::Playback {
                        self.submit_playback(i)?;
                    }
                }
                playback_started = true;
            }

            match self.fs.wait(50) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => return Err(Error::Disconnected),
            }
            loop {
                let ptr = match self.fs.reap_nonblocking() {
                    Ok(Some(p)) => p,
                    Ok(None) => break,
                    Err(e) if e.raw_os_error() == Some(libc::ENODEV) => {
                        return Err(Error::Disconnected);
                    }
                    Err(e) => return Err(e.into()),
                };
                self.complete(ptr)?;
            }
        }
        Ok(())
    }

    fn complete(&mut self, ptr: *mut IsoUrb) -> Result<()> {
        // SAFETY: the kernel hands back a pointer we submitted; it points
        // into a live boxed slot.
        let tag = unsafe { (*ptr).tag() };
        let Some(slot) = self.slots.get_mut(tag) else {
            warn!("reaped unknown URB tag {tag}");
            return Ok(());
        };
        debug_assert!(std::ptr::eq(ptr, &*slot.urb));
        slot.in_flight = false;

        let status = slot.urb.status();
        if status == -libc::ENODEV || status == -libc::ESHUTDOWN {
            return Err(Error::Disconnected);
        }
        if status != 0 && status != -libc::EXDEV {
            // -EXDEV only means some packets failed; they are counted below.
            debug!("URB {tag} completed with status {status}");
        }
        let errors = slot.urb.error_count().max(0) as u64;
        if errors > 0 {
            self.counters
                .packet_errors
                .fetch_add(errors, Ordering::Relaxed);
        }

        match slot.kind {
            Kind::Capture => {
                self.consume_capture(tag);
                self.submit_capture(tag)
            }
            Kind::Playback => self.submit_playback(tag),
        }
    }

    fn consume_capture(&mut self, tag: usize) {
        let slot = &self.slots[tag];
        let frame_bytes = self.capture.frame_bytes();
        let channels = self.capture.channels as usize;
        let mut offset = 0usize;
        let mut total_frames = 0u64;
        let mut overruns = 0u64;
        for desc in slot.urb.packets() {
            let start = offset;
            offset += desc.length as usize;
            let frames = if desc.status == 0 && frame_bytes > 0 {
                desc.actual_length as usize / frame_bytes
            } else {
                0
            };
            if self.feedback_enabled {
                if self.feedback.len() >= self.feedback_cap {
                    self.feedback.pop_front();
                }
                self.feedback.push_back(frames as u32);
            }
            if frames == 0 {
                continue;
            }
            let samples = frames * channels;
            let bytes = &slot.buffer[start..start + frames * frame_bytes];
            let n = pcm::decode(self.cap_wire, bytes, &mut self.scratch[..samples]);
            if self.cap_tx.slots() >= n {
                for &s in &self.scratch[..n] {
                    let _ = self.cap_tx.push(s);
                }
            } else {
                overruns += frames as u64;
            }
            total_frames += frames as u64;
        }
        self.counters
            .capture_frames
            .fetch_add(total_frames, Ordering::Relaxed);
        if overruns > 0 {
            self.counters
                .capture_overruns
                .fetch_add(overruns, Ordering::Relaxed);
        }
    }

    fn submit_capture(&mut self, tag: usize) -> Result<()> {
        let len = self.capture.endpoint.bytes_per_interval();
        self.lengths.fill(len);
        let slot = &mut self.slots[tag];
        slot.urb.prepare(&mut slot.buffer, &self.lengths);
        // SAFETY: the slot's box and buffer live until the URB is reaped
        // (see cancel_all).
        unsafe { self.fs.submit(&mut slot.urb)? };
        slot.in_flight = true;
        Ok(())
    }

    fn submit_playback(&mut self, tag: usize) -> Result<()> {
        let frame_bytes = self.playback.frame_bytes();
        let channels = self.playback.channels as usize;
        let mut offset = 0usize;
        let mut total_frames = 0u64;
        let mut underruns = 0u64;
        for i in 0..self.packets_per_urb {
            let frames = self
                .feedback
                .pop_front()
                .unwrap_or_else(|| self.clock.next_packet())
                .min(self.play_max_frames) as usize;
            let samples = frames * channels;
            let available = self.play_rx.slots().min(samples);
            if available > 0 {
                self.playback_primed = true;
            }
            for (k, s) in self.scratch[..samples].iter_mut().enumerate() {
                *s = if k < available {
                    self.play_rx.pop().unwrap_or(0)
                } else {
                    0
                };
            }
            if self.playback_primed && available < samples && channels > 0 {
                underruns += ((samples - available) / channels) as u64;
            }
            let bytes = frames * frame_bytes;
            let slot = &mut self.slots[tag];
            pcm::encode(
                self.play_wire,
                &self.scratch[..samples],
                &mut slot.buffer[offset..offset + bytes],
            );
            self.lengths[i] = bytes as u32;
            offset += bytes;
            total_frames += frames as u64;
        }
        let slot = &mut self.slots[tag];
        slot.urb.prepare(&mut slot.buffer, &self.lengths);
        // SAFETY: as in submit_capture.
        unsafe { self.fs.submit(&mut slot.urb)? };
        slot.in_flight = true;
        self.counters
            .playback_frames
            .fetch_add(total_frames, Ordering::Relaxed);
        if underruns > 0 {
            self.counters
                .playback_underruns
                .fetch_add(underruns, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Cancels every in-flight URB and waits for the kernel to return them.
    fn cancel_all(&mut self) {
        for slot in &mut self.slots {
            if slot.in_flight {
                let _ = self.fs.discard(&mut *slot.urb);
            }
        }
        while self.slots.iter().any(|s| s.in_flight) {
            match self.fs.reap_blocking() {
                Ok(ptr) => {
                    // SAFETY: see complete().
                    let tag = unsafe { (*ptr).tag() };
                    if let Some(slot) = self.slots.get_mut(tag) {
                        slot.in_flight = false;
                    }
                }
                Err(e) => {
                    warn!("could not reap cancelled URBs: {e}");
                    break;
                }
            }
        }
        // Never free memory the kernel may still write to.
        for slot in std::mem::take(&mut self.slots) {
            if slot.in_flight {
                let _ = ManuallyDrop::new(slot);
            }
        }
    }
}
