//! Full-duplex isochronous streaming.
//!
//! [`DuplexStream`] runs the USB engine on its own thread and exchanges
//! interleaved 24-bit samples (`i32` in `-0x80_0000..=0x7f_ffff`) with the
//! application through two lock-free ring buffers:
//!
//! * [`DuplexStream::capture`]: frames recorded from the device's inputs;
//! * [`DuplexStream::playback`]: frames to send to the device's outputs.
//!   When it runs dry the engine sends silence.
//!
//! Playback packet sizes follow the frame counts of the capture packets
//! (implicit feedback, since the device is the clock master). Until capture
//! data arrives, they follow the nominal rate.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

use ua101_protocol::descriptors::StreamFormat;

use crate::{Error, Result, Ua101};

#[cfg(target_os = "linux")]
mod linux;

/// Streaming parameters.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// URBs kept in flight per direction.
    pub urbs: usize,
    /// Isochronous packets per URB (8 at high speed with `bInterval = 1`
    /// means one URB per millisecond).
    pub packets_per_urb: usize,
    /// Capacity of each ring buffer, in frames.
    pub ring_frames: usize,
    /// Give interfaces back to the kernel driver when the stream stops.
    pub reattach_kernel_driver: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            urbs: 8,
            packets_per_urb: 8,
            ring_frames: 96_000,
            reattach_kernel_driver: true,
        }
    }
}

/// Counters updated by the streaming thread.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub capture_frames: AtomicU64,
    pub playback_frames: AtomicU64,
    pub capture_overruns: AtomicU64,
    pub playback_underruns: AtomicU64,
    pub packet_errors: AtomicU64,
}

/// Snapshot of the stream counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Frames received from the device.
    pub capture_frames: u64,
    /// Frames sent to the device (including silence).
    pub playback_frames: u64,
    /// Captured frames dropped because the capture ring was full.
    pub capture_overruns: u64,
    /// Frames of silence sent because the playback ring ran dry after the
    /// application started queueing audio.
    pub playback_underruns: u64,
    /// Isochronous packets that completed with an error.
    pub packet_errors: u64,
}

impl Counters {
    fn snapshot(&self) -> StreamStats {
        StreamStats {
            capture_frames: self.capture_frames.load(Ordering::Relaxed),
            playback_frames: self.playback_frames.load(Ordering::Relaxed),
            capture_overruns: self.capture_overruns.load(Ordering::Relaxed),
            playback_underruns: self.playback_underruns.load(Ordering::Relaxed),
            packet_errors: self.packet_errors.load(Ordering::Relaxed),
        }
    }
}

/// A running full-duplex stream. Dropping it stops the stream.
pub struct DuplexStream {
    // `Option` only so `split` can move them out; always `Some` otherwise.
    capture: Option<rtrb::Consumer<i32>>,
    playback: Option<rtrb::Producer<i32>>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<Result<()>>>,
    playback_format: StreamFormat,
    capture_format: StreamFormat,
}

impl DuplexStream {
    /// Interleaved captured samples (`capture_format().channels` per frame).
    pub fn capture(&mut self) -> &mut rtrb::Consumer<i32> {
        self.capture.as_mut().expect("rings present until split")
    }

    /// Interleaved samples to play (`playback_format().channels` per frame).
    pub fn playback(&mut self) -> &mut rtrb::Producer<i32> {
        self.playback.as_mut().expect("rings present until split")
    }

    pub fn playback_format(&self) -> &StreamFormat {
        &self.playback_format
    }

    pub fn capture_format(&self) -> &StreamFormat {
        &self.capture_format
    }

    /// Sample rate of the running stream.
    pub fn sample_rate(&self) -> u32 {
        self.capture_format.sample_rate
    }

    /// Current counters.
    pub fn stats(&self) -> StreamStats {
        self.counters.snapshot()
    }

    /// `false` once the streaming thread has exited (stopped or failed).
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Splits the stream into a control handle and its two ring ends, so
    /// capture and playback can be serviced from different threads.
    pub fn split(mut self) -> (StreamControl, rtrb::Consumer<i32>, rtrb::Producer<i32>) {
        let capture = self.capture.take().expect("rings present until split");
        let playback = self.playback.take().expect("rings present until split");
        let control = StreamControl {
            stop: self.stop.clone(),
            counters: self.counters.clone(),
            thread: self.thread.take(),
            playback_format: self.playback_format,
            capture_format: self.capture_format,
        };
        (control, capture, playback)
    }

    /// Stops the stream and returns the final counters, or the error that
    /// ended it.
    pub fn stop(mut self) -> Result<StreamStats> {
        self.shutdown()?;
        Ok(self.counters.snapshot())
    }

    fn shutdown(&mut self) -> Result<()> {
        // After `split`, the thread belongs to the `StreamControl`: leave the
        // shared stop flag alone.
        let Some(t) = self.thread.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Release);
        t.join().map_err(|_| Error::ThreadPanicked)?
    }
}

impl Drop for DuplexStream {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Control half of a [`DuplexStream`] returned by [`DuplexStream::split`].
/// Dropping it stops the stream.
pub struct StreamControl {
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<Result<()>>>,
    playback_format: StreamFormat,
    capture_format: StreamFormat,
}

impl StreamControl {
    pub fn playback_format(&self) -> &StreamFormat {
        &self.playback_format
    }

    pub fn capture_format(&self) -> &StreamFormat {
        &self.capture_format
    }

    pub fn stats(&self) -> StreamStats {
        self.counters.snapshot()
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Stops the stream and returns the final counters, or the error that
    /// ended it.
    pub fn stop(mut self) -> Result<StreamStats> {
        self.shutdown()?;
        Ok(self.counters.snapshot())
    }

    fn shutdown(&mut self) -> Result<()> {
        // After `split`, the thread belongs to the `StreamControl`: leave the
        // shared stop flag alone.
        let Some(t) = self.thread.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Release);
        t.join().map_err(|_| Error::ThreadPanicked)?
    }
}

impl Drop for StreamControl {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[allow(unused_variables)]
pub(crate) fn start(
    device: &Ua101,
    playback: StreamFormat,
    capture: StreamFormat,
    config: StreamConfig,
) -> Result<DuplexStream> {
    #[cfg(target_os = "linux")]
    {
        let ring_samples_cap = config.ring_frames * capture.channels.max(1) as usize;
        let ring_samples_play = config.ring_frames * playback.channels.max(1) as usize;
        let (cap_tx, cap_rx) = rtrb::RingBuffer::<i32>::new(ring_samples_cap.max(1));
        let (play_tx, play_rx) = rtrb::RingBuffer::<i32>::new(ring_samples_play.max(1));
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());

        let engine = linux::Engine::new(
            device,
            playback,
            capture,
            &config,
            cap_tx,
            play_rx,
            stop.clone(),
            counters.clone(),
        )?;
        let thread = std::thread::Builder::new()
            .name("ua101-stream".into())
            .spawn(move || engine.run())?;
        Ok(DuplexStream {
            capture: Some(cap_rx),
            playback: Some(play_tx),
            stop,
            counters,
            thread: Some(thread),
            playback_format: playback,
            capture_format: capture,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::Unsupported(
            "isochronous streaming is only implemented for Linux (usbfs) so far",
        ))
    }
}
