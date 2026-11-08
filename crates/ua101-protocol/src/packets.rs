//! Isochronous packet sizing.
//!
//! The device is the clock master. Capture packets arrive with whatever
//! number of frames the device produced in that (micro)frame; playback
//! packets must carry a matching number of frames or the device's buffer
//! drifts. Two helpers live here:
//!
//! * [`distribute_frames`] reproduces the Windows driver's split of a frame
//!   count over the packets of one transfer: packet `i` ends at
//!   `floor(total * (i + 1) / packets)` frames.
//! * [`FrameClock`] produces the nominal per-packet frame count for a sample
//!   rate, used until capture feedback is available.

/// Number of isochronous service intervals per second.
///
/// High speed: 8000 microframes per second divided by `2^(bInterval-1)`.
/// Full speed: 1000 frames per second (isochronous `bInterval` must be 1).
pub const fn packets_per_second(high_speed: bool, b_interval: u8) -> u32 {
    if high_speed {
        let exp = if b_interval == 0 {
            0
        } else if b_interval > 16 {
            15
        } else {
            b_interval - 1
        };
        8000 >> exp
    } else {
        1000
    }
}

/// Splits `total_frames` over `packets.len()` packets the way the Windows
/// driver does and writes each packet's frame count. Returns the total, which
/// always equals `total_frames` when `packets` is non-empty.
pub fn distribute_frames(total_frames: u32, packets: &mut [u32]) -> u32 {
    let n = packets.len() as u64;
    if n == 0 {
        return 0;
    }
    let mut prev = 0u64;
    for (i, slot) in packets.iter_mut().enumerate() {
        let end = total_frames as u64 * (i as u64 + 1) / n;
        *slot = (end - prev) as u32;
        prev = end;
    }
    prev as u32
}

/// Fractional frame accumulator that yields how many frames belong to each
/// successive packet at a nominal sample rate (Bresenham style, no drift).
#[derive(Debug, Clone)]
pub struct FrameClock {
    rate: u32,
    packets_per_second: u32,
    remainder: u32,
}

impl FrameClock {
    pub const fn new(rate: u32, packets_per_second: u32) -> Self {
        Self {
            rate,
            packets_per_second,
            remainder: 0,
        }
    }

    /// Frames in the next packet.
    pub fn next_packet(&mut self) -> u32 {
        if self.packets_per_second == 0 {
            return 0;
        }
        let acc = self.remainder + self.rate;
        let frames = acc / self.packets_per_second;
        self.remainder = acc % self.packets_per_second;
        frames
    }

    /// Largest frame count [`next_packet`](Self::next_packet) can return.
    pub const fn max_packet_frames(&self) -> u32 {
        if self.packets_per_second == 0 {
            0
        } else {
            self.rate.div_ceil(self.packets_per_second)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals() {
        assert_eq!(packets_per_second(true, 1), 8000);
        assert_eq!(packets_per_second(true, 4), 1000);
        assert_eq!(packets_per_second(false, 1), 1000);
    }

    #[test]
    fn distribution_matches_driver_formula() {
        let mut p = [0u32; 8];
        assert_eq!(distribute_frames(44, &mut p), 44);
        // floor(44*(i+1)/8) = 5,11,16,22,27,33,38,44
        assert_eq!(p, [5, 6, 5, 6, 5, 6, 5, 6]);

        let mut p = [0u32; 8];
        assert_eq!(distribute_frames(48, &mut p), 48);
        assert_eq!(p, [6; 8]);
    }

    #[test]
    fn distribution_of_nothing() {
        assert_eq!(distribute_frames(10, &mut []), 0);
        let mut p = [9u32; 4];
        distribute_frames(0, &mut p);
        assert_eq!(p, [0; 4]);
    }

    #[test]
    fn clock_is_exact_over_one_second() {
        for rate in [44_100, 48_000, 88_200, 96_000] {
            let mut clk = FrameClock::new(rate, 8000);
            let total: u32 = (0..8000).map(|_| clk.next_packet()).sum();
            assert_eq!(total, rate);
            assert!(clk.max_packet_frames() >= rate / 8000);
        }
    }

    #[test]
    fn clock_44k1_alternates() {
        let mut clk = FrameClock::new(44_100, 8000);
        let first: [u32; 4] = core::array::from_fn(|_| clk.next_packet());
        assert!(first.iter().all(|&f| f == 5 || f == 6));
    }
}
