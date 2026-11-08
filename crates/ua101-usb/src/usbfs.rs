//! Minimal Linux `usbfs` access for isochronous transfers.
//!
//! `nusb` covers control and bulk transfers but does not expose isochronous
//! URBs, so this module talks to `/dev/bus/usb/BBB/DDD` directly with the
//! `USBDEVFS_*` ioctls from `<linux/usbdevice_fs.h>`.
//!
//! The ioctl numbers use the generic `_IOC` encoding shared by x86, x86_64,
//! ARM, AArch64 and RISC-V. Architectures with a different encoding (MIPS,
//! PowerPC, SPARC) are not supported.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io,
    mem::size_of,
    os::fd::AsRawFd,
    ptr,
};

/// Upper bound on packets per URB supported by [`IsoUrb`].
pub const MAX_ISO_PACKETS: usize = 64;

const USBDEVFS_URB_TYPE_ISO: u8 = 0;
const USBDEVFS_URB_ISO_ASAP: u32 = 0x02;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct IsoPacketDesc {
    pub length: u32,
    pub actual_length: u32,
    pub status: u32,
}

/// `struct usbdevfs_urb` without its flexible array member.
#[repr(C)]
struct UrbHeader {
    urb_type: u8,
    endpoint: u8,
    status: i32,
    flags: u32,
    buffer: *mut c_void,
    buffer_length: i32,
    actual_length: i32,
    start_frame: i32,
    number_of_packets: i32,
    error_count: i32,
    signr: u32,
    usercontext: *mut c_void,
}

/// An isochronous URB followed by its packet descriptors, laid out exactly as
/// the kernel expects. It must not move while submitted, so it is always
/// boxed.
#[repr(C)]
pub struct IsoUrb {
    hdr: UrbHeader,
    packets: [IsoPacketDesc; MAX_ISO_PACKETS],
}

// The raw pointers only ever point into buffers owned by the same streaming
// thread.
unsafe impl Send for IsoUrb {}

impl IsoUrb {
    pub fn new(endpoint: u8, tag: usize) -> Box<Self> {
        Box::new(Self {
            hdr: UrbHeader {
                urb_type: USBDEVFS_URB_TYPE_ISO,
                endpoint,
                status: 0,
                flags: USBDEVFS_URB_ISO_ASAP,
                buffer: ptr::null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets: 0,
                error_count: 0,
                signr: 0,
                usercontext: tag as *mut c_void,
            },
            packets: [IsoPacketDesc::default(); MAX_ISO_PACKETS],
        })
    }

    /// Prepares the URB for submission. `buffer` must stay alive and
    /// unmoved until the URB is reaped; `lengths` gives each packet's size
    /// and the packets are laid out back to back in `buffer`.
    pub fn prepare(&mut self, buffer: &mut [u8], lengths: &[u32]) {
        assert!(lengths.len() <= MAX_ISO_PACKETS, "too many iso packets");
        let total: u32 = lengths.iter().sum();
        assert!(total as usize <= buffer.len(), "iso buffer too small");
        for (desc, &len) in self.packets.iter_mut().zip(lengths) {
            *desc = IsoPacketDesc {
                length: len,
                actual_length: 0,
                status: 0,
            };
        }
        self.hdr.status = 0;
        self.hdr.flags = USBDEVFS_URB_ISO_ASAP;
        self.hdr.buffer = buffer.as_mut_ptr().cast();
        self.hdr.buffer_length = total as i32;
        self.hdr.actual_length = 0;
        self.hdr.start_frame = 0;
        self.hdr.number_of_packets = lengths.len() as i32;
        self.hdr.error_count = 0;
    }

    /// Tag given at construction (index of the slot owning this URB).
    pub fn tag(&self) -> usize {
        self.hdr.usercontext as usize
    }

    /// Kernel status of the whole URB (0 or a negative errno).
    pub fn status(&self) -> i32 {
        self.hdr.status
    }

