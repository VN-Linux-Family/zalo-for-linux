//! RFC 6464 Audio Level calculation.
//!
//! Level is in -dBov:
//!   0 = full scale (maximum volume)
//!   127 = silence (or rms < 1)

pub fn compute_rfc6464_level(pcm: &[i16]) -> u8 {
    if pcm.is_empty() {
        return 127;
    }
    let sum_sq: f64 = pcm.iter().map(|&v| (v as f64) * (v as f64)).sum();
    let rms = (sum_sq / pcm.len() as f64).sqrt();
    if rms < 1.0 {
        return 127;
    }
    let dbov = (-20.0 * (rms / 32768.0).log10()).round() as i32;
    dbov.clamp(0, 127) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silence() {
        let pcm = vec![0i16; 320];
        assert_eq!(compute_rfc6464_level(&pcm), 127);
    }

    #[test]
    fn test_full_scale() {
        let pcm = vec![32767i16; 320];
        assert_eq!(compute_rfc6464_level(&pcm), 0);
    }
}
