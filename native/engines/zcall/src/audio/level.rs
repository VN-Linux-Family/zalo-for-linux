//! RFC 6464 Audio Level calculation.
//!
//! Level is in -dBov:
//!   0 = full scale (maximum volume)
//!   127 = silence (or rms < 1)

#[must_use]
pub fn compute_rfc6464_level(pcm: &[i16]) -> u8 {
    if pcm.is_empty() {
        return 127;
    }
    let sum_sq: f64 = pcm.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    let rms = (sum_sq / pcm.len() as f64).sqrt();
    if rms < 1.0 {
        return 127;
    }
    let dbov = (-20.0 * (rms / 32768.0).log10()).round() as i32;
    dbov.clamp(0, 127) as u8
}

#[cfg(test)]
#[path = "../tests/audio/level.rs"]
mod tests;