    /// Number of packets that reported an error.
    pub fn error_count(&self) -> i32 {
        self.hdr.error_count
    }

    /// Packet descriptors of the last submission.
    pub fn packets(&self) -> &[IsoPacketDesc] {
        &self.packets[..self.hdr.number_of_packets.max(0) as usize]
    }
}

#[repr(C)]
struct SetInterface {
    interface: u32,
    altsetting: u32,
}

#[repr(C)]
struct UsbdevfsIoctl {
    ifno: i32,
    ioctl_code: i32,
    data: *mut c_void,
}

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((b'U' as u32) << 8) | nr
}

const USBDEVFS_SETINTERFACE: u32 = ioc(IOC_READ, 4, size_of::<SetInterface>());
const USBDEVFS_SUBMITURB: u32 = ioc(IOC_READ, 10, size_of::<UrbHeader>());
const USBDEVFS_DISCARDURB: u32 = ioc(IOC_NONE, 11, 0);
const USBDEVFS_REAPURB: u32 = ioc(IOC_WRITE, 12, size_of::<*mut c_void>());
const USBDEVFS_REAPURBNDELAY: u32 = ioc(IOC_WRITE, 13, size_of::<*mut c_void>());
const USBDEVFS_CLAIMINTERFACE: u32 = ioc(IOC_READ, 15, size_of::<u32>());
const USBDEVFS_RELEASEINTERFACE: u32 = ioc(IOC_READ, 16, size_of::<u32>());
const USBDEVFS_IOCTL: u32 = ioc(IOC_READ | IOC_WRITE, 18, size_of::<UsbdevfsIoctl>());
const USBDEVFS_DISCONNECT: u32 = ioc(IOC_NONE, 22, 0);
const USBDEVFS_CONNECT: u32 = ioc(IOC_NONE, 23, 0);

/// An open usbfs device node.
pub struct UsbFs {
    file: File,
    claimed: Vec<u32>,
    detached: Vec<u32>,
    reattach: bool,
}

impl UsbFs {
    /// Opens `/dev/bus/usb/{bus}/{address}`.
    pub fn open(bus: u8, address: u8) -> io::Result<Self> {
        let path = format!("/dev/bus/usb/{bus:03}/{address:03}");
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        Ok(Self {
            file,
            claimed: Vec::new(),
            detached: Vec::new(),
            reattach: true,
        })
    }

    /// Whether kernel drivers detached by [`detach_kernel_driver`] are
    /// reattached on drop (default `true`).
    pub fn set_reattach(&mut self, reattach: bool) {
        self.reattach = reattach;
    }

    fn ioctl(&self, request: u32, arg: *mut c_void) -> io::Result<i32> {
        // SAFETY: each caller passes the argument type the request expects.
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, arg) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    fn interface_ioctl(&self, interface: u32, code: u32) -> io::Result<i32> {
        let mut cmd = UsbdevfsIoctl {
            ifno: interface as i32,
            ioctl_code: code as i32,
            data: ptr::null_mut(),
        };
        self.ioctl(USBDEVFS_IOCTL, (&mut cmd as *mut UsbdevfsIoctl).cast())
    }

