//! H.264 RTP access-unit assembly.
//!
//! Zalo sends Annex-B H.264 NAL chunks directly in RTP payloads. Large access
//! units use its two-byte 0x1c FU header. Packets can arrive out of order over
//! P2P and relay paths, so assembly is keyed by RTP timestamp and extended
//! sequence number rather than assuming arrival order.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const VIDEO_PT_KEY: u8 = 98;
pub const VIDEO_PT_INTER: u8 = 97;
const REORDER_FRAMES: usize = 3;
const REORDER_WINDOW: Duration = Duration::from_millis(200);
const MAX_PENDING: usize = 30;
const START_CODE: &[u8] = &[0, 0, 0, 1];

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub key: bool,
    pub codec: String,
    pub data: Vec<u8>,
    pub src: Option<u32>,
    pub timestamp: u32,
}

#[derive(Default)]
struct PendingFrame {
    parts: BTreeMap<u64, Vec<u8>>,
    key: bool,
    end: Option<u64>,
}

struct ReadyFrame {
    key: bool,
    data: Vec<u8>,
    timestamp: u32,
    last: u64,
    since: Instant,
}

pub struct VideoAssembler {
    group: bool,
    pending: BTreeMap<u32, PendingFrame>,
    ready: BTreeMap<u64, ReadyFrame>,
    last_idx: Option<u64>,
    started: bool,
    need_key: bool,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    keyframe_requested: bool,
}

impl Default for VideoAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoAssembler {
    pub fn new() -> Self {
        Self::with_group(false)
    }

    pub fn with_group(group: bool) -> Self {
        Self {
            group,
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            last_idx: None,
            started: false,
            need_key: false,
            sps: None,
            pps: None,
            keyframe_requested: false,
        }
    }

    /// Add one authenticated RTP payload and return every access unit made
    /// ready by this packet. roc is the SRTP rollover counter for this SSRC.
    pub fn push_packet(
        &mut self,
        seq: u16,
        roc: u32,
        timestamp: u32,
        payload: &[u8],
        marker: bool,
        pt: u8,
    ) -> Vec<VideoFrame> {
        if payload.is_empty() || (!self.group && pt != VIDEO_PT_KEY && pt != VIDEO_PT_INTER) {
            return Vec::new();
        }
        let idx = (u64::from(roc) << 16) | u64::from(seq);
        if self.last_idx.is_some_and(|last| idx <= last) {
            return Vec::new();
        }

        let frame = self.pending.entry(timestamp).or_default();
        frame.parts.insert(idx, payload.to_vec());
        frame.key |= pt == VIDEO_PT_KEY;
        if marker {
            frame.end = Some(idx);
        }
        if self.pending.len() > MAX_PENDING {
            if let Some(oldest) = self.pending.keys().next().copied() {
                self.pending.remove(&oldest);
            }
        }
        self.complete(timestamp);
        self.flush()
    }

    pub fn take_keyframe_request(&mut self) -> bool {
        std::mem::take(&mut self.keyframe_requested)
    }

