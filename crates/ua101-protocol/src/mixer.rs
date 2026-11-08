//! Parameter map of the UA-101's internal monitor mixer and device state.
//!
//! Every parameter is written with a Roland DT1 message on the control
//! cable (see [`crate::sysex`]). The addresses were read from the Windows
//! driver, which builds one message per control-panel action.
//!
//! Evidence levels, as in `docs/protocol.md`:
//!
//! * **observed (device)**: the front-panel status block (`00 01 00 xx`),
//!   which the unit sends by itself when a switch moves;
//! * **inferred**: everything else. The address layout is certain, but the
//!   name of each parameter comes from the control panel's manual and has
//!   not been confirmed on hardware yet.

use crate::sysex::{self, Message};

/// A mixer bus (a block of channel strips).
///
/// The driver numbers buses `0..=2` and stores bus `n` at address block
/// `0x03 + n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bus {
    /// MONITOR OUT L/R (2 channels).
    MonitorOut,
    /// INPUT MONITOR 1–10.
    Input,
    /// OUTPUT (WAVE) MONITOR 1–10.
    Output,
}

impl Bus {
    pub const ALL: [Bus; 3] = [Bus::Input, Bus::Output, Bus::MonitorOut];

    /// Index used by the driver.
    pub const fn index(self) -> u8 {
        match self {
            Bus::MonitorOut => 0,
            Bus::Input => 1,
            Bus::Output => 2,
        }
    }

    /// Address block holding this bus's channel parameters.
    pub const fn block(self) -> u8 {
        0x03 + self.index()
    }

    /// Number of channel strips.
    pub const fn channels(self) -> u8 {
        match self {
            Bus::MonitorOut => 2,
            Bus::Input | Bus::Output => 10,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Bus::MonitorOut => "MONITOR OUT",
            Bus::Input => "INPUT",
            Bus::Output => "OUTPUT",
        }
    }
}

/// Channel-strip parameter slots (last address byte).
pub mod slot {
    /// Stereo link of the pair starting at this (even) channel (inferred).
    pub const LINK: u8 = 0x01;
    /// MUTE, 1 byte: `01` mutes, `00` unmutes. **Observed on hardware.**
    /// For OUTPUT strips this decides whether the output reaches MONITOR
    /// OUT.
    pub const MUTE: u8 = 0x02;
    /// Pan, 16-bit as 4 nibbles: `0x0000` left, `0x4000` centre, `0x7fff`
    /// right. Moving it was heard as a balance change on hardware.
    pub const PAN: u8 = 0x03;
    /// Volume, linear 24-bit gain as 6 nibbles. **Observed on hardware**:
    /// `0x400000` on both a channel and MONITOR OUT saturates, `0x100000`
    /// on both gives a clean, normal level.
    pub const VOLUME: u8 = 0x07;
}

/// Pan positions.
pub const PAN_LEFT: u16 = 0x0000;
pub const PAN_CENTER: u16 = 0x4000;
pub const PAN_RIGHT: u16 = 0x7fff;

/// Largest volume value (24-bit).
pub const VOLUME_MAX: u32 = 0x7f_ffff;
/// Working assumption for 0 dB: the level heard as normal on hardware.
pub const VOLUME_UNITY: u32 = 0x10_0000;

/// Splits `value` into `N` nibbles, most significant first, as the driver
/// does for multi-byte parameters.
pub fn nibbles<const N: usize>(value: u32) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        let shift = 4 * (N - 1 - i);
        *b = ((value >> shift) & 0x0f) as u8;
    }
    out
}

/// Reassembles nibbles produced by [`nibbles`].
pub fn from_nibbles(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, &b| (acc << 4) | (b & 0x0f) as u32)
}

/// Address of a channel parameter.
pub const fn channel_address(bus: Bus, channel: u8, slot: u8) -> [u8; 4] {
    [0x00, bus.block(), channel, slot]
}

/// MUTE of one channel (driver sub-command 23).
pub fn set_mute(bus: Bus, channel: u8, muted: bool) -> Message {
    sysex::dt1(channel_address(bus, channel, slot::MUTE), &[muted as u8])
}

/// Stereo link of channel pair `pair` (channels `2·pair` and `2·pair+1`;
/// driver sub-command 22).
pub fn set_link(bus: Bus, pair: u8, linked: bool) -> Message {
    sysex::dt1(channel_address(bus, pair * 2, slot::LINK), &[linked as u8])
}

/// Channel pan, 16-bit (driver sub-command 24).
pub fn set_pan(bus: Bus, channel: u8, value: u16) -> Message {
    sysex::dt1(
        channel_address(bus, channel, slot::PAN),
        &nibbles::<4>(value.min(0x7fff) as u32),
    )
}