    /// Detaches the kernel driver bound to `interface`. Returns `false` if
    /// no driver was bound.
    pub fn detach_kernel_driver(&mut self, interface: u32) -> io::Result<bool> {
        match self.interface_ioctl(interface, USBDEVFS_DISCONNECT) {
            Ok(_) => {
                self.detached.push(interface);
                Ok(true)
            }
            Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn claim_interface(&mut self, interface: u32) -> io::Result<()> {
        let mut n = interface;
        self.ioctl(USBDEVFS_CLAIMINTERFACE, (&mut n as *mut u32).cast())?;
        self.claimed.push(interface);
        Ok(())
    }

    pub fn set_interface(&self, interface: u32, alt_setting: u32) -> io::Result<()> {
        let mut si = SetInterface {
            interface,
            altsetting: alt_setting,
        };
        self.ioctl(USBDEVFS_SETINTERFACE, (&mut si as *mut SetInterface).cast())?;
        Ok(())
    }

    /// Submits a prepared URB.
    ///
    /// # Safety
    /// The URB and the buffer passed to [`IsoUrb::prepare`] must stay alive
    /// and unmoved until the URB is returned by a reap call.
    pub unsafe fn submit(&self, urb: &mut IsoUrb) -> io::Result<()> {
        self.ioctl(USBDEVFS_SUBMITURB, (urb as *mut IsoUrb).cast())?;
        Ok(())
    }

    /// Cancels a submitted URB. It is still returned by a later reap.
    pub fn discard(&self, urb: *mut IsoUrb) -> io::Result<()> {
        match self.ioctl(USBDEVFS_DISCARDURB, urb.cast()) {
            Ok(_) => Ok(()),
            // Already completed: nothing to cancel.
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn reap_with(&self, request: u32) -> io::Result<Option<*mut IsoUrb>> {
        let mut out: *mut c_void = ptr::null_mut();
        match self.ioctl(request, (&mut out as *mut *mut c_void).cast()) {
            Ok(_) => Ok(Some(out.cast())),
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Returns a completed URB without blocking, if one is ready.
    pub fn reap_nonblocking(&self) -> io::Result<Option<*mut IsoUrb>> {
        self.reap_with(USBDEVFS_REAPURBNDELAY)
    }

    /// Blocks until a submitted URB completes.
    pub fn reap_blocking(&self) -> io::Result<*mut IsoUrb> {
        loop {
            if let Some(urb) = self.reap_with(USBDEVFS_REAPURB)? {
                return Ok(urb);
            }
        }
    }

    /// Waits until a URB completion is pending or `timeout_ms` elapses.
    pub fn wait(&self, timeout_ms: i32) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(e);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(io::Error::from_raw_os_error(libc::ENODEV));
        }
        Ok(r > 0)
    }
}

impl Drop for UsbFs {
    fn drop(&mut self) {
        for intf in std::mem::take(&mut self.claimed) {
            let _ = self.set_interface(intf, 0);
            let mut n = intf;
            let _ = self.ioctl(USBDEVFS_RELEASEINTERFACE, (&mut n as *mut u32).cast());
        }
        if self.reattach {
            for intf in std::mem::take(&mut self.detached) {
                let _ = self.interface_ioctl(intf, USBDEVFS_CONNECT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_kernel_abi() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(size_of::<UrbHeader>(), 56);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(size_of::<UrbHeader>(), 44);
        assert_eq!(size_of::<IsoPacketDesc>(), 12);
        assert_eq!(
            std::mem::offset_of!(IsoUrb, packets),
            size_of::<UrbHeader>()
        );
    }

    #[test]
    fn ioctl_numbers_match_headers() {
        // Values from <linux/usbdevice_fs.h> on x86_64.
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(USBDEVFS_SUBMITURB, 0x8038_550a);
            assert_eq!(USBDEVFS_REAPURB, 0x4008_550c);
            assert_eq!(USBDEVFS_REAPURBNDELAY, 0x4008_550d);
            assert_eq!(USBDEVFS_IOCTL, 0xc010_5512);
        }
        assert_eq!(USBDEVFS_DISCARDURB, 0x0000_550b);
        assert_eq!(USBDEVFS_CLAIMINTERFACE, 0x8004_550f);
        assert_eq!(USBDEVFS_RELEASEINTERFACE, 0x8004_5510);
        assert_eq!(USBDEVFS_SETINTERFACE, 0x8008_5504);
        assert_eq!(USBDEVFS_DISCONNECT, 0x0000_5516);
        assert_eq!(USBDEVFS_CONNECT, 0x0000_5517);
    }
}
