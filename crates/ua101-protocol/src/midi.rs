//! USB-MIDI 1.0 event packets.
//!
//! The Windows driver moves MIDI over a bulk endpoint pair in units of
//! 4 bytes (its MIDI-out path sizes each transfer as `events * 4`). Those are
//! standard USB-MIDI 1.0 event packets. The first byte holds the cable number
//! (high nibble) and the Code Index Number (low nibble). The next three
//! bytes hold the MIDI data, zero-padded.

/// One 4-byte USB-MIDI event packet.
pub type EventPacket = [u8; 4];

/// Number of MIDI bytes carried by a packet with the given Code Index Number.
pub const fn cin_payload_len(cin: u8) -> usize {
    match cin & 0x0f {
        0x0 | 0x1 => 0, // reserved / cable events
        0x2 | 0x6 | 0xc | 0xd => 2,
        0x5 | 0xf => 1,
        _ => 3, // 0x3, 0x4, 0x7, 0x8..=0xb, 0xe
    }
}

/// Splits a packet into its cable number and MIDI bytes.
/// Returns `None` for empty or reserved packets.
pub fn decode_packet(packet: &EventPacket) -> Option<(u8, &[u8])> {
    let cable = packet[0] >> 4;
    let len = cin_payload_len(packet[0]);
    if len == 0 {
        return None;
    }
    Some((cable, &packet[1..1 + len]))
}

/// Number of data bytes that follow a channel or system-common status byte.
const fn data_bytes_for_status(status: u8) -> usize {
    match status & 0xf0 {
        0x80 | 0x90 | 0xa0 | 0xb0 | 0xe0 => 2,
        0xc0 | 0xd0 => 1,
        _ => match status {
            0xf1 | 0xf3 => 1,
            0xf2 => 2,
            _ => 0,
        },
    }
}

/// Converts a raw MIDI byte stream into USB-MIDI event packets, handling
/// running status, SysEx and interleaved real-time bytes.
#[derive(Debug, Clone)]
pub struct Encoder {
    cable: u8,
    running_status: u8,
    data: [u8; 2],
    data_len: usize,
    sysex: [u8; 3],
    sysex_len: usize,
    in_sysex: bool,
}

impl Encoder {
    /// Creates an encoder for virtual cable `cable` (0..=15).
    pub const fn new(cable: u8) -> Self {
        Self {
            cable: cable & 0x0f,
            running_status: 0,
            data: [0; 2],
            data_len: 0,
            sysex: [0; 3],
            sysex_len: 0,
            in_sysex: false,
        }
    }

    const fn header(&self, cin: u8) -> u8 {
        (self.cable << 4) | (cin & 0x0f)
    }

