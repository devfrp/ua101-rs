//! User-space USB driver for the Roland/EDIROL UA-101.
//!
//! * Enumeration, descriptors, vendor control requests and bulk MIDI go
//!   through [`nusb`], so they work on Linux, macOS and Windows.
//! * Isochronous audio streaming needs a dedicated backend per OS, because
//!   `nusb` does not expose isochronous transfers. The Linux backend
//!   (`usbfs`) is implemented. macOS (IOKit) and Windows (WinUSB isoch API)
//!   return [`Error::Unsupported`] for now.
//!
//! ```no_run
//! use ua101_usb::Ua101;
//! let dev = Ua101::open_first()?;
//! println!("{} Hz", dev.sample_rate());
//! # Ok::<(), ua101_usb::Error>(())
//! ```

mod device;
mod error;
mod midi;
pub mod stream;

#[cfg(target_os = "linux")]
mod usbfs;

pub use device::{Ua101, list_devices};
pub use error::{Error, Result};
pub use midi::MidiPort;
pub use stream::{DuplexStream, StreamConfig, StreamStats};

pub use nusb::{DeviceInfo, Speed};
pub use ua101_protocol as protocol;
