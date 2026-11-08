//! Parsing of the streaming-interface descriptors.
//!
//! The UA-101 uses vendor-specific interface classes, but its streaming
//! alternate settings carry USB Audio Class 1.0 style class-specific
//! descriptors. The Windows driver locates, for interfaces 0 and 1 in
//! alternate setting 1:
//!
//! * the class-specific `FORMAT_TYPE` descriptor, from which it reads
//!   `bFormatType`, `bNrChannels`, `bBitResolution`, `bSamFreqType` and the
//!   24-bit `tSamFreq`;
//! * the isochronous endpoint descriptor, from which it reads the direction,
//!   the transfer type and `wMaxPacketSize`.
//!
//! This module performs the same walk over a raw configuration descriptor.

use core::fmt;

/// `bDescriptorType` of an interface descriptor.
pub const DT_INTERFACE: u8 = 0x04;
/// `bDescriptorType` of an endpoint descriptor.
pub const DT_ENDPOINT: u8 = 0x05;
/// `bDescriptorType` of a class-specific interface descriptor.
pub const DT_CS_INTERFACE: u8 = 0x24;
/// Audio-streaming `bDescriptorSubtype` of the general descriptor.
pub const AS_GENERAL: u8 = 0x01;
/// Audio-streaming `bDescriptorSubtype` of the format type descriptor.
pub const AS_FORMAT_TYPE: u8 = 0x02;

/// Errors returned while parsing descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// A descriptor's `bLength` is zero or runs past the end of the buffer.
    Malformed { offset: usize },
    /// The requested interface/alternate setting is absent.
    InterfaceNotFound { interface: u8, alt_setting: u8 },
    /// The alternate setting has no `FORMAT_TYPE` descriptor.
    FormatNotFound { interface: u8 },
    /// The alternate setting has no isochronous endpoint.
    EndpointNotFound { interface: u8 },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { offset } => write!(f, "malformed descriptor at offset {offset}"),
            Self::InterfaceNotFound {
                interface,
                alt_setting,
            } => write!(
                f,
                "interface {interface} alt setting {alt_setting} not found"
            ),
            Self::FormatNotFound { interface } => {
                write!(f, "interface {interface} has no FORMAT_TYPE descriptor")
            }
            Self::EndpointNotFound { interface } => {
                write!(f, "interface {interface} has no isochronous endpoint")
            }
        }
    }
}

impl core::error::Error for ParseError {}

/// Endpoint transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host to device.
    Out,
    /// Device to host.
    In,
}

/// Endpoint transfer type (`bmAttributes & 3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl TransferType {
    const fn from_attributes(attributes: u8) -> Self {
        match attributes & 0x03 {
            0 => Self::Control,
            1 => Self::Isochronous,
            2 => Self::Bulk,
            _ => Self::Interrupt,
        }
    }
}

/// An endpoint descriptor reduced to the fields the driver needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    /// `bEndpointAddress`, including the direction bit.
    pub address: u8,
    pub transfer_type: TransferType,
    /// Bytes per transaction (`wMaxPacketSize & 0x7ff`).
    pub max_packet_size: u16,
    /// Additional transactions per microframe (`(wMaxPacketSize >> 11) & 3`).
    pub additional_transactions: u8,
    /// `bInterval`.
    pub interval: u8,
}

impl Endpoint {
    /// Parses a 7-byte (or longer) endpoint descriptor.
    pub fn parse(desc: &[u8]) -> Option<Self> {
        if desc.len() < 7 || desc[1] != DT_ENDPOINT {
            return None;
        }
        let w_max = u16::from_le_bytes([desc[4], desc[5]]);
        Some(Self {
            address: desc[2],
            transfer_type: TransferType::from_attributes(desc[3]),
            max_packet_size: w_max & 0x07ff,
            additional_transactions: ((w_max >> 11) & 0x03) as u8,
            interval: desc[6],
        })
    }

    /// Direction from bit 7 of the address.
    pub const fn direction(&self) -> Direction {
        if self.address & 0x80 != 0 {
            Direction::In
        } else {
            Direction::Out
        }
    }

    /// Maximum bytes the endpoint moves per (micro)frame.
    pub const fn bytes_per_interval(&self) -> u32 {
        self.max_packet_size as u32 * (self.additional_transactions as u32 + 1)
    }
}

/// Audio format of one streaming interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    /// Interface number (0 = playback, 1 = capture).
    pub interface: u8,
    /// Alternate setting this format belongs to.
    pub alt_setting: u8,
    /// `bFormatType` (1 = Type I PCM).
    pub format_type: u8,
    /// `bNrChannels`.
    pub channels: u8,
    /// `bSubframeSize`: bytes per sample on the wire (3 or 4).
    pub subframe_size: u8,
    /// `bBitResolution`: significant bits per sample (24 on the UA-101).
    pub bit_resolution: u8,
    /// First `tSamFreq` entry, in Hz.
    pub sample_rate: u32,
    /// The isochronous data endpoint.
    pub endpoint: Endpoint,
}

impl StreamFormat {
    /// Bytes per audio frame (all channels of one sample instant).
    pub const fn frame_bytes(&self) -> usize {
        self.channels as usize * self.subframe_size as usize
    }

