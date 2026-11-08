use ua101_protocol::descriptors::ParseError;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by the driver.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no UA-101 found (USB 0582:007d)")]
    NotFound,
    #[error("USB error: {0}")]
    Usb(#[from] nusb::Error),
    #[error("USB transfer error: {0}")]
    Transfer(#[from] nusb::transfer::TransferError),
    #[error("cannot read the active configuration: {0}")]
    Configuration(#[from] nusb::ActiveConfigurationError),
    #[error("descriptor error: {0}")]
    Descriptor(#[from] ParseError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported sample layout: bSubframeSize = {0}")]
    UnsupportedSubframe(u8),
    #[error("not supported on this platform yet: {0}")]
    Unsupported(&'static str),
    #[error("the device was disconnected")]
    Disconnected,
    #[error("the streaming thread panicked")]
    ThreadPanicked,
}