/// Channel volume, linear 24-bit gain (driver sub-command 25).
pub fn set_volume(bus: Bus, channel: u8, value: u32) -> Message {
    sysex::dt1(
        channel_address(bus, channel, slot::VOLUME),
        &nibbles::<6>(value.min(VOLUME_MAX)),
    )
}

/// One channel strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub mute: bool,
    pub pan: u16,
    pub volume: u32,
}

/// Complete state of the monitor mixer. With SOFT CTRL on, the unit takes
/// its whole monitor mix from the computer and starts silent, so the host
/// must send everything (the Windows driver re-sends its state when the
/// unit reports a front-panel change).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixerState {
    /// Indexed by [`Bus::index`]; MONITOR OUT uses the first two entries.
    pub channels: [[Channel; 10]; 3],
    pub links: [[bool; 5]; 3],
}

impl MixerState {
    /// Safe starting point: playback (OUTPUT) reaches MONITOR OUT as stereo
    /// pairs at the normal level, inputs are muted (no feedback), MONITOR
    /// OUT L/R at the normal level.
    pub fn initial() -> Self {
        let ch = |mute, pan| Channel {
            mute,
            pan,
            volume: VOLUME_UNITY,
        };
        let mut channels = [[ch(false, PAN_CENTER); 10]; 3];
        let mut links = [[false; 5]; 3];
        let (out, inp) = (Bus::Output.index() as usize, Bus::Input.index() as usize);
        for (i, c) in channels[out].iter_mut().enumerate() {
            *c = ch(false, if i % 2 == 0 { PAN_LEFT } else { PAN_RIGHT });
        }
        channels[inp] = [ch(true, PAN_CENTER); 10];
        channels[Bus::MonitorOut.index() as usize][0] = ch(false, PAN_LEFT);
        channels[Bus::MonitorOut.index() as usize][1] = ch(false, PAN_RIGHT);
        links[Bus::Output.index() as usize] = [true; 5];
        links[Bus::MonitorOut.index() as usize][0] = true;
        Self { channels, links }
    }

    /// Updates the state from a DT1 written to the unit. Returns `false` if
    /// the address is not a mixer channel parameter.
    pub fn apply(&mut self, address: [u8; 4], data: &[u8]) -> bool {
        if address[0] != 0 || !(0x03..=0x05).contains(&address[1]) || data.is_empty() {
            return false;
        }
        let bus = (address[1] - 0x03) as usize;
        let ch = address[2] as usize;
        let Some(c) = self.channels[bus].get_mut(ch) else {
            return false;
        };
        match address[3] {
            slot::MUTE => c.mute = data[0] != 0,
            slot::PAN if data.len() >= 4 => c.pan = from_nibbles(&data[..4]) as u16,
            slot::VOLUME if data.len() >= 6 => c.volume = from_nibbles(&data[..6]),
            slot::LINK => {
                if let Some(l) = self.links[bus].get_mut(ch / 2) {
                    *l = data[0] != 0;
                }
            }
            _ => return false,
        }
        true
    }

    /// Calls `f` with every message needed to put the unit in this state.
    pub fn for_each_message(&self, mut f: impl FnMut(Message)) {
        for bus in Bus::ALL {
            let i = bus.index() as usize;
            let n = bus.channels() as usize;
            for pair in 0..n / 2 {
                f(set_link(bus, pair as u8, self.links[i][pair]));
            }
            for ch in 0..n {
                let c = self.channels[i][ch];
                f(set_mute(bus, ch as u8, c.mute));
                // MONITOR OUT has no pan in the unit's mixer memory.
                if bus != Bus::MonitorOut {
                    f(set_pan(bus, ch as u8, c.pan));
                }
                f(set_volume(bus, ch as u8, c.volume));
            }
        }
    }
}

/// Address of the complete mixer memory block. The unit answers an RQ1
/// for it with one 256-byte DT1 (**observed**), and the Windows driver sends
/// the same block to restore the whole mixer (sub-commands 8 and 13).
pub const BLOCK_ADDRESS: [u8; 4] = [0x02, 0x00, 0x00, 0x00];
/// Size of the mixer block.
pub const BLOCK_SIZE: usize = 256;

/// Offsets in the mixer block (from the driver's serialiser `0x228a0`,
/// confirmed by reading a unit back after writing known values).
mod block {
    /// LINK bits, one per pair, 4 per byte: MONITOR OUT, INPUT, OUTPUT.
    pub const LINK: [usize; 3] = [0x18, 0x19, 0x1b];
    /// MUTE bits, one per channel, 4 per byte.
    pub const MUTE: [usize; 3] = [0x1d, 0x1e, 0x21];
    /// PAN, 4 nibbles per channel (MONITOR OUT has none).
    pub const PAN: [Option<usize>; 3] = [None, Some(0x24), Some(0x4c)];
    /// VOLUME, 6 nibbles per channel.
    pub const VOLUME: [usize; 3] = [0x74, 0x80, 0xbc];
}

