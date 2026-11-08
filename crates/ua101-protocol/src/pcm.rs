//! Sample packing between host-side samples and the wire format.
//!
//! Host-side samples are `i32` values in the signed 24-bit range
//! `-0x80_0000..=0x7f_ffff`. The Windows driver's playback mixer sums all
//! client streams into that range, clips to ±`0x7f_ffff`, then writes each
//! sample as a little-endian 32-bit word holding the sample shifted left by
//! 8 bits (24 significant bits, left-justified). That is the 4-byte subframe
//! layout. A 3-byte subframe layout (packed little-endian 24-bit) is also
//! supported because the descriptor's `bSubframeSize` decides.

/// Largest positive 24-bit sample.
pub const S24_MAX: i32 = 0x7f_ffff;
/// Most negative 24-bit sample.
pub const S24_MIN: i32 = -0x80_0000;

/// On-the-wire sample layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// 24-bit sample left-justified in a little-endian 32-bit word.
    S24In32Le,
    /// Packed little-endian 24-bit sample.
    S24PackedLe,
}

impl WireFormat {
    /// Chooses the layout from `bSubframeSize`.
    pub const fn from_subframe_size(size: u8) -> Option<Self> {
        match size {
            4 => Some(Self::S24In32Le),
            3 => Some(Self::S24PackedLe),
            _ => None,
        }
    }

    /// Bytes per sample on the wire.
    pub const fn bytes(self) -> usize {
        match self {
            Self::S24In32Le => 4,
            Self::S24PackedLe => 3,
        }
    }
}

/// Clips an accumulated value to the 24-bit range, as the driver's mixer does.
#[inline]
pub const fn clip_s24(value: i64) -> i32 {
    if value > S24_MAX as i64 {
        S24_MAX
    } else if value < S24_MIN as i64 {
        S24_MIN
    } else {
        value as i32
    }
}

/// Encodes 24-bit samples into wire bytes. Returns the number of samples
/// written, which is limited by whichever buffer is shorter.
pub fn encode(format: WireFormat, samples: &[i32], out: &mut [u8]) -> usize {
    let n = samples.len().min(out.len() / format.bytes());
    match format {
        WireFormat::S24In32Le => {
            for (s, chunk) in samples[..n].iter().zip(out.chunks_exact_mut(4)) {
                let v = clip_s24(*s as i64) << 8;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        WireFormat::S24PackedLe => {
            for (s, chunk) in samples[..n].iter().zip(out.chunks_exact_mut(3)) {
                let v = clip_s24(*s as i64).to_le_bytes();
                chunk.copy_from_slice(&v[..3]);
            }
        }
    }
    n
}

/// Decodes wire bytes into 24-bit samples. Returns the number of samples
/// written.
pub fn decode(format: WireFormat, bytes: &[u8], out: &mut [i32]) -> usize {
    let n = out.len().min(bytes.len() / format.bytes());
    match format {
        WireFormat::S24In32Le => {
            for (chunk, s) in bytes.chunks_exact(4).zip(out[..n].iter_mut()) {
                *s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) >> 8;
            }
        }
        WireFormat::S24PackedLe => {
            for (chunk, s) in bytes.chunks_exact(3).zip(out[..n].iter_mut()) {
                *s = i32::from_le_bytes([0, chunk[0], chunk[1], chunk[2]]) >> 8;
            }
        }
    }
    n
}

/// Converts a float sample in `[-1.0, 1.0]` to 24-bit.
#[inline]
pub fn f32_to_s24(x: f32) -> i32 {
    // `f64::round` is not available in `no_std`; round half away from zero.
    let v = x as f64 * S24_MAX as f64;
    let r = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    clip_s24(r as i64)
}

/// Converts a 24-bit sample to a float in `[-1.0, 1.0]`.
#[inline]
pub fn s24_to_f32(x: i32) -> f32 {
    x as f32 / 8_388_608.0
}

/// Converts a 16-bit sample to 24-bit.
#[inline]
pub const fn s16_to_s24(x: i16) -> i32 {
    (x as i32) << 8
}

/// Converts a 24-bit sample to 16-bit by truncation.
#[inline]
pub const fn s24_to_s16(x: i32) -> i16 {
    (x >> 8) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s24_in_32_matches_driver_shift() {
        let mut out = [0u8; 8];
        assert_eq!(encode(WireFormat::S24In32Le, &[1, -1], &mut out), 2);
        assert_eq!(out, [0x00, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn roundtrip_both_formats() {
        let src = [0, 1, -1, S24_MAX, S24_MIN, 123_456, -654_321];
        for fmt in [WireFormat::S24In32Le, WireFormat::S24PackedLe] {
            let mut bytes = [0u8; 7 * 4];
            let n = encode(fmt, &src, &mut bytes);
            assert_eq!(n, src.len());
            let mut back = [0i32; 7];
            assert_eq!(decode(fmt, &bytes[..n * fmt.bytes()], &mut back), n);
            assert_eq!(back, src);
        }
    }

    #[test]
    fn encode_clips_out_of_range() {
        let mut bytes = [0u8; 8];
        encode(WireFormat::S24In32Le, &[i32::MAX, i32::MIN], &mut bytes);
        let mut back = [0i32; 2];
        decode(WireFormat::S24In32Le, &bytes, &mut back);
        assert_eq!(back, [S24_MAX, S24_MIN]);
    }

    #[test]
    fn short_buffers_limit_count() {
        let mut bytes = [0u8; 5];
        assert_eq!(encode(WireFormat::S24In32Le, &[1, 2, 3], &mut bytes), 1);
        assert_eq!(encode(WireFormat::S24PackedLe, &[1, 2, 3], &mut bytes), 1);
    }

    #[test]
    fn float_conversion() {
        assert_eq!(f32_to_s24(1.0), S24_MAX);
        assert_eq!(f32_to_s24(-2.0), S24_MIN);
        assert_eq!(f32_to_s24(0.0), 0);
        assert!((s24_to_f32(S24_MAX) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn s16_conversion() {
        assert_eq!(s16_to_s24(i16::MAX), 0x7f_ff00);
        assert_eq!(s24_to_s16(s16_to_s24(-1234)), -1234);
    }
}
