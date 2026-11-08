//! Roland exclusive messages used by the UA-101's internal mixer.
//!
//! The mixer, patch bay and device memory are not driven by USB control
//! requests. The Windows driver sends **Roland SysEx** on **USB-MIDI cable 1**
//! (cable 0 is the external MIDI port) through the same bulk endpoint:
//!
//! ```text
//! DT1 (write): F0 41 10 00 69 12 a0 a1 a2 a3 d0 .. dn sum F7
//! RQ1 (read):  F0 41 10 00 69 11 a0 a1 a2 a3 s0 s1 s2 s3 sum F7
//! ```
//!
//! `41` is Roland, `10` the device ID, `00 69` the UA-101 model ID. The
//! address and size are 4 bytes of 7 bits each. `sum` is the usual Roland
//! checksum over address and data: `(128 - (Σ bytes mod 128)) mod 128`.
//! The device answers an RQ1 with DT1 messages carrying the requested
//! memory.

/// USB-MIDI cable used for the mixer/control messages.
pub const CONTROL_CABLE: u8 = 1;
/// Roland manufacturer ID.
pub const ROLAND_ID: u8 = 0x41;
/// Device ID used by the driver.
pub const DEVICE_ID: u8 = 0x10;
/// UA-101 model ID.
pub const MODEL_ID: [u8; 2] = [0x00, 0x69];
/// Command byte of a data set (write).
pub const CMD_DT1: u8 = 0x12;
/// Command byte of a data request (read).
pub const CMD_RQ1: u8 = 0x11;

const HEADER: [u8; 5] = [0xf0, ROLAND_ID, DEVICE_ID, MODEL_ID[0], MODEL_ID[1]];

/// Largest DT1 payload [`dt1`] accepts.
pub const MAX_DATA: usize = 32;
/// Size of the largest message this module builds.
pub const MAX_MESSAGE: usize = HEADER.len() + 1 + 4 + MAX_DATA + 2;

/// Roland checksum of the address and data bytes.
pub fn checksum(bytes: &[u8]) -> u8 {
    let sum: u32 = bytes.iter().map(|&b| b as u32).sum();
    ((128 - (sum % 128)) % 128) as u8
}

/// A message built into a fixed buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Message {
    buf: [u8; MAX_MESSAGE],
    len: usize,
}

impl Message {
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Debug for Message {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, b) in self.as_bytes().iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

fn build(cmd: u8, address: [u8; 4], body: &[u8]) -> Message {
    let mut buf = [0u8; MAX_MESSAGE];
    buf[..5].copy_from_slice(&HEADER);
    buf[5] = cmd;
    buf[6..10].copy_from_slice(&address.map(|b| b & 0x7f));
    let mut n = 10;
    for &b in body {
        buf[n] = b & 0x7f;
        n += 1;
    }
    buf[n] = checksum(&buf[6..n]);
    buf[n + 1] = 0xf7;
    Message { buf, len: n + 2 }
}

/// Builds a DT1 (write) message. Data bytes are masked to 7 bits.
///
/// # Panics
/// If `data` is longer than [`MAX_DATA`].
pub fn dt1(address: [u8; 4], data: &[u8]) -> Message {
    assert!(data.len() <= MAX_DATA, "DT1 payload too long");
    build(CMD_DT1, address, data)
}

/// Builds an RQ1 (read) request for `size` bytes at `address`.
pub fn rq1(address: [u8; 4], size: u32) -> Message {
    build(CMD_RQ1, address, &encode_7bit_u32(size))
}

/// Splits a value into four 7-bit bytes, most significant first.
pub const fn encode_7bit_u32(v: u32) -> [u8; 4] {
    [
        ((v >> 21) & 0x7f) as u8,
        ((v >> 14) & 0x7f) as u8,
        ((v >> 7) & 0x7f) as u8,
        (v & 0x7f) as u8,
    ]
}

/// Adds `offset` to a 7-bit-per-byte address.
pub const fn address_add(address: [u8; 4], offset: u32) -> [u8; 4] {
    let base = ((address[0] as u32) << 21)
        | ((address[1] as u32) << 14)
        | ((address[2] as u32) << 7)
        | address[3] as u32;
    encode_7bit_u32(base + offset)
}

/// A DT1 message received from the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataSet<'a> {
    pub address: [u8; 4],
    pub data: &'a [u8],
}

