//! Minimal ALSA PCM access through the kernel ioctl interface, without
//! alsa-lib. Only what the bridge needs: interleaved S32_LE read/write on a
//! hardware PCM device such as the `snd-aloop` loopback card.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io,
    mem::size_of,
    os::fd::AsRawFd,
    path::Path,
};

const PARAM_ACCESS: usize = 0;
const PARAM_FORMAT: usize = 1;
const PARAM_SUBFORMAT: usize = 2;
const PARAM_FIRST_INTERVAL: usize = 8;
const PARAM_CHANNELS: usize = 10;
const PARAM_RATE: usize = 11;
const PARAM_PERIOD_SIZE: usize = 13;
const PARAM_PERIODS: usize = 15;

const ACCESS_RW_INTERLEAVED: u32 = 3;
const FORMAT_S32_LE: u32 = 10;
const SUBFORMAT_STD: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Mask {
    bits: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Interval {
    min: u32,
    max: u32,
    /// Bit field: openmin, openmax, integer, empty (LSB first).
    flags: u32,
}

const INTERVAL_INTEGER: u32 = 1 << 2;

#[repr(C)]
struct HwParams {
    flags: u32,
    masks: [Mask; 3],
    mres: [Mask; 5],
    intervals: [Interval; 12],
    ires: [Interval; 9],
    rmask: u32,
    cmask: u32,
    info: u32,
    msbits: u32,
    rate_num: u32,
    rate_den: u32,
    fifo_size: libc::c_ulong,
    reserved: [u8; 64],
}

impl HwParams {
    /// Equivalent of `snd_pcm_hw_params_any`.
    fn any() -> Self {
        Self {
            flags: 0,
            masks: [Mask { bits: [!0; 8] }; 3],
            mres: [Mask { bits: [0; 8] }; 5],
            intervals: [Interval {
                min: 0,
                max: u32::MAX,
                flags: 0,
            }; 12],
            ires: [Interval {
                min: 0,
                max: 0,
                flags: 0,
            }; 9],
            rmask: !0,
            cmask: 0,
            info: !0,
            msbits: 0,
            rate_num: 0,
            rate_den: 0,
            fifo_size: 0,
            reserved: [0; 64],
        }
    }

    fn set_mask(&mut self, param: usize, value: u32) {
        let m = &mut self.masks[param];
        m.bits = [0; 8];
        m.bits[(value / 32) as usize] = 1 << (value % 32);
    }

    fn set_int(&mut self, param: usize, value: u32) {
        let i = &mut self.intervals[param - PARAM_FIRST_INTERVAL];
        i.min = value;
        i.max = value;
        i.flags = INTERVAL_INTEGER;
    }

    fn get_int(&self, param: usize) -> u32 {
        self.intervals[param - PARAM_FIRST_INTERVAL].min
    }
}

#[repr(C)]
struct XferI {
    result: libc::c_long,
    buf: *mut c_void,
    frames: libc::c_ulong,
}

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((b'A' as u32) << 8) | nr
}

const IOCTL_HW_PARAMS: u32 = ioc(IOC_READ | IOC_WRITE, 0x11, size_of::<HwParams>());
const IOCTL_PREPARE: u32 = ioc(IOC_NONE, 0x40, 0);
const IOCTL_DROP: u32 = ioc(IOC_NONE, 0x43, 0);
const IOCTL_WRITEI: u32 = ioc(IOC_WRITE, 0x50, size_of::<XferI>());
const IOCTL_READI: u32 = ioc(IOC_READ, 0x51, size_of::<XferI>());

/// Stream direction of a PCM device node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Playback,
    Capture,
}

/// Negotiated configuration.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub channels: u32,
    pub rate: u32,
    pub period: u32,
    pub periods: u32,
}

/// An open PCM device in interleaved S32_LE mode.
pub struct Pcm {
    file: File,
    direction: Direction,
    config: Config,
}