fn get_bit(b: &[u8], base: usize, i: usize) -> bool {
    b[base + i / 4] & (1 << (i % 4)) != 0
}

impl MixerState {
    /// Decodes the channel part of a mixer block read from the unit.
    pub fn from_block(b: &[u8]) -> Option<Self> {
        if b.len() < BLOCK_SIZE {
            return None;
        }
        let mut s = Self::initial();
        for bus in Bus::ALL {
            let i = bus.index() as usize;
            for pair in 0..bus.channels() as usize / 2 {
                s.links[i][pair] = get_bit(b, block::LINK[i], pair);
            }
            for ch in 0..bus.channels() as usize {
                let c = &mut s.channels[i][ch];
                c.mute = get_bit(b, block::MUTE[i], ch);
                if let Some(base) = block::PAN[i] {
                    c.pan = from_nibbles(&b[base + 4 * ch..base + 4 * ch + 4]) as u16;
                }
                let v = block::VOLUME[i] + 6 * ch;
                c.volume = from_nibbles(&b[v..v + 6]);
            }
        }
        Some(s)
    }

    /// `true` when nothing can be heard: every OUTPUT and MONITOR OUT
    /// channel muted or at zero volume (the state a unit starts in with
    /// SOFT CTRL on).
    pub fn is_silent(&self) -> bool {
        let silent = |bus: Bus| {
            let i = bus.index() as usize;
            self.channels[i][..bus.channels() as usize]
                .iter()
                .all(|c| c.mute || c.volume == 0)
        };
        silent(Bus::MonitorOut) || silent(Bus::Output)
    }
}

/// Bus-level parameter at `00 (n+2) 00 00` (driver sub-command 21, `n` = 1
/// or 2 in the control panel).
pub fn set_bus_parameter(n: u8, value: u8) -> Message {
    sysex::dt1([0x00, n + 2, 0x00, 0x00], &[value])
}

/// Patch bay: source of OUTPUT `output` (0-based; driver sub-command 12,
/// address `00 00 n 03`).
pub fn set_patch_bay(output: u8, source: u8) -> Message {
    sysex::dt1([0x00, 0x00, output, 0x03], &[source])
}

/// Device memory operations at `00 00 00 0x` (driver sub-commands 9–11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    /// Load settings from the device memory (`00 00 00 00`).
    Load,
    /// Store settings into the device memory (`00 00 00 01`).
    Store,
    /// Restore initial settings (`00 00 00 02`).
    Reset,
}

pub fn memory(op: MemoryOp, value: u8) -> Message {
    let last = match op {
        MemoryOp::Load => 0x00,
        MemoryOp::Store => 0x01,
        MemoryOp::Reset => 0x02,
    };
    sysex::dt1([0x00, 0x00, 0x00, last], &[value])
}

/// Front-panel state reported by the unit (block `00 01 00 xx`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontPanel {
    /// `00 01 00 02`: clock source. `0` was seen with the internal clock and
    /// `9` after switching; raw value kept until the encoding is known.
    pub clock: Option<u8>,
    /// `00 01 00 04`: DIRECT MONITOR MONO.
    pub mono: Option<bool>,
    /// `00 01 00 05`: SOFT CTRL. When off, direct monitoring cannot be
    /// controlled from the computer.
    pub soft_ctrl: Option<bool>,
    /// Other bytes of the block, by offset, for diagnostics.
    pub other: [Option<u8>; 6],
}

impl FrontPanel {
    /// Applies a DT1 received from the unit. Returns `false` if the message
    /// is outside the front-panel block.
    pub fn apply(&mut self, address: [u8; 4], data: &[u8]) -> bool {
        if address[0] != 0x00 || address[1] != 0x01 || address[2] != 0x00 {
            return false;
        }
        for (i, &v) in data.iter().enumerate() {
            let offset = address[3] as usize + i;
            match offset {
                0x02 => self.clock = Some(v),
                0x04 => self.mono = Some(v != 0),
                0x05 => self.soft_ctrl = Some(v != 0),
                _ => {}
            }
            if let Some(slot) = self.other.get_mut(offset) {
                *slot = Some(v);
            }
        }
        true
    }

