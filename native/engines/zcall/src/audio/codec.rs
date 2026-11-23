//! Pure-Rust Opus adapter preserving zcall's interleaved s16 API.

use rusty_opus::{
    Application, OpusDecoder as RustOpusDecoder, OpusEncoder as RustOpusEncoder, SignalType,
};

pub struct OpusEncoder {
    inner: RustOpusEncoder,
    channels: usize,
    pcm_f32: Vec<f32>,
}

impl OpusEncoder {
    pub fn new(sample_rate: i32, channels: i32, bitrate_bps: i32) -> Result<Self, String> {
        let channels =
            usize::try_from(channels).map_err(|_| "invalid Opus channel count".to_string())?;
        if channels == 0 {
            return Err("invalid Opus channel count".into());
        }
        let mut inner = RustOpusEncoder::new(sample_rate, channels, Application::Voip)
            .map_err(|error| format!("opus encoder create failed: {error:?}"))?;
        inner.bitrate_bps = bitrate_bps;
        inner.complexity = 10;
        inner.signal_type = Some(SignalType::Voice);
        inner.use_inband_fec = true;
        inner.lsb_depth = 16;
        Ok(Self {
            inner,
            channels,
            pcm_f32: Vec::new(),
        })
    }

    pub fn encode(&mut self, pcm: &[i16], out_buf: &mut [u8]) -> Result<usize, String> {
        if pcm.is_empty() || !pcm.len().is_multiple_of(self.channels) {
            return Err("invalid Opus PCM frame length".into());
        }
        self.pcm_f32.clear();
        self.pcm_f32
            .extend(pcm.iter().map(|&sample| sample as f32 / 32768.0));
        self.inner
            .encode(&self.pcm_f32, pcm.len() / self.channels, out_buf)
            .map_err(|error| format!("opus encode failed: {error:?}"))
    }
}

pub struct OpusDecoder {
    inner: RustOpusDecoder,
    channels: usize,
    pcm_f32: Vec<f32>,
}

impl OpusDecoder {
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self, String> {
        let channels =
            usize::try_from(channels).map_err(|_| "invalid Opus channel count".to_string())?;
        if channels == 0 {
            return Err("invalid Opus channel count".into());
        }
        let inner = RustOpusDecoder::new(sample_rate, channels)
            .map_err(|error| format!("opus decoder create failed: {error:?}"))?;
        Ok(Self {
            inner,
            channels,
            pcm_f32: Vec::new(),
        })
    }

    pub fn decode(
        &mut self,
        payload: &[u8],
        out_pcm: &mut [i16],
        decode_fec: bool,
    ) -> Result<usize, String> {
        if out_pcm.is_empty() || !out_pcm.len().is_multiple_of(self.channels) {
            return Err("invalid Opus output buffer length".into());
        }
        let frame_size = out_pcm.len() / self.channels;
        self.pcm_f32.resize(out_pcm.len(), 0.0);
        let decoded = if decode_fec && !payload.is_empty() {
            self.inner
                .decode_fec(payload, frame_size, &mut self.pcm_f32)
        } else {
            self.inner.decode(payload, frame_size, &mut self.pcm_f32)
        }
        .map_err(|error| format!("opus decode failed: {error:?}"))?;
        let samples = decoded.saturating_mul(self.channels).min(out_pcm.len());
        for (dst, &src) in out_pcm.iter_mut().zip(self.pcm_f32.iter()).take(samples) {
            *dst = (src * 32768.0)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_voice_frame_encodes_and_decodes_at_call_sample_rate() {
        let pcm = vec![0i16; 320];
        let mut encoder = OpusEncoder::new(16_000, 1, 32_000).expect("create Opus encoder");
        let mut packet = [0u8; 1500];
        let encoded = encoder
            .encode(&pcm, &mut packet)
            .expect("encode Opus frame");
        assert!(encoded > 0 && encoded < packet.len());
        let mut decoder = OpusDecoder::new(16_000, 1).expect("create Opus decoder");
        let mut decoded = vec![0i16; 640];
        let samples = decoder
            .decode(&packet[..encoded], &mut decoded, false)
            .expect("decode Opus frame");
        assert_eq!(samples, 320);
    }

    #[test]
    fn empty_packet_uses_plc_and_fec_path_is_available() {
        let mut encoder = OpusEncoder::new(16_000, 1, 32_000).unwrap();
        let mut packet = [0u8; 1500];
        let len = encoder.encode(&vec![1200i16; 320], &mut packet).unwrap();
        let mut decoder = OpusDecoder::new(16_000, 1).unwrap();
        let mut output = [0i16; 320];
        assert_eq!(
            decoder.decode(&packet[..len], &mut output, false).unwrap(),
            320
        );
        assert_eq!(decoder.decode(&[], &mut output, false).unwrap(), 320);
        assert_eq!(
            decoder.decode(&packet[..len], &mut output, true).unwrap(),
            320
        );
    }
}
