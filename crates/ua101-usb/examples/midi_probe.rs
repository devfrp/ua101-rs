// Temporary probe: send RQ1 on cable 1 with interface 2 in alt 0 or 1 and dump replies.
use nusb::{
    MaybeFuture,
    transfer::{Buffer, Bulk, In, Interrupt, Out},
};
use std::time::{Duration, Instant};
use ua101_usb::protocol::{midi::Encoder, sysex};

fn main() {
    let alt: u8 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let addr: Vec<u8> = std::env::args()
        .nth(2)
        .unwrap_or("01 00 00 00".into())
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect();
    let size: u32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x48);
    let info = ua101_usb::list_devices().unwrap().remove(0);
    let dev = info.open().wait().unwrap();
    let intf = dev.detach_and_claim_interface(2).wait().unwrap();
    if alt != 0 {
        intf.set_alt_setting(alt).wait().unwrap();
    }
    println!("alt {}", intf.get_alt_setting());
    let mut out = intf.endpoint::<Bulk, Out>(0x04).unwrap();
    let msg = sysex::rq1([addr[0], addr[1], addr[2], addr[3]], size);
    let mut enc = Encoder::new(1);
    let mut pk = Vec::new();
    for &b in msg.as_bytes() {
        if let Some(p) = enc.push(b) {
            pk.extend_from_slice(&p);
        }
    }
    println!("send {:02x?}", pk);
    let deadline = Instant::now() + Duration::from_millis(1500);
    let r = out.transfer_blocking(Buffer::from(pk), Duration::from_secs(1));
    println!("out status {:?}", r.status);
    if alt == 0 {
        let mut inp = intf.endpoint::<Bulk, In>(0x83).unwrap();
        while Instant::now() < deadline {
            let c = inp.transfer_blocking(Buffer::new(512), Duration::from_millis(300));
            if c.actual_len > 0 || c.status.is_ok() {
                println!(
                    "in {:?} {:02x?}",
                    c.status,
                    &c.buffer[..c.actual_len.min(c.buffer.len())]
                );
            }
        }
    } else {
        let mut inp = intf.endpoint::<Interrupt, In>(0x83).unwrap();
        while Instant::now() < deadline {
            let c = inp.transfer_blocking(Buffer::new(512), Duration::from_millis(300));
            if c.actual_len > 0 || c.status.is_ok() {
                println!(
                    "in {:?} {:02x?}",
                    c.status,
                    &c.buffer[..c.actual_len.min(c.buffer.len())]
                );
            }
        }
    }
    drop(intf);
    ua101_usb::reattach_kernel_driver(&dev);
}