/// Errors from [`parse_dt1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysexError {
    /// Not a UA-101 DT1 message.
    NotDt1,
    /// Too short to hold an address and checksum.
    Truncated,
    /// Checksum mismatch.
    Checksum { expected: u8, found: u8 },
}

/// Parses a complete `F0 … F7` message as a UA-101 DT1.
pub fn parse_dt1(msg: &[u8]) -> Result<DataSet<'_>, SysexError> {
    if msg.len() < 13 {
        return if msg.starts_with(&HEADER) {
            Err(SysexError::Truncated)
        } else {
            Err(SysexError::NotDt1)
        };
    }
    if !msg.starts_with(&HEADER) || msg[5] != CMD_DT1 || msg[msg.len() - 1] != 0xf7 {
        return Err(SysexError::NotDt1);
    }
    let body = &msg[6..msg.len() - 2];
    let found = msg[msg.len() - 2];
    let expected = checksum(body);
    if expected != found {
        return Err(SysexError::Checksum { expected, found });
    }
    Ok(DataSet {
        address: [body[0], body[1], body[2], body[3]],
        data: &body[4..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_roland_examples() {
        // Classic Roland GS reset: F0 41 10 42 12 40 00 7F 00 41 F7
        assert_eq!(checksum(&[0x40, 0x00, 0x7f, 0x00]), 0x41);
        assert_eq!(checksum(&[]), 0);
    }

    #[test]
    fn dt1_layout_matches_driver() {
        // 13-byte message built by the driver for a one-byte parameter.
        let m = dt1([0x00, 0x01, 0x00, 0x02], &[0x05]);
        assert_eq!(
            m.as_bytes(),
            &[
                0xf0, 0x41, 0x10, 0x00, 0x69, 0x12, 0x00, 0x01, 0x00, 0x02, 0x05, 0x78, 0xf7
            ]
        );
    }

    #[test]
    fn rq1_layout() {
        let m = rq1([0x00, 0x02, 0x03, 0x00], 4);
        assert_eq!(m.as_bytes().len(), 16);
        assert_eq!(
            &m.as_bytes()[5..14],
            &[0x11, 0x00, 0x02, 0x03, 0x00, 0, 0, 0, 4]
        );
        assert_eq!(*m.as_bytes().last().unwrap(), 0xf7);
    }

    #[test]
    fn parse_roundtrip() {
        let m = dt1([0x00, 0x03, 0x02, 0x01], &[1, 2, 3]);
        let d = parse_dt1(m.as_bytes()).unwrap();
        assert_eq!(d.address, [0x00, 0x03, 0x02, 0x01]);
        assert_eq!(d.data, &[1, 2, 3]);
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        let mut bytes = [0u8; 13];
        bytes.copy_from_slice(dt1([0, 1, 0, 0], &[9]).as_bytes());
        bytes[11] ^= 1;
        assert!(matches!(
            parse_dt1(&bytes),
            Err(SysexError::Checksum { .. })
        ));
        assert_eq!(parse_dt1(&[0xf0, 0x7e, 0xf7]), Err(SysexError::NotDt1));
    }

    #[test]
    fn address_arithmetic_is_7_bit() {
        assert_eq!(address_add([0, 0, 0, 0x7f], 1), [0, 0, 1, 0]);
        assert_eq!(address_add([0, 2, 0, 0], 0x81), [0, 2, 1, 1]);
    }
}