    fn complete(&mut self, timestamp: u32) {
        let Some(frame) = self.pending.get(&timestamp) else {
            return;
        };
        let Some(end) = frame.end else {
            return;
        };
        let Some((&first, first_payload)) = frame.parts.first_key_value() else {
            return;
        };
        if is_fragment(first_payload) && first_payload[1] != 0x80 {
            return;
        }
        let indices = frame.parts.keys().copied().collect::<Vec<_>>();
        if indices.last().copied() != Some(end)
            || indices.windows(2).any(|pair| pair[1] != pair[0] + 1)
        {
            return;
        }

        let frame = self.pending.remove(&timestamp).expect("frame was present");
        let mut data = Vec::new();
        for payload in frame.parts.values() {
            if is_fragment(payload) {
                data.extend_from_slice(&payload[2..]);
            } else {
                data.extend_from_slice(payload);
            }
        }
        if data.is_empty() {
            return;
        }

        let mut key = frame.key;
        if self.group {
            let units = nal_units(&data);
            key = units.iter().any(|unit| matches!(unit[0] & 0x1f, 5 | 7));
            let kept = units
                .iter()
                .filter(|unit| !matches!(unit[0] & 0x1f, 14 | 15 | 20))
                .cloned()
                .collect::<Vec<_>>();
            if kept.is_empty() {
                return;
            }
            for unit in &kept {
                match unit[0] & 0x1f {
                    7 => self.sps = Some(unit.clone()),
                    8 => self.pps = Some(unit.clone()),
                    _ => {}
                }
            }
            if kept.len() != units.len() {
                data = annex_b_from_units(&kept);
            }
            if key {
                let has_sps = kept.iter().any(|unit| unit[0] & 0x1f == 7);
                let has_pps = kept.iter().any(|unit| unit[0] & 0x1f == 8);
                if (!has_sps && self.sps.is_some()) || (!has_pps && self.pps.is_some()) {
                    let mut prefixed = Vec::with_capacity(data.len() + 256);
                    if !has_sps {
                        prefixed.extend_from_slice(START_CODE);
                        prefixed.extend_from_slice(self.sps.as_deref().unwrap_or_default());
                    }
                    if !has_pps {
                        prefixed.extend_from_slice(START_CODE);
                        prefixed.extend_from_slice(self.pps.as_deref().unwrap_or_default());
                    }
                    prefixed.extend_from_slice(&data);
                    data = prefixed;
                }
            }
        }

        self.ready.insert(
            first,
            ReadyFrame {
                key,
                data,
                timestamp,
                last: end,
                since: Instant::now(),
            },
        );
    }

    fn flush(&mut self) -> Vec<VideoFrame> {
        let mut output = Vec::new();
        while let Some((&first_idx, oldest)) = self.ready.first_key_value() {
            if let Some(last) = self.last_idx {
                if first_idx == last + 1 {
                    let frame = self.ready.remove(&first_idx).expect("ready frame exists");
                    self.emit(frame, &mut output);
                    continue;
                }
                if self.ready.len() <= REORDER_FRAMES && oldest.since.elapsed() < REORDER_WINDOW {
                    break;
                }
            }
            let frame = self.ready.remove(&first_idx).expect("ready frame exists");
            if !self.started && !frame.key {
                self.last_idx = Some(frame.last);
                self.keyframe_requested = true;
                continue;
            }
            if self.last_idx.is_some() {
                self.keyframe_requested = true;
                if self.group {
                    self.need_key = true;
                }
            }
            self.emit(frame, &mut output);
        }
        output
    }

    fn emit(&mut self, frame: ReadyFrame, output: &mut Vec<VideoFrame>) {
        self.last_idx = Some(frame.last);
        if self.group && self.need_key && !frame.key {
            return;
        }
        if frame.key {
            self.need_key = false;
        }
        self.started |= frame.key;
        let codec = avc_codec_string(&frame.data).unwrap_or_else(|| "avc1.64001e".into());
        output.push(VideoFrame {
            key: frame.key,
            codec,
            data: frame.data,
            src: None,
            timestamp: frame.timestamp,
        });
        let last = self.last_idx.unwrap_or_default();
        self.pending.retain(|_, pending| {
            pending
                .parts
                .keys()
                .next_back()
                .is_some_and(|idx| *idx > last)
        });
    }
}

fn is_fragment(payload: &[u8]) -> bool {
    // The SFU's two-byte FU marker uses NAL type 28 in the indicator;
    // F/NRI occupy the high three bits and are not fixed to zero (0x1c).
    payload.len() >= 2 && payload[0] & 0x1f == 28 && (payload[1] & 0x3f) == 0
}