    /// Maximum audio frames that fit in one isochronous packet.
    pub const fn max_frames_per_packet(&self) -> u32 {
        match self
            .endpoint
            .bytes_per_interval()
            .checked_div(self.frame_bytes() as u32)
        {
            Some(n) => n,
            None => 0,
        }
    }
}

/// Iterates over the descriptors in a configuration descriptor blob.
#[derive(Clone)]
pub struct DescriptorIter<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl<'a> DescriptorIter<'a> {
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, offset: 0 }
    }
}

impl<'a> Iterator for DescriptorIter<'a> {
    /// `(offset, descriptor bytes)` or an error for a malformed descriptor.
    type Item = Result<(usize, &'a [u8]), ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.buf.get(self.offset..)?;
        if rest.is_empty() {
            return None;
        }
        let len = rest[0] as usize;
        if len < 2 || len > rest.len() {
            let offset = self.offset;
            self.offset = self.buf.len();
            return Some(Err(ParseError::Malformed { offset }));
        }
        let start = self.offset;
        self.offset += len;
        Some(Ok((start, &rest[..len])))
    }
}

/// Returns the descriptors that belong to `interface`/`alt_setting`, i.e. the
/// interface descriptor itself followed by everything up to the next
/// interface descriptor.
pub fn interface_block(config: &[u8], interface: u8, alt_setting: u8) -> Result<&[u8], ParseError> {
    let mut start = None;
    for item in DescriptorIter::new(config) {
        let (offset, desc) = item?;
        if desc[1] != DT_INTERFACE || desc.len() < 9 {
            continue;
        }
        if let Some(s) = start {
            return Ok(&config[s..offset]);
        }
        if desc[2] == interface && desc[3] == alt_setting {
            start = Some(offset);
        }
    }
    match start {
        Some(s) => Ok(&config[s..]),
        None => Err(ParseError::InterfaceNotFound {
            interface,
            alt_setting,
        }),
    }
}

/// Parses the stream format of `interface` in `alt_setting` from a raw
/// configuration descriptor (as returned by `GET_DESCRIPTOR(CONFIGURATION)`
/// with the full `wTotalLength`).
pub fn parse_stream_format(
    config: &[u8],
    interface: u8,
    alt_setting: u8,
) -> Result<StreamFormat, ParseError> {
    let block = interface_block(config, interface, alt_setting)?;
    let mut format = None;
    let mut endpoint = None;
    for item in DescriptorIter::new(block) {
        let (_, desc) = item?;
        match desc[1] {
            DT_CS_INTERFACE if desc.len() >= 11 && desc[2] == AS_FORMAT_TYPE => {
                format = Some((
                    desc[3],
                    desc[4],
                    desc[5],
                    desc[6],
                    u32::from_le_bytes([desc[8], desc[9], desc[10], 0]),
                ));
            }
            DT_ENDPOINT => {
                if let Some(ep) = Endpoint::parse(desc) {
                    if ep.transfer_type == TransferType::Isochronous && endpoint.is_none() {
                        endpoint = Some(ep);
                    }
                }
            }
            _ => {}
        }
    }
    let (format_type, channels, subframe_size, bit_resolution, sample_rate) =
        format.ok_or(ParseError::FormatNotFound { interface })?;
    let endpoint = endpoint.ok_or(ParseError::EndpointNotFound { interface })?;
    Ok(StreamFormat {
        interface,
        alt_setting,
        format_type,
        channels,
        subframe_size,
        bit_resolution,
        sample_rate,
        endpoint,
    })
}

