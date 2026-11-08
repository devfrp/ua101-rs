//! Direct (hardware) input monitoring.
//!
//! Roland's ASIO driver exposes direct monitoring through driver IOCTL
//! `0x222144`. The kernel driver turns it into one vendor control request
//! with no data stage:
//!
//! | field           | value                                   |
//! |-----------------|-----------------------------------------|
//! | `bmRequestType` | `0x40` (vendor, device, host-to-device) |
//! | `bRequest`      | `0x02`                                  |
//! | `wValue`        | see [`direct_monitor_value`]            |
//! | `wIndex`        | `0`                                     |
//! | `wLength`       | `0`                                     |
//!
//! The ASIO DLL builds `wValue` as `0x8000` when monitoring is being
//! switched on, OR'ed with `(input index + 1) & 0x7fff`. To switch every
//! input at once it sends the request with no channel bits.
//!
//! **Status: inferred.** The encoding is read from static code. Its effect on
//! the hardware has not been observed yet.

/// `bmRequestType` for vendor, device-recipient, host-to-device requests.
pub const REQUEST_TYPE_VENDOR_OUT: u8 = 0x40;
/// `bRequest` used for direct monitoring.
pub const REQUEST_DIRECT_MONITOR: u8 = 0x02;

const ENABLE_BIT: u16 = 0x8000;
const CHANNEL_MASK: u16 = 0x7fff;

/// Which inputs a direct-monitor request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorTarget {
    /// Every input.
    All,
    /// One input, zero-based (input 1 on the front panel is `Input(0)`).
    Input(u16),
}

/// Builds the `wValue` of a direct-monitor request.
pub const fn direct_monitor_value(target: MonitorTarget, enabled: bool) -> u16 {
    let channel = match target {
        MonitorTarget::All => 0,
        MonitorTarget::Input(i) => (i.wrapping_add(1)) & CHANNEL_MASK,
    };
    if enabled {
        ENABLE_BIT | channel
    } else {
        channel
    }
}

/// Decodes a `wValue` back into its target and state.
pub const fn parse_direct_monitor_value(value: u16) -> (MonitorTarget, bool) {
    let enabled = value & ENABLE_BIT != 0;
    let channel = value & CHANNEL_MASK;
    let target = if channel == 0 {
        MonitorTarget::All
    } else {
        MonitorTarget::Input(channel - 1)
    };
    (target, enabled)
}

/// A complete vendor control request, independent of any USB library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorRequest {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

/// The control request that switches direct monitoring.
pub const fn direct_monitor_request(target: MonitorTarget, enabled: bool) -> VendorRequest {
    VendorRequest {
        request_type: REQUEST_TYPE_VENDOR_OUT,
        request: REQUEST_DIRECT_MONITOR,
        value: direct_monitor_value(target, enabled),
        index: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_asio_dll() {
        assert_eq!(direct_monitor_value(MonitorTarget::All, true), 0x8000);
        assert_eq!(direct_monitor_value(MonitorTarget::All, false), 0x0000);
        assert_eq!(direct_monitor_value(MonitorTarget::Input(0), true), 0x8001);
        assert_eq!(direct_monitor_value(MonitorTarget::Input(9), false), 0x000a);
    }

    #[test]
    fn roundtrip() {
        for target in [
            MonitorTarget::All,
            MonitorTarget::Input(0),
            MonitorTarget::Input(7),
        ] {
            for enabled in [false, true] {
                let v = direct_monitor_value(target, enabled);
                assert_eq!(parse_direct_monitor_value(v), (target, enabled));
            }
        }
    }

    #[test]
    fn request_fields() {
        let r = direct_monitor_request(MonitorTarget::Input(2), true);
        assert_eq!(r.request_type, 0x40);
        assert_eq!(r.request, 0x02);
        assert_eq!(r.value, 0x8003);
        assert_eq!(r.index, 0);
    }
}