impl Pcm {
    /// Opens `/dev/snd/pcmC{card}D{device}{p|c}` and configures it.
    pub fn open(card: u32, device: u32, direction: Direction, want: Config) -> io::Result<Self> {
        let suffix = match direction {
            Direction::Playback => 'p',
            Direction::Capture => 'c',
        };
        let path = format!("/dev/snd/pcmC{card}D{device}{suffix}");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new(&path))
            .map_err(|e| io::Error::new(e.kind(), format!("{path}: {e}")))?;
        let mut hw = HwParams::any();
        hw.set_mask(PARAM_ACCESS, ACCESS_RW_INTERLEAVED);
        hw.set_mask(PARAM_FORMAT, FORMAT_S32_LE);
        hw.set_mask(PARAM_SUBFORMAT, SUBFORMAT_STD);
        hw.set_int(PARAM_CHANNELS, want.channels);
        hw.set_int(PARAM_RATE, want.rate);
        hw.set_int(PARAM_PERIOD_SIZE, want.period);
        hw.set_int(PARAM_PERIODS, want.periods);
        let mut pcm = Self {
            file,
            direction,
            config: want,
        };
        pcm.ioctl(IOCTL_HW_PARAMS, (&mut hw as *mut HwParams).cast())
            .map_err(|e| io::Error::new(e.kind(), format!("{path}: hw_params: {e}")))?;
        pcm.config = Config {
            channels: hw.get_int(PARAM_CHANNELS),
            rate: hw.get_int(PARAM_RATE),
            period: hw.get_int(PARAM_PERIOD_SIZE),
            periods: hw.get_int(PARAM_PERIODS),
        };
        pcm.prepare()?;
        Ok(pcm)
    }

    pub fn config(&self) -> Config {
        self.config
    }

    fn ioctl(&self, request: u32, arg: *mut c_void) -> io::Result<i32> {
        // SAFETY: callers pass the argument type each request expects.
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, arg) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    pub fn prepare(&self) -> io::Result<()> {
        self.ioctl(IOCTL_PREPARE, std::ptr::null_mut()).map(|_| ())
    }

    /// Transfers whole frames, recovering from over/underruns. Returns the
    /// number of frames moved (blocking until at least some are).
    fn transfer(&self, buf: *mut c_void, frames: usize) -> io::Result<usize> {
        let request = match self.direction {
            Direction::Playback => IOCTL_WRITEI,
            Direction::Capture => IOCTL_READI,
        };
        loop {
            let mut x = XferI {
                result: 0,
                buf,
                frames: frames as libc::c_ulong,
            };
            match self.ioctl(request, (&mut x as *mut XferI).cast()) {
                Ok(_) => return Ok(x.result as usize),
                Err(e) if matches!(e.raw_os_error(), Some(libc::EPIPE | libc::ESTRPIPE)) => {
                    log::debug!("{:?} xrun, re-preparing", self.direction);
                    self.prepare()?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Writes interleaved frames (`samples.len()` must be a multiple of the
    /// channel count).
    pub fn write(&self, samples: &[i32]) -> io::Result<usize> {
        let frames = samples.len() / self.config.channels as usize;
        self.transfer(samples.as_ptr() as *mut c_void, frames)
    }

    /// Reads interleaved frames into `samples`.
    pub fn read(&self, samples: &mut [i32]) -> io::Result<usize> {
        let frames = samples.len() / self.config.channels as usize;
        self.transfer(samples.as_mut_ptr().cast(), frames)
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        let _ = self.ioctl(IOCTL_DROP, std::ptr::null_mut());
    }
}

/// Finds an ALSA card by its id (the bracketed name in /proc/asound/cards).
pub fn find_card(id: &str) -> Option<u32> {
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    for line in cards.lines() {
        let Some((num, rest)) = line.trim_start().split_once(' ') else {
            continue;
        };
        let Ok(num) = num.parse::<u32>() else {
            continue;
        };
        let name = rest.trim_start().trim_start_matches('[');
        let card_id = name.split(']').next().unwrap_or("").trim();
        if card_id == id {
            return Some(num);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_matches_kernel_headers() {
        // Values printed from <sound/asound.h> on x86_64 Linux.
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<HwParams>(), 608);
            assert_eq!(std::mem::offset_of!(HwParams, intervals), 260);
            assert_eq!(std::mem::offset_of!(HwParams, rmask), 512);
            assert_eq!(std::mem::offset_of!(HwParams, fifo_size), 536);
            assert_eq!(size_of::<XferI>(), 24);
            assert_eq!(IOCTL_HW_PARAMS, 0xc260_4111);
            assert_eq!(IOCTL_WRITEI, 0x4018_4150);
            assert_eq!(IOCTL_READI, 0x8018_4151);
        }
        assert_eq!(IOCTL_PREPARE, 0x4140);
        assert_eq!(IOCTL_DROP, 0x4143);
    }

    #[test]
    fn mask_bit_for_s32() {
        let mut hw = HwParams::any();
        hw.set_mask(PARAM_FORMAT, FORMAT_S32_LE);
        assert_eq!(hw.masks[PARAM_FORMAT].bits[0], 1 << 10);
        assert!(hw.masks[PARAM_FORMAT].bits[1..].iter().all(|&b| b == 0));
    }
}
