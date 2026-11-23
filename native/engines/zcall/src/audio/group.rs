//! Per-member group audio jitter buffers and software mixer.

use super::JitterBuffer;
use super::FRAME_SAMPLES;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MAX_GROUP_AUDIO_MEMBERS: usize = 64;
const MEMBER_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

struct MemberAudio {
    jitter: JitterBuffer,
    roc: u32,
    last_seq: Option<u16>,
    last_rx: Instant,
    primed: bool,
}

/// Maintains an independent sequence/ROC and Opus decoder for each RTP SSRC,
/// then mixes one decoded frame from each active member into a mono output.
pub struct GroupAudioMixer {
    members: HashMap<u32, MemberAudio>,
}

impl GroupAudioMixer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            members: HashMap::new(),
        }
    }

    /// Estimate the SRTP rollover counter for an incoming sequence number.
    /// Call `push_packet` only after packet authentication succeeds.
    #[must_use]
    pub fn candidate_roc(&self, ssrc: u32, seq: u16) -> u32 {
        let Some(member) = self.members.get(&ssrc) else {
            return 0;
        };
        let Some(last) = member.last_seq else {
            return member.roc;
        };
        if seq < last && last.wrapping_sub(seq) > 0x8000 {
            member.roc.wrapping_add(1)
        } else if seq > last && seq.wrapping_sub(last) > 0x8000 && member.roc > 0 {
            member.roc - 1
        } else {
            member.roc
        }
    }

    /// Candidate order follows PR #220: guessed ROC first, then ROC 0 and
    /// adjacent rollover values. SRTP HMAC decides which candidate is valid.
    #[must_use]
    pub fn candidate_rocs(&self, ssrc: u32, seq: u16) -> Vec<u32> {
        let guessed = self.candidate_roc(ssrc, seq);
        let mut values = vec![guessed, 0, guessed.wrapping_add(1)];
        if guessed > 0 {
            values.push(guessed - 1);
        }
        let mut seen = std::collections::HashSet::new();
        values.retain(|value| seen.insert(*value));
        values
    }

    /// Queue one authenticated/decoded RTP payload. Returns false when the
    /// member cap is reached and a new SSRC cannot be admitted.
    pub fn push_packet(&mut self, ssrc: u32, seq: u16, roc: u32, opus: Vec<u8>) -> bool {
        if ssrc == 0 || opus.is_empty() {
            return false;
        }
        if !self.members.contains_key(&ssrc) && self.members.len() >= MAX_GROUP_AUDIO_MEMBERS {
            self.expire_idle();
            if self.members.len() >= MAX_GROUP_AUDIO_MEMBERS {
                return false;
            }
        }
        let member = if let Some(member) = self.members.get_mut(&ssrc) {
            member
        } else {
            let Ok(jitter) = JitterBuffer::new(16_000, 1, 10) else {
                return false;
            };
            self.members.entry(ssrc).or_insert(MemberAudio {
                jitter,
                roc: 0,
                last_seq: None,
                last_rx: Instant::now(),
                primed: false,
            })
        };
        member.jitter.push((roc << 16) | u32::from(seq), opus);
        if let Some(last) = member.last_seq {
            let previous = (u64::from(member.roc) << 16) | u64::from(last);
            let next = (u64::from(roc) << 16) | u64::from(seq);
            if next > previous || previous - next > 100 {
                member.roc = roc;
                member.last_seq = Some(seq);
            }
        } else {
            member.roc = roc;
            member.last_seq = Some(seq);
        }
        member.last_rx = Instant::now();
        member.primed |= member.jitter.is_ready();
        true
    }

    /// Decode and sum one frame per primed member. Samples are clamped to the
    /// i16 range to prevent integer wrap when several users speak at once.
    /// Returns the number of member streams mixed into this frame.
    pub fn mix_frame(&mut self, output: &mut [i16]) -> usize {
        if output.len() < FRAME_SAMPLES {
            return 0;
        }
        output[..FRAME_SAMPLES].fill(0);
        self.expire_idle();
        let mut mixed = 0usize;
        let mut frame = [0i16; FRAME_SAMPLES];
        for member in self.members.values_mut() {
            if !member.primed {
                member.primed = member.jitter.is_ready();
                if !member.primed {
                    continue;
                }
            }
            if !member.jitter.pop_frame(&mut frame) {
                continue;
            }
            mixed += 1;
            for (out, sample) in output[..FRAME_SAMPLES].iter_mut().zip(frame.iter()) {
                *out = out.saturating_add(*sample);
            }
        }
        mixed
    }

    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    fn expire_idle(&mut self) {
        let now = Instant::now();
        self.members
            .retain(|_, member| now.duration_since(member.last_rx) < MEMBER_IDLE_TIMEOUT);
    }
}

impl Default for GroupAudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../tests/audio/group.rs"]
mod tests;
