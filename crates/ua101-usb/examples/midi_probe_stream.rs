// Temporary probe: RQ1 on cable 1 while the isochronous streams run (silence).
use std::time::Duration;
fn main() {
    let a: Vec<u8> = std::env::args()
        .nth(1)
        .unwrap_or("01 00 00 00".into())
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect();
    let size: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x48);
    let dev = ua101_usb::Ua101::open_first().unwrap();
    let stream = dev.start_stream(Default::default()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let mut midi = dev.open_midi().unwrap();
    for _ in 0..3 {
        let r = midi
            .read_parameters([a[0], a[1], a[2], a[3]], size, Duration::from_millis(500))
            .unwrap();
        println!("replies: {}", r.len());
        for (ad, d) in r {
            println!("{:02x?}: {:02x?}", ad, d);
        }
    }
    drop(midi);
    println!("{:?}", stream.stop().unwrap());
}
