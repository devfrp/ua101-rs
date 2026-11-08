// Temporary probe: dump every incoming USB-MIDI packet for N seconds, sending RQ1 01 00 00 00 every 2 s.
use nusb::{
    MaybeFuture,
    transfer::{Buffer, Bulk, In, Out},
};
use std::time::{Duration, Instant};
use ua101_usb::protocol::{midi::Encoder, sysex};
fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let info = ua101_usb::list_devices().unwrap().remove(0);
    let dev = info.open().wait().unwrap();
    let intf = dev.detach_and_claim_interface(2).wait().unwrap();
    let mut out = intf.endpoint::<Bulk, Out>(0x04).unwrap();
    let mut inp = intf.endpoint::<Bulk, In>(0x83).unwrap();
    for _ in 0..4 {
        inp.submit(Buffer::new(512));
    }
    let end = Instant::now() + Duration::from_secs(secs);
    let mut next_rq = Instant::now();
    while Instant::now() < end {
        if Instant::now() >= next_rq {
            let mut enc = Encoder::new(1);
            let mut pk = Vec::new();
            for &b in sysex::rq1([1, 0, 0, 0], 0x48).as_bytes() {
                if let Some(p) = enc.push(b) {
                    pk.extend_from_slice(&p);
                }
            }
            let _ = out.transfer_blocking(Buffer::from(pk), Duration::from_millis(500));
            next_rq = Instant::now() + Duration::from_secs(2);
        }
        if let Some(c) = inp.wait_next_complete(Duration::from_millis(100)) {
            let n = c.actual_len;
            if n > 0 {
                println!(
                    "{:6} ms IN {:?}: {:02x?}",
                    (secs * 1000) as i64 - (end - Instant::now()).as_millis() as i64,
                    c.status,
                    &c.buffer[..n]
                );
            } else if c.status.is_err() {
                println!("IN err {:?}", c.status);
            }
            inp.submit(Buffer::new(512));
        }
    }
    inp.cancel_all();
    while inp.pending() > 0 {
        let _ = inp.wait_next_complete(Duration::from_millis(200));
    }
    drop(inp);
    drop(out);
    drop(intf);
    ua101_usb::reattach_kernel_driver(&dev);
    println!("done");
}