fn nal_units(annex_b: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut offset = 0usize;
    let mut current: Option<usize> = None;
    while offset + 2 < annex_b.len() {
        let prefix = if annex_b[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if annex_b[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        if let Some(start) = current {
            let mut end = offset;
            while end > start && annex_b[end - 1] == 0 {
                end -= 1;
            }
            if start < end {
                units.push(annex_b[start..end].to_vec());
            }
        }
        current = Some(offset + prefix);
        offset += prefix;
    }
    if let Some(start) = current {
        let mut end = annex_b.len();
        while end > start && annex_b[end - 1] == 0 {
            end -= 1;
        }
        if start < end {
            units.push(annex_b[start..end].to_vec());
        }
    }
    units
}

fn annex_b_from_units(units: &[Vec<u8>]) -> Vec<u8> {
    let capacity = units.iter().map(|unit| unit.len() + 4).sum();
    let mut data = Vec::with_capacity(capacity);
    for unit in units {
        data.extend_from_slice(START_CODE);
        data.extend_from_slice(unit);
    }
    data
}

pub fn avc_codec_string(annex_b: &[u8]) -> Option<String> {
    for unit in nal_units(annex_b) {
        if unit.len() >= 4 && unit[0] & 0x1f == 7 {
            return Some(format!(
                "avc1.{:02x}{:02x}{:02x}",
                unit[1], unit[2], unit[3]
            ));
        }
    }
    None
}

pub fn packetize_frame(annex_b: &[u8], max_chunk: usize) -> Vec<Vec<u8>> {
    let max_chunk = max_chunk.max(1);
    if annex_b.len() <= max_chunk {
        return vec![annex_b.to_vec()];
    }
    let chunks: Vec<&[u8]> = annex_b.chunks(max_chunk).collect();
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let flag = if index == 0 {
                0x80
            } else if index + 1 == chunks.len() {
                0x40
            } else {
                0x00
            };
            let mut packet = Vec::with_capacity(chunk.len() + 2);
            packet.extend_from_slice(&[0x1c, flag]);
            packet.extend_from_slice(chunk);
            packet
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b_key() -> Vec<u8> {
        vec![
            0, 0, 0, 1, 0x67, 0x64, 0, 0x28, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            1, 2, 3,
        ]
    }

    #[test]
    fn assembles_complete_key_and_delta_frames() {
        let mut assembler = VideoAssembler::new();
        let key = annex_b_key();
        let frames = assembler.push_packet(1, 0, 90, &key, true, VIDEO_PT_KEY);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
        assert_eq!(frames[0].codec, "avc1.640028");
        assert_eq!(frames[0].data, key);
        let delta = [0, 0, 0, 1, 0x41, 9, 8, 7];
        let frames = assembler.push_packet(2, 0, 180, &delta, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert!(!frames[0].key);
        assert_eq!(frames[0].data, delta);
    }

    #[test]
    fn packetizes_and_reassembles_large_annex_b_frame() {
        let data: Vec<u8> = (0..2500).map(|n| (n % 251) as u8).collect();
        let packets = packetize_frame(&data, 1068);
        assert_eq!(packets.len(), 3);
        assert_eq!(&packets[0][..2], &[0x1c, 0x80]);
        assert_eq!(&packets[1][..2], &[0x1c, 0]);
        assert_eq!(&packets[2][..2], &[0x1c, 0x40]);
        let mut assembler = VideoAssembler::new();
        assert!(assembler
            .push_packet(1, 0, 90, &packets[0], false, VIDEO_PT_KEY)
            .is_empty());
        assert!(assembler
            .push_packet(2, 0, 90, &packets[1], false, VIDEO_PT_INTER)
            .is_empty());
        let frames = assembler.push_packet(3, 0, 90, &packets[2], true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
        assert_eq!(frames[0].data, data);
    }

    #[test]
    fn group_fragments_accept_fu_indicators_with_nri_bits() {
        for indicator in [0x1c, 0x5c, 0x7c] {
            let mut assembler = VideoAssembler::with_group(true);
            let first = [indicator, 0x80, 0, 0, 0, 1, 0x65, 1];
            let middle = [indicator, 0x00, 2, 3];
            let last = [indicator, 0x40, 4, 5];
            assert!(assembler
                .push_packet(10, 0, 900, &first, false, VIDEO_PT_INTER)
                .is_empty());
            assert!(assembler
                .push_packet(11, 0, 900, &middle, false, VIDEO_PT_INTER)
                .is_empty());
            let frames = assembler.push_packet(12, 0, 900, &last, true, VIDEO_PT_INTER);
            assert_eq!(frames.len(), 1, "indicator={indicator:#04x}");
            assert!(frames[0].key, "indicator={indicator:#04x}");
            assert_eq!(frames[0].data, [0, 0, 0, 1, 0x65, 1, 2, 3, 4, 5]);
        }
    }

    #[test]
    fn completes_a_frame_when_its_middle_packet_arrives_late() {
        let mut assembler = VideoAssembler::new();
        let first = [0x1c, 0x80, 1, 2];
        let middle = [0x1c, 0x00, 3, 4];
        let last = [0x1c, 0x40, 5, 6];
        assert!(assembler
            .push_packet(100, 0, 900, &first, false, VIDEO_PT_KEY)
            .is_empty());
        assert!(assembler
            .push_packet(102, 0, 900, &last, true, VIDEO_PT_INTER)
            .is_empty());
        let frames = assembler.push_packet(101, 0, 900, &middle, false, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn reorders_complete_frames_by_extended_sequence_number() {
        let mut assembler = VideoAssembler::new();
        assert_eq!(
            assembler
                .push_packet(10, 0, 100, &annex_b_key(), true, VIDEO_PT_KEY)
                .len(),
            1
        );
        let second = [0, 0, 0, 1, 0x41, 2];
        let third = [0, 0, 0, 1, 0x41, 3];
        assert!(assembler
            .push_packet(12, 0, 300, &third, true, VIDEO_PT_INTER)
            .is_empty());
        let frames = assembler.push_packet(11, 0, 200, &second, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].timestamp, 200);
        assert_eq!(frames[1].timestamp, 300);
    }

    #[test]
    fn ignores_duplicate_and_late_packets_after_a_frame_was_emitted() {
        let mut assembler = VideoAssembler::new();
        let key = annex_b_key();
        assert_eq!(
            assembler
                .push_packet(10, 0, 100, &key, true, VIDEO_PT_KEY)
                .len(),
            1
        );
        assert!(assembler
            .push_packet(10, 0, 100, &key, true, VIDEO_PT_KEY)
            .is_empty());
        assert!(assembler
            .push_packet(9, 0, 90, &key, true, VIDEO_PT_KEY)
            .is_empty());
    }

    #[test]
    fn handles_sequence_rollover_using_the_srtp_roc() {
        let mut assembler = VideoAssembler::new();
        assert_eq!(
            assembler
                .push_packet(u16::MAX, 0, 100, &annex_b_key(), true, VIDEO_PT_KEY)
                .len(),
            1
        );
        let delta = [0, 0, 0, 1, 0x41, 9];
        assert_eq!(
            assembler
                .push_packet(0, 1, 200, &delta, true, VIDEO_PT_INTER)
                .len(),
            1
        );
    }

    #[test]
    fn group_assembler_drops_svc_nals_and_caches_sps_pps() {
        let mut assembler = VideoAssembler::with_group(true);
        let mut key = annex_b_key();
        key.extend_from_slice(&[0, 0, 0, 1, 0x6e, 1, 2]);
        let frames = assembler.push_packet(1, 0, 100, &key, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
        assert_eq!(frames[0].codec, "avc1.640028");
        assert!(!frames[0]
            .data
            .windows(5)
            .any(|window| window == [0, 0, 0, 1, 0x6e]));
        let idr_only = [0, 0, 0, 1, 0x65, 4, 5, 6];
        let frames = assembler.push_packet(2, 0, 200, &idr_only, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
        assert!(frames[0].data.starts_with(&annex_b_key()[..16]));
    }

    #[test]
    fn first_delta_is_dropped_and_requests_a_keyframe() {
        let mut assembler = VideoAssembler::new();
        let delta = [0, 0, 0, 1, 0x41, 9];
        assert!(assembler
            .push_packet(1, 0, 100, &delta, true, VIDEO_PT_INTER)
            .is_empty());
        assert!(assembler.take_keyframe_request());
        assert!(!assembler.take_keyframe_request());
        let frames = assembler.push_packet(2, 0, 200, &annex_b_key(), true, VIDEO_PT_KEY);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
    }

    #[test]
    fn group_gap_drops_deltas_until_the_next_keyframe() {
        let mut assembler = VideoAssembler::with_group(true);
        let key = annex_b_key();
        assert_eq!(
            assembler
                .push_packet(1, 0, 100, &key, true, VIDEO_PT_INTER)
                .len(),
            1
        );
        let delta = [0, 0, 0, 1, 0x41, 9];
        assert!(assembler
            .push_packet(4, 0, 400, &delta, true, VIDEO_PT_INTER)
            .is_empty());
        std::thread::sleep(REORDER_WINDOW + Duration::from_millis(10));
        assert!(assembler
            .push_packet(5, 0, 500, &delta, true, VIDEO_PT_INTER)
            .is_empty());
        assert!(assembler.take_keyframe_request());
        let frames = assembler.push_packet(6, 0, 600, &key, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].key);
    }
}
