use std::time::Duration;

use nusb::{
    DeviceInfo, MaybeFuture, Speed,
    transfer::{ControlOut, ControlType, Recipient},
};
use ua101_protocol::{
    DEFAULT_SAMPLE_RATE, INTF_CAPTURE, INTF_PLAYBACK, STREAMING_ALT_SETTING,
    descriptors::{ParseError, StreamFormat, parse_stream_format},
    is_ua101,
    monitor::{MonitorTarget, VendorRequest, direct_monitor_request},
};

use crate::{DuplexStream, Error, MidiPort, Result, StreamConfig};

const CONTROL_TIMEOUT: Duration = Duration::from_millis(1000);

/// Lists every connected UA-101.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    Ok(nusb::list_devices()
        .wait()?
        .filter(|d| is_ua101(d.vendor_id(), d.product_id()))
        .collect())
}

/// An opened UA-101.
///
/// Opening the device claims no interface, so it can coexist with a kernel
/// driver until streaming or MIDI is started.
pub struct Ua101 {
    info: DeviceInfo,
    device: nusb::Device,
    config: Vec<u8>,
    playback: Result<StreamFormat, ParseError>,
    capture: Result<StreamFormat, ParseError>,
}

impl Ua101 {
    /// Opens the first UA-101 found.
    pub fn open_first() -> Result<Self> {
        let info = list_devices()?.into_iter().next().ok_or(Error::NotFound)?;
        Self::open(info)
    }

    /// Opens a specific device returned by [`list_devices`].
    pub fn open(info: DeviceInfo) -> Result<Self> {
        let device = info.open().wait()?;
        let config = device.active_configuration()?.as_bytes().to_vec();
        let playback = parse_stream_format(&config, INTF_PLAYBACK, STREAMING_ALT_SETTING);
        let capture = parse_stream_format(&config, INTF_CAPTURE, STREAMING_ALT_SETTING);
        Ok(Self {
            info,
            device,
            config,
            playback,
            capture,
        })
    }

    /// Enumeration information.
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// The underlying `nusb` device.
    pub fn usb(&self) -> &nusb::Device {
        &self.device
    }

    /// Raw bytes of the active configuration descriptor.
    pub fn config_descriptor(&self) -> &[u8] {
        &self.config
    }

    /// Playback stream format parsed from interface 0, alternate setting 1.
    pub fn playback_format(&self) -> Result<StreamFormat, ParseError> {
        self.playback
    }

    /// Capture stream format parsed from interface 1, alternate setting 1.
    pub fn capture_format(&self) -> Result<StreamFormat, ParseError> {
        self.capture
    }

    /// Negotiated bus speed, when the OS reports it.
    pub fn speed(&self) -> Option<Speed> {
        self.info.speed().or_else(|| self.device.speed())
    }

    /// `true` unless the OS reports a speed below high speed.
    pub fn is_high_speed(&self) -> bool {
        !matches!(self.speed(), Some(Speed::Low | Speed::Full))
    }

    /// Sample rate reported by the descriptors. The rate is chosen with the
    /// switch on the hardware; the Windows driver falls back to 96 kHz.
    pub fn sample_rate(&self) -> u32 {
        self.capture
            .or(self.playback)
            .map(|f| f.sample_rate)
            .ok()
            .filter(|&r| r != 0)
            .unwrap_or(DEFAULT_SAMPLE_RATE)
    }

    /// Product string from enumeration, if available.
    pub fn product_string(&self) -> Option<&str> {
        self.info.product_string()
    }

    /// Sends a vendor request with no data stage.
    pub fn send_vendor_request(&self, req: VendorRequest) -> Result<()> {
        let data = ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: req.request,
            value: req.value,
            index: req.index,
            data: &[],
        };
        #[cfg(not(target_os = "windows"))]
        {
            self.device.control_out(data, CONTROL_TIMEOUT).wait()?;
        }
        #[cfg(target_os = "windows")]
        {
            // WinUSB only allows control transfers through a claimed interface.
            let intf = self
                .device
                .claim_interface(ua101_protocol::INTF_MIDI)
                .wait()?;
            intf.control_out(data, CONTROL_TIMEOUT).wait()?;
        }
        Ok(())
    }

    /// Switches direct (hardware) monitoring. See
    /// [`ua101_protocol::monitor`]: the request encoding is inferred from
    /// Roland's ASIO driver.
    pub fn set_direct_monitor(&self, target: MonitorTarget, enabled: bool) -> Result<()> {
        self.send_vendor_request(direct_monitor_request(target, enabled))
    }

    /// Claims the MIDI interface (detaching the kernel driver on Linux).
    pub fn open_midi(&self) -> Result<MidiPort> {
        MidiPort::open(&self.device, &self.config)
    }

    /// Starts full-duplex isochronous streaming.
    pub fn start_stream(&self, config: StreamConfig) -> Result<DuplexStream> {
        let playback = self.playback?;
        let capture = self.capture?;
        crate::stream::start(self, playback, capture, config)
    }
}
