//! Platform-independent protocol of the Roland/EDIROL UA-101 USB audio interface.
//!
//! Everything in this crate is pure computation: descriptor parsing, sample
//! packing, isochronous packet sizing, the vendor control request used for
//! direct monitoring and the USB-MIDI 1.0 event codec. It has no I/O and no
//! dependencies, and is `no_std`, so it can be unit-tested on any host and
//! reused by any USB backend.
//!
//! The facts encoded here come from static analysis of Roland's Windows
//! driver; see `docs/protocol.md` in the repository for the evidence behind
//! each constant and which parts are still inferred.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod descriptors;
#[cfg(test)]
mod fixtures;
pub mod midi;
pub mod monitor;
pub mod packets;
pub mod pcm;
pub mod sysex;

/// Roland Corporation USB vendor ID.
pub const VENDOR_ID: u16 = 0x0582;
/// UA-101 USB product ID (`USB\VID_0582&PID_007D` in the Windows INF).
pub const PRODUCT_ID: u16 = 0x007d;

/// Interface carrying the isochronous playback (host → device) stream.
pub const INTF_PLAYBACK: u8 = 0;
/// Interface carrying the isochronous capture (device → host) stream.
pub const INTF_CAPTURE: u8 = 1;
/// Interface carrying the bulk USB-MIDI endpoints.
pub const INTF_MIDI: u8 = 2;
/// Alternate setting of the streaming interfaces that enables their endpoints.
/// Alternate setting 0 is the zero-bandwidth idle setting.
pub const STREAMING_ALT_SETTING: u8 = 1;

/// Sample rate the Windows driver assumes when the descriptor cannot be read.
/// The real rate is selected with the switch on the hardware and reported in
/// the format descriptor.
pub const DEFAULT_SAMPLE_RATE: u32 = 96_000;

/// Returns `true` if the vendor/product pair identifies a UA-101.
pub const fn is_ua101(vendor_id: u16, product_id: u16) -> bool {
    vendor_id == VENDOR_ID && product_id == PRODUCT_ID
}