    /// Human-readable clock source.
    pub fn clock_label(&self) -> &'static str {
        match self.clock {
            None => "?",
            Some(0) => "INTERNAL",
            Some(_) => "DIGITAL",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysex::parse_dt1;

    #[test]
    fn nibble_split_matches_driver() {
        assert_eq!(nibbles::<4>(0x1234), [1, 2, 3, 4]);
        assert_eq!(nibbles::<6>(0xabcdef), [0xa, 0xb, 0xc, 0xd, 0xe, 0xf]);
        assert_eq!(from_nibbles(&[1, 2, 3, 4]), 0x1234);
    }

    #[test]
    fn channel_addresses() {
        let m = set_pan(Bus::Input, 3, 0x7fff);
        let d = parse_dt1(m.as_bytes()).unwrap();
        assert_eq!(d.address, [0x00, 0x04, 0x03, 0x03]);
        assert_eq!(d.data, &[0x7, 0xf, 0xf, 0xf]);
        assert_eq!(m.as_bytes().len(), 16);

        // The message that gave a clean level on hardware.
        let m = set_volume(Bus::Output, 0, 0x10_0000);
        let d = parse_dt1(m.as_bytes()).unwrap();
        assert_eq!(d.address, [0x00, 0x05, 0x00, 0x07]);
        assert_eq!(d.data, &[0x1, 0, 0, 0, 0, 0]);
        assert_eq!(m.as_bytes().len(), 18);

        let m = set_link(Bus::Output, 2, true);
        assert_eq!(parse_dt1(m.as_bytes()).unwrap().address, [0, 0x05, 4, 0x01]);

        let m = set_mute(Bus::MonitorOut, 1, false);
        let d = parse_dt1(m.as_bytes()).unwrap();
        assert_eq!(d.address, [0, 0x03, 1, 0x02]);
        assert_eq!(d.data, &[0]);
    }

    #[test]
    fn full_state_roundtrip() {
        let initial = MixerState::initial();
        let mut rebuilt = MixerState::initial();
        rebuilt.channels = [[Channel {
            mute: true,
            pan: 0,
            volume: 0,
        }; 10]; 3];
        rebuilt.links = [[false; 5]; 3];
        // MONITOR OUT has no pan on the unit, so it is not sent.
        let mon = Bus::MonitorOut.index() as usize;
        rebuilt.channels[mon][0].pan = initial.channels[mon][0].pan;
        rebuilt.channels[mon][1].pan = initial.channels[mon][1].pan;
        let mut count = 0;
        initial.for_each_message(|m| {
            let d = parse_dt1(m.as_bytes()).unwrap();
            assert!(rebuilt.apply(d.address, d.data));
            count += 1;
        });
        // 10+10+2 strips × 3 parameters (no pan on MONITOR OUT) + 5+5+1 links.
        assert_eq!(count, 22 * 3 - 2 + 11);
        // MONITOR OUT only has 2 channels; compare what was sent.
        for bus in Bus::ALL {
            let i = bus.index() as usize;
            for ch in 0..bus.channels() as usize {
                assert_eq!(rebuilt.channels[i][ch], initial.channels[i][ch]);
            }
        }
    }

    #[test]
    fn decodes_block_read_from_unit() {
        // Block read back from a real unit after sending the initial state.
        let hex = include_str!("../tests/data/mixer_block.txt");
        let b: std::vec::Vec<u8> = hex
            .split_whitespace()
            .map(|x| u8::from_str_radix(x, 16).unwrap())
            .collect();
        let s = MixerState::from_block(&b).unwrap();
        assert_eq!(s, MixerState::initial());
        assert!(!s.is_silent());
    }

    #[test]
    fn silent_detection() {
        let mut s = MixerState::initial();
        let mon = Bus::MonitorOut.index() as usize;
        s.channels[mon][0].mute = true;
        s.channels[mon][1].volume = 0;
        assert!(s.is_silent());
    }

    #[test]
    fn initial_state_is_safe() {
        let s = MixerState::initial();
        let input = Bus::Input.index() as usize;
        assert!(s.channels[input].iter().all(|c| c.mute));
        let out = Bus::Output.index() as usize;
        assert!(
            s.channels[out]
                .iter()
                .all(|c| !c.mute && c.volume == VOLUME_UNITY)
        );
    }

    #[test]
    fn front_panel_from_real_messages() {
        // Messages captured from a real unit while toggling the switches.
        let mut fp = FrontPanel::default();
        for raw in [
            [
                0xf0, 0x41, 0x10, 0x00, 0x69, 0x12, 0x00, 0x01, 0x00, 0x05, 0x01, 0x79, 0xf7,
            ],
            [
                0xf0, 0x41, 0x10, 0x00, 0x69, 0x12, 0x00, 0x01, 0x00, 0x04, 0x01, 0x7a, 0xf7,
            ],
            [
                0xf0, 0x41, 0x10, 0x00, 0x69, 0x12, 0x00, 0x01, 0x00, 0x02, 0x09, 0x74, 0xf7,
            ],
        ] {
            let d = parse_dt1(&raw).unwrap();
            assert!(fp.apply(d.address, d.data));
        }
        assert_eq!(fp.mono, Some(true));
        assert_eq!(fp.soft_ctrl, Some(true));
        assert_eq!(fp.clock, Some(9));
        assert_eq!(fp.clock_label(), "DIGITAL");
        assert!(!fp.apply([0, 4, 0, 3], &[1]));
    }
}
