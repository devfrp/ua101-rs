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
    /// Stereo link of the pair starting at this (even) channel.
    pub const LINK: u8 = 0x01;
    /// Monitor ON/OFF.
    pub const ON: u8 = 0x02;
    /// Volume, 16-bit, sent as 4 nibbles.
    pub const VOLUME: u8 = 0x03;
    /// 24-bit parameter sent as 6 nibbles (pan or pan/solo pair).
    pub const WIDE: u8 = 0x07;
}

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

/// Monitor ON/OFF of one channel (driver sub-command 23).
pub fn set_on(bus: Bus, channel: u8, on: bool) -> Message {
    sysex::dt1(channel_address(bus, channel, slot::ON), &[on as u8])
}

/// Stereo link of channel pair `pair` (channels `2·pair` and `2·pair+1`;
/// driver sub-command 22).
pub fn set_link(bus: Bus, pair: u8, linked: bool) -> Message {
    sysex::dt1(channel_address(bus, pair * 2, slot::LINK), &[linked as u8])
}

/// Channel volume (driver sub-command 24). The scale is not known yet.
pub fn set_volume(bus: Bus, channel: u8, value: u16) -> Message {
    sysex::dt1(
        channel_address(bus, channel, slot::VOLUME),
        &nibbles::<4>(value as u32),
    )
}

/// 24-bit channel parameter at slot `07` (driver sub-command 25).
pub fn set_wide(bus: Bus, channel: u8, value: u32) -> Message {
    sysex::dt1(
        channel_address(bus, channel, slot::WIDE),
        &nibbles::<6>(value & 0x00ff_ffff),
    )
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
    /// `00 01 00 04`: SOFT CTRL. When off, direct monitoring cannot be
    /// controlled from the computer.
    pub soft_ctrl: Option<bool>,
    /// `00 01 00 05`: DIRECT MONITOR MONO.
    pub mono: Option<bool>,
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
                0x04 => self.soft_ctrl = Some(v != 0),
                0x05 => self.mono = Some(v != 0),
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
        let m = set_volume(Bus::Input, 3, 0x7fff);
        let d = parse_dt1(m.as_bytes()).unwrap();
        assert_eq!(d.address, [0x00, 0x04, 0x03, 0x03]);
        assert_eq!(d.data, &[0x7, 0xf, 0xf, 0xf]);
        assert_eq!(m.as_bytes().len(), 16);

        let m = set_link(Bus::Output, 2, true);
        assert_eq!(parse_dt1(m.as_bytes()).unwrap().address, [0, 0x05, 4, 0x01]);

        let m = set_on(Bus::MonitorOut, 1, true);
        assert_eq!(parse_dt1(m.as_bytes()).unwrap().address, [0, 0x03, 1, 0x02]);

        assert_eq!(set_wide(Bus::Input, 0, 0x40).as_bytes().len(), 18);
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