/// Finds the first bulk endpoint of `interface` (alternate setting 0) in the
/// given direction. The UA-101 exposes MIDI on a bulk IN/OUT pair.
pub fn find_bulk_endpoint(
    config: &[u8],
    interface: u8,
    direction: Direction,
) -> Result<Endpoint, ParseError> {
    let block = interface_block(config, interface, 0)?;
    for item in DescriptorIter::new(block) {
        let (_, desc) = item?;
        if let Some(ep) = Endpoint::parse(desc) {
            if ep.transfer_type == TransferType::Bulk && ep.direction() == direction {
                return Ok(ep);
            }
        }
    }
    Err(ParseError::EndpointNotFound { interface })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::vec::Vec;

    /// Builds a synthetic configuration descriptor shaped like the UA-101's:
    /// two streaming interfaces (idle alt 0 + streaming alt 1) and a MIDI
    /// interface with a bulk pair. Channel counts and rates are parameters
    /// because the real values depend on the hardware rate switch.
    pub fn synthetic_config(channels: u8, subframe: u8, rate: u32, max_packet: u16) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&[9, 2, 0, 0, 3, 1, 0, 0x80, 250]);
        for (intf, ep) in [(0u8, 0x01u8), (1, 0x81)] {
            c.extend_from_slice(&[9, DT_INTERFACE, intf, 0, 0, 0xff, 0x02, 0, 0]);
            c.extend_from_slice(&[9, DT_INTERFACE, intf, 1, 1, 0xff, 0x02, 0, 0]);
            c.extend_from_slice(&[7, DT_CS_INTERFACE, AS_GENERAL, 0, 1, 1, 0]);
            let r = rate.to_le_bytes();
            c.extend_from_slice(&[
                11,
                DT_CS_INTERFACE,
                AS_FORMAT_TYPE,
                1,
                channels,
                subframe,
                24,
                1,
                r[0],
                r[1],
                r[2],
            ]);
            let mp = max_packet.to_le_bytes();
            c.extend_from_slice(&[7, DT_ENDPOINT, ep, 0x05, mp[0], mp[1], 1]);
        }
        c.extend_from_slice(&[9, DT_INTERFACE, 2, 0, 2, 0xff, 0x03, 0, 0]);
        c.extend_from_slice(&[7, DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
        c.extend_from_slice(&[7, DT_ENDPOINT, 0x82, 0x02, 0x00, 0x02, 0]);
        let total = (c.len() as u16).to_le_bytes();
        c[2] = total[0];
        c[3] = total[1];
        c
    }

    #[test]
    fn parses_both_streams() {
        let c = synthetic_config(10, 4, 48_000, 400);
        let play = parse_stream_format(&c, 0, 1).unwrap();
        assert_eq!(play.channels, 10);
        assert_eq!(play.subframe_size, 4);
        assert_eq!(play.bit_resolution, 24);
        assert_eq!(play.sample_rate, 48_000);
        assert_eq!(play.endpoint.address, 0x01);
        assert_eq!(play.endpoint.direction(), Direction::Out);
        assert_eq!(play.frame_bytes(), 40);
        assert_eq!(play.max_frames_per_packet(), 10);

        let cap = parse_stream_format(&c, 1, 1).unwrap();
        assert_eq!(cap.endpoint.address, 0x81);
        assert_eq!(cap.endpoint.direction(), Direction::In);
    }

    #[test]
    fn idle_alt_setting_has_no_format() {
        let c = synthetic_config(10, 4, 48_000, 400);
        assert_eq!(
            parse_stream_format(&c, 0, 0),
            Err(ParseError::FormatNotFound { interface: 0 })
        );
    }

    #[test]
    fn missing_interface() {
        let c = synthetic_config(10, 4, 48_000, 400);
        assert_eq!(
            parse_stream_format(&c, 5, 1),
            Err(ParseError::InterfaceNotFound {
                interface: 5,
                alt_setting: 1
            })
        );
    }

    #[test]
    fn reads_24_bit_rate() {
        let c = synthetic_config(8, 3, 96_000, 600);
        assert_eq!(parse_stream_format(&c, 1, 1).unwrap().sample_rate, 96_000);
    }

    #[test]
    fn finds_midi_bulk_pair() {
        let c = synthetic_config(10, 4, 48_000, 400);
        assert_eq!(
            find_bulk_endpoint(&c, 2, Direction::Out).unwrap().address,
            0x02
        );
        assert_eq!(
            find_bulk_endpoint(&c, 2, Direction::In).unwrap().address,
            0x82
        );
    }

    #[test]
    fn rejects_truncated_descriptor() {
        let mut c = synthetic_config(10, 4, 48_000, 400);
        c.push(40);
        c.push(DT_INTERFACE);
        assert!(matches!(
            parse_stream_format(&c, 9, 1),
            Err(ParseError::Malformed { .. })
        ));
    }

    #[test]
    fn real_device_at_44k1() {
        let c = &crate::fixtures::UA101_CONFIG_44K1;
        let play = parse_stream_format(c, crate::INTF_PLAYBACK, 1).unwrap();
        assert_eq!(
            (
                play.channels,
                play.subframe_size,
                play.bit_resolution,
                play.sample_rate
            ),
            (10, 4, 24, 44_100)
        );
        assert_eq!(play.endpoint.address, 0x02);
        assert_eq!(play.endpoint.max_packet_size, 280);
        assert_eq!(play.max_frames_per_packet(), 7);

        let cap = parse_stream_format(c, crate::INTF_CAPTURE, 1).unwrap();
        assert_eq!(
            (
                cap.channels,
                cap.subframe_size,
                cap.bit_resolution,
                cap.sample_rate
            ),
            (12, 4, 24, 44_100)
        );
        assert_eq!(cap.endpoint.address, 0x81);
        assert_eq!(cap.endpoint.max_packet_size, 336);
        assert_eq!(cap.endpoint.interval, 1);

        assert_eq!(
            find_bulk_endpoint(c, crate::INTF_MIDI, Direction::Out)
                .unwrap()
                .address,
            0x04
        );
        assert_eq!(
            find_bulk_endpoint(c, crate::INTF_MIDI, Direction::In)
                .unwrap()
                .address,
            0x83
        );
    }

    #[test]
    fn high_bandwidth_endpoint() {
        let ep = Endpoint::parse(&[7, DT_ENDPOINT, 0x81, 0x05, 0x00, 0x14, 1]).unwrap();
        assert_eq!(ep.max_packet_size, 0x400);
        assert_eq!(ep.additional_transactions, 2);
        assert_eq!(ep.bytes_per_interval(), 3 * 1024);
    }
}
