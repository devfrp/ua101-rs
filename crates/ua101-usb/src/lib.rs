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

pub use device::{Ua101, list_devices, reattach_kernel_driver};
pub use error::{Error, Result};
pub use midi::MidiPort;
pub use stream::{DuplexStream, StreamConfig, StreamControl, StreamStats};

pub use nusb::{DeviceInfo, Speed};

/// Path of the `ua101d` control socket shared by the daemon and its clients:
/// `$XDG_RUNTIME_DIR/ua101d.sock`, or `/tmp/ua101d-$USER.sock`.
#[cfg(unix)]
pub fn daemon_socket_path() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => std::path::PathBuf::from(dir).join("ua101d.sock"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::path::PathBuf::from(format!("/tmp/ua101d-{user}.sock"))
        }
    }
}
pub use ua101_protocol as protocol;