    /// Feeds one MIDI byte. Returns a completed packet, if any.
    pub fn push(&mut self, byte: u8) -> Option<EventPacket> {
        // Real-time messages may appear anywhere and never disturb state.
        if byte >= 0xf8 {
            return Some([self.header(0xf), byte, 0, 0]);
        }

        if self.in_sysex {
            if byte == 0xf7 {
                self.sysex[self.sysex_len] = byte;
                self.sysex_len += 1;
                let cin = 0x4 + self.sysex_len as u8; // 1 → 5, 2 → 6, 3 → 7
                let mut p = [self.header(cin), 0, 0, 0];
                p[1..1 + self.sysex_len].copy_from_slice(&self.sysex[..self.sysex_len]);
                self.sysex_len = 0;
                self.in_sysex = false;
                return Some(p);
            }
            if byte & 0x80 == 0 {
                self.sysex[self.sysex_len] = byte;
                self.sysex_len += 1;
                if self.sysex_len == 3 {
                    self.sysex_len = 0;
                    return Some([
                        self.header(0x4),
                        self.sysex[0],
                        self.sysex[1],
                        self.sysex[2],
                    ]);
                }
                return None;
            }
            // Any other status byte aborts the SysEx without emitting it.
            self.in_sysex = false;
            self.sysex_len = 0;
        }

        if byte == 0xf0 {
            self.in_sysex = true;
            self.sysex[0] = byte;
            self.sysex_len = 1;
            self.running_status = 0;
            return None;
        }

        if byte & 0x80 != 0 {
            self.data_len = 0;
            if byte >= 0xf0 {
                // System common cancels running status.
                self.running_status = 0;
                if data_bytes_for_status(byte) == 0 {
                    // Tune request (0xF6) or stray 0xF7: single-byte message.
                    return Some([self.header(0x5), byte, 0, 0]);
                }
            }
            self.running_status = byte;
            return None;
        }

        // Data byte.
        let status = self.running_status;
        if status == 0 {
            return None; // no status to attach to: drop
        }
        let needed = data_bytes_for_status(status);
        self.data[self.data_len] = byte;
        self.data_len += 1;
        if self.data_len < needed {
            return None;
        }
        self.data_len = 0;
        let cin = if status >= 0xf0 {
            if needed == 1 { 0x2 } else { 0x3 }
        } else {
            status >> 4
        };
        let p = [
            self.header(cin),
            status,
            self.data[0],
            if needed == 2 { self.data[1] } else { 0 },
        ];
        if status >= 0xf0 {
            self.running_status = 0;
        }
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn encode_all(bytes: &[u8]) -> Vec<EventPacket> {
        let mut e = Encoder::new(0);
        bytes.iter().filter_map(|&b| e.push(b)).collect()
    }

    #[test]
    fn note_on_and_running_status() {
        let p = encode_all(&[0x90, 60, 100, 62, 90]);
        assert_eq!(p, [[0x09, 0x90, 60, 100], [0x09, 0x90, 62, 90]]);
    }

    #[test]
    fn program_change_two_bytes() {
        assert_eq!(encode_all(&[0xc3, 5]), [[0x0c, 0xc3, 5, 0]]);
    }

    #[test]
    fn sysex_split_into_packets() {
        let p = encode_all(&[0xf0, 0x41, 0x10, 0x00, 0x12, 0xf7]);
        assert_eq!(p, [[0x04, 0xf0, 0x41, 0x10], [0x07, 0x00, 0x12, 0xf7]]);
        let p = encode_all(&[0xf0, 0x7e, 0xf7]);
        assert_eq!(p, [[0x07, 0xf0, 0x7e, 0xf7]]);
        let p = encode_all(&[0xf0, 1, 2, 0xf7]);
        assert_eq!(p, [[0x04, 0xf0, 1, 2], [0x05, 0xf7, 0, 0]]);
    }

    #[test]
    fn realtime_inside_sysex() {
        let p = encode_all(&[0xf0, 1, 0xf8, 2, 0xf7]);
        assert_eq!(
            p,
            [[0x0f, 0xf8, 0, 0], [0x04, 0xf0, 1, 2], [0x05, 0xf7, 0, 0]]
        );
    }

    #[test]
    fn system_common() {
        assert_eq!(encode_all(&[0xf2, 0x10, 0x20]), [[0x03, 0xf2, 0x10, 0x20]]);
        assert_eq!(encode_all(&[0xf1, 0x33]), [[0x02, 0xf1, 0x33, 0]]);
        assert_eq!(encode_all(&[0xf6]), [[0x05, 0xf6, 0, 0]]);
    }

    #[test]
    fn cable_number_in_header() {
        let mut e = Encoder::new(3);
        assert_eq!(e.push(0xfe), Some([0x3f, 0xfe, 0, 0]));
    }

    #[test]
    fn decode_roundtrip() {
        let bytes = [0x90, 60, 100, 0xf0, 1, 2, 3, 0xf7, 0xc0, 7];
        let mut out = Vec::new();
        for p in encode_all(&bytes) {
            let (cable, data) = decode_packet(&p).unwrap();
            assert_eq!(cable, 0);
            out.extend_from_slice(data);
        }
        assert_eq!(out, bytes);
    }

    #[test]
    fn decode_ignores_empty_packets() {
        assert_eq!(decode_packet(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn orphan_data_dropped() {
        assert!(encode_all(&[60, 100]).is_empty());
    }
}
