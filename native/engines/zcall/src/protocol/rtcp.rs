//! RTCP receiver feedback for the Zalo 1:1 media path.
//!
//! Supports RFC 3550 SR/RR/SDES, RFC 4585 PLI/NACK and the transport-wide
//! congestion-control feedback format used by ZaloCall. RTCP is plain on the
//! Zalo relay (srtcp=0), and the same compound packets are used on direct P2P.
use std::collections::{BTreeMap, HashMap, VecDeque};

pub const TWCC_EXT_ID: u8 = 5;
pub const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
const MAX_TWCC_PACKETS: u64 = 400;
const NACK_MAX_HOLE: u16 = 64;
const NACK_RETRY_MS: u64 = 40;
const NACK_MAX_TRIES: u8 = 5;
const NACK_DROP_MS: u64 = 400;
const NACK_KEEP: usize = 128;

pub fn read_tw_seq(rtp: &[u8]) -> Option<u16> {
    if rtp.len() < 16 || (rtp[0] & 0x10) == 0 {
        return None;
    }
    let cc = usize::from(rtp[0] & 0x0f);
    let mut offset = 12usize.checked_add(4usize.checked_mul(cc)?)?;
    if offset.checked_add(4)? > rtp.len()
        || u16::from_be_bytes([rtp[offset], rtp[offset + 1]]) != 0xbede
    {
        return None;
    }
    let words = usize::from(u16::from_be_bytes([rtp[offset + 2], rtp[offset + 3]]));
    let end = rtp
        .len()
        .min(offset.checked_add(4)?.checked_add(words.checked_mul(4)?)?);
    offset += 4;
    while offset < end {
        let header = rtp[offset];
        if header == 0 {
            offset += 1;
            continue;
        }
        let id = header >> 4;
        let len = usize::from((header & 0x0f) + 1);
        if id == 15 || offset.checked_add(1 + len)? > end {
            return None;
        }
        if id == TWCC_EXT_ID && len == 2 {
            return Some(u16::from_be_bytes([rtp[offset + 1], rtp[offset + 2]]));
        }
        offset += 1 + len;
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub struct TwccFeedback {
    pub sender_ssrc: u32,
    pub media_ssrc: u32,
    pub base: u16,
    pub count: u16,
    pub reference_time: u32,
    pub feedback_count: u8,
    pub status: Vec<u8>,
    /// Arrival deltas in units of 250 microseconds. Missing packets have no delta.
    pub delta_q: Vec<i16>,
}

#[derive(Default)]
pub struct TwccRecorder {
    arrivals: HashMap<u64, i64>,
    top: Option<u64>,
    next: Option<u64>,
    feedback_count: u8,
    pub sent: u64,
}

impl TwccRecorder {
    fn unwrap(&self, seq: u16) -> u64 {
        let Some(top) = self.top else {
            return u64::from(seq);
        };
        let mut unwrapped = (top & !0xffff) | u64::from(seq);
        if unwrapped > top && unwrapped - top > 0x8000 {
            unwrapped = unwrapped.saturating_sub(0x1_0000);
        } else if top > unwrapped && top - unwrapped > 0x8000 {
            unwrapped = unwrapped.saturating_add(0x1_0000);
        }
        unwrapped
    }

    /// now_ms is a monotonic millisecond clock, not wall-clock time.
    pub fn add(&mut self, seq: u16, now_ms: u64) {
        let unwrapped = self.unwrap(seq);
        if self.next.is_none() {
            self.next = Some(unwrapped);
        }
        let Some(next) = self.next else {
            return;
        };
        if unwrapped < next || self.arrivals.contains_key(&unwrapped) {
            return;
        }
        let arrival_q = (now_ms.min(i64::MAX as u64) as i64).saturating_mul(4);
        self.arrivals.insert(unwrapped, arrival_q);
        self.top = Some(self.top.map_or(unwrapped, |top| top.max(unwrapped)));
    }

    pub fn is_pending(&self) -> bool {
        matches!((self.next, self.top), (Some(next), Some(top)) if top >= next)
    }

    pub fn build(&mut self, sender_ssrc: u32, media_ssrc: u32) -> Vec<Vec<u8>> {
        let mut output = Vec::new();
        while let (Some(base), Some(top)) = (self.next, self.top) {
            if top < base {
                break;
            }
            let end = top.min(base.saturating_add(MAX_TWCC_PACKETS - 1));
            let any = (base..=end).any(|seq| self.arrivals.contains_key(&seq));
            if !any {
                self.next = Some(end.saturating_add(1));
                continue;
            }
            let feedback_count = self.feedback_count;
            self.feedback_count = self.feedback_count.wrapping_add(1);
            output.push(encode_twcc(
                sender_ssrc,
                media_ssrc,
                base,
                end,
                &self.arrivals,
                feedback_count,
            ));
            for seq in base..=end {
                self.arrivals.remove(&seq);
            }
            self.next = Some(end.saturating_add(1));
            self.sent = self.sent.saturating_add(1);
        }
        output
    }
}

fn encode_twcc(
    sender_ssrc: u32,
    media_ssrc: u32,
    base: u64,
    end: u64,
    arrivals: &HashMap<u64, i64>,
    feedback_count: u8,
) -> Vec<u8> {
    let count = (end - base + 1) as usize;
    let first_arrival = (base..=end)
        .find_map(|seq| arrivals.get(&seq).copied())
        .unwrap_or_default();
    let reference_units = first_arrival.div_euclid(256);
    let reference_q = reference_units.saturating_mul(256);
    let mut previous_q = reference_q;
    let mut symbols = Vec::with_capacity(count);
    let mut deltas = Vec::with_capacity(count);

    for seq in base..=end {
        let Some(arrival_q) = arrivals.get(&seq).copied() else {
            symbols.push(0u8);
            continue;
        };
        let delta = arrival_q.saturating_sub(previous_q);
        previous_q = arrival_q;
        if (0..=255).contains(&delta) {
            symbols.push(1);
            deltas.push(delta as i16);
        } else {
            symbols.push(2);
            deltas.push(delta.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16);
        }
    }

    let mut chunks = Vec::<u16>::new();
    let mut index = 0usize;
    while index < symbols.len() {
        let mut run = 1usize;
        while index + run < symbols.len() && symbols[index + run] == symbols[index] && run < 0x1fff
        {
            run += 1;
        }
        if run >= 8 {
            chunks.push((u16::from(symbols[index]) << 13) | run as u16);
            index += run;
            continue;
        }
        let mut chunk = 0xc000u16;
        for slot in 0..7 {
            if let Some(symbol) = symbols.get(index + slot) {
                chunk |= u16::from(*symbol) << (12 - slot * 2);
            }
        }
        chunks.push(chunk);
        index += 7;
    }

    let delta_bytes = symbols
        .iter()
        .map(|symbol| match symbol {
            1 => 1usize,
            2 => 2usize,
            _ => 0usize,
        })
        .sum::<usize>();
    let raw_len = 20 + chunks.len() * 2 + delta_bytes;
    let padded_len = (raw_len + 3) & !3;
    let mut packet = vec![0u8; padded_len];
    packet[0] = 0x8f;
    packet[1] = 205;
    packet[2..4].copy_from_slice(&((padded_len / 4 - 1) as u16).to_be_bytes());
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet[12..14].copy_from_slice(&(base as u16).to_be_bytes());
    packet[14..16].copy_from_slice(&(count as u16).to_be_bytes());
    let ref24 = (reference_units as u64 & 0x00ff_ffff) as u32;
    packet[16] = (ref24 >> 16) as u8;
    packet[17] = (ref24 >> 8) as u8;
    packet[18] = ref24 as u8;
    packet[19] = feedback_count;

    let mut offset = 20usize;
    for chunk in chunks {
        packet[offset..offset + 2].copy_from_slice(&chunk.to_be_bytes());
        offset += 2;
    }
    let mut delta_index = 0usize;
    for symbol in symbols {
        match symbol {
            1 => {
                packet[offset] = deltas[delta_index].clamp(0, 255) as u8;
                offset += 1;
                delta_index += 1;
            }
            2 => {
                packet[offset..offset + 2].copy_from_slice(&deltas[delta_index].to_be_bytes());
                offset += 2;
                delta_index += 1;
            }
            _ => {}
        }
    }
    packet
}

/// Decodes one TWCC RTPFB/FMT=15 packet. Invalid or truncated chunks are rejected.
pub fn parse_twcc(packet: &[u8]) -> Option<TwccFeedback> {
    if packet.len() < 20 || packet[0] >> 6 != 2 || packet[0] & 0x1f != 15 || packet[1] != 205 {
        return None;
    }
    let declared_len = (usize::from(u16::from_be_bytes([packet[2], packet[3]])) + 1) * 4;
    if declared_len > packet.len() || declared_len < 20 {
        return None;
    }
    let count = u16::from_be_bytes([packet[14], packet[15]]);
    let mut result = TwccFeedback {
        sender_ssrc: u32::from_be_bytes(packet[4..8].try_into().ok()?),
        media_ssrc: u32::from_be_bytes(packet[8..12].try_into().ok()?),
        base: u16::from_be_bytes([packet[12], packet[13]]),
        count,
        reference_time: (u32::from(packet[16]) << 16)
            | (u32::from(packet[17]) << 8)
            | u32::from(packet[18]),
        feedback_count: packet[19],
        status: Vec::with_capacity(usize::from(count)),
        delta_q: Vec::new(),
    };
    let mut offset = 20usize;
    while result.status.len() < usize::from(count) {
        if offset + 2 > declared_len {
            return None;
        }
        let chunk = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        offset += 2;
        if chunk & 0x8000 == 0 {
            let symbol = ((chunk >> 13) & 0x03) as u8;
            let run = usize::from(chunk & 0x1fff);
            if run == 0 {
                return None;
            }
            let remaining = usize::from(count) - result.status.len();
            result
                .status
                .extend(std::iter::repeat_n(symbol, run.min(remaining)));
        } else if chunk & 0x4000 != 0 {
            for slot in 0..7 {
                if result.status.len() >= usize::from(count) {
                    break;
                }
                result
                    .status
                    .push(((chunk >> (12 - 2 * slot)) & 0x03) as u8);
            }
        } else {
            for slot in 0..14 {
                if result.status.len() >= usize::from(count) {
                    break;
                }
                result.status.push(((chunk >> (13 - slot)) & 0x01) as u8);
            }
        }
    }
    for status in &result.status {
        match status {
            0 => {}
            1 => {
                if offset >= declared_len {
                    return None;
                }
                result.delta_q.push(i16::from(packet[offset]));
                offset += 1;
            }
            2 => {
                if offset + 2 > declared_len {
                    return None;
                }
                result
                    .delta_q
                    .push(i16::from_be_bytes([packet[offset], packet[offset + 1]]));
                offset += 2;
            }
            _ => return None,
        }
    }
    Some(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NackFeedback {
    pub sender_ssrc: u32,
    pub media_ssrc: u32,
    pub lost_sequences: Vec<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct RtcpEvents {
    pub keyframe_targets: Vec<u32>,
    pub nack_requests: Vec<NackFeedback>,
    pub twcc_feedback: Vec<TwccFeedback>,
    pub report_blocks: Vec<ReportBlock>,
}

pub fn parse_nack(packet: &[u8]) -> Option<NackFeedback> {
    if packet.len() < 16 || packet[0] >> 6 != 2 || packet[0] & 0x1f != 1 || packet[1] != 205 {
        return None;
    }
    let declared = (usize::from(u16::from_be_bytes([packet[2], packet[3]])) + 1) * 4;
    if declared > packet.len() || declared < 16 || (declared - 12) % 4 != 0 {
        return None;
    }
    let mut lost = Vec::new();
    let mut offset = 12usize;
    while offset + 4 <= declared {
        let pid = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let blp = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
        lost.push(pid);
        for bit in 0..16 {
            if blp & (1 << bit) != 0 {
                lost.push(pid.wrapping_add(bit + 1));
            }
        }
        offset += 4;
    }
    lost.sort_unstable();
    lost.dedup();
    Some(NackFeedback {
        sender_ssrc: u32::from_be_bytes(packet[4..8].try_into().ok()?),
        media_ssrc: u32::from_be_bytes(packet[8..12].try_into().ok()?),
        lost_sequences: lost,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportBlock {
    pub ssrc: u32,
    pub fraction_lost: u8,
    pub cumulative_lost: i32,
    pub extended_highest_seq: u32,
    pub jitter: u32,
    pub lsr: u32,
    pub dlsr: u32,
}

fn parse_report_block(packet: &[u8]) -> Option<ReportBlock> {
    if packet.len() < 24 {
        return None;
    }
    let lost_raw =
        (u32::from(packet[5]) << 16) | (u32::from(packet[6]) << 8) | u32::from(packet[7]);
    let cumulative_lost = if lost_raw & 0x80_0000 != 0 {
        (lost_raw | 0xff00_0000) as i32
    } else {
        lost_raw as i32
    };
    Some(ReportBlock {
        ssrc: u32::from_be_bytes(packet[0..4].try_into().ok()?),
        fraction_lost: packet[4],
        cumulative_lost,
        extended_highest_seq: u32::from_be_bytes(packet[8..12].try_into().ok()?),
        jitter: u32::from_be_bytes(packet[12..16].try_into().ok()?),
        lsr: u32::from_be_bytes(packet[16..20].try_into().ok()?),
        dlsr: u32::from_be_bytes(packet[20..24].try_into().ok()?),
    })
}

fn write_report_block(packet: &mut [u8], offset: usize, block: ReportBlock) {
    packet[offset..offset + 4].copy_from_slice(&block.ssrc.to_be_bytes());
    packet[offset + 4] = block.fraction_lost;
    let lost = block.cumulative_lost.clamp(-0x80_0000, 0x7f_ffff) as u32 & 0x00ff_ffff;
    packet[offset + 5] = (lost >> 16) as u8;
    packet[offset + 6] = (lost >> 8) as u8;
    packet[offset + 7] = lost as u8;
    packet[offset + 8..offset + 12].copy_from_slice(&block.extended_highest_seq.to_be_bytes());
    packet[offset + 12..offset + 16].copy_from_slice(&block.jitter.to_be_bytes());
    packet[offset + 16..offset + 20].copy_from_slice(&block.lsr.to_be_bytes());
    packet[offset + 20..offset + 24].copy_from_slice(&block.dlsr.to_be_bytes());
}

pub fn ntp_now(now_ms: u64) -> (u32, u32) {
    let seconds = now_ms / 1000;
    let remainder = now_ms % 1000;
    let msw = seconds.wrapping_add(NTP_EPOCH_OFFSET) as u32;
    let lsw = ((remainder as u128 * (1u128 << 32)) / 1000) as u32;
    (msw, lsw)
}

pub fn build_sr(
    ssrc: u32,
    now_ms: u64,
    rtp_ts: u32,
    packets: u32,
    octets: u32,
    blocks: &[ReportBlock],
) -> Vec<u8> {
    let block_count = blocks.len().min(31);
    let mut packet = vec![0u8; 28 + block_count * 24];
    packet[0] = 0x80 | block_count as u8;
    packet[1] = 200;
    let length = (packet.len() / 4 - 1) as u16;
    packet[2..4].copy_from_slice(&length.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    let (msw, lsw) = ntp_now(now_ms);
    packet[8..12].copy_from_slice(&msw.to_be_bytes());
    packet[12..16].copy_from_slice(&lsw.to_be_bytes());
    packet[16..20].copy_from_slice(&rtp_ts.to_be_bytes());
    packet[20..24].copy_from_slice(&packets.to_be_bytes());
    packet[24..28].copy_from_slice(&octets.to_be_bytes());
    for (index, block) in blocks.iter().take(block_count).enumerate() {
        write_report_block(&mut packet, 28 + index * 24, *block);
    }
    packet
}

pub fn build_rr(ssrc: u32, blocks: &[ReportBlock]) -> Vec<u8> {
    let block_count = blocks.len().min(31);
    let mut packet = vec![0u8; 8 + block_count * 24];
    packet[0] = 0x80 | block_count as u8;
    packet[1] = 201;
    let length = (packet.len() / 4 - 1) as u16;
    packet[2..4].copy_from_slice(&length.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    for (index, block) in blocks.iter().take(block_count).enumerate() {
        write_report_block(&mut packet, 8 + index * 24, *block);
    }
    packet
}

pub fn build_sdes(ssrc: u32, cname: &str) -> Vec<u8> {
    let cname_bytes = &cname.as_bytes()[..cname.len().min(255)];
    let item_len = 2 + cname_bytes.len() + 1;
    let packet_len = (4 + 4 + item_len + 3) & !3;
    let mut packet = vec![0u8; packet_len];
    packet[0] = 0x81;
    packet[1] = 202;
    let length = (packet.len() / 4 - 1) as u16;
    packet[2..4].copy_from_slice(&length.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    packet[8] = 1;
    packet[9] = cname_bytes.len() as u8;
    packet[10..10 + cname_bytes.len()].copy_from_slice(cname_bytes);
    packet
}

pub fn build_pli(sender_ssrc: u32, media_ssrc: u32) -> [u8; 12] {
    let mut packet = [0u8; 12];
    packet[0] = 0x81;
    packet[1] = 206;
    packet[2..4].copy_from_slice(&2u16.to_be_bytes());
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet
}

pub fn build_nack(sender_ssrc: u32, media_ssrc: u32, lost_seq: u16) -> [u8; 16] {
    let mut packet = [0u8; 16];
    packet[0] = 0x81;
    packet[1] = 205;
    packet[2..4].copy_from_slice(&3u16.to_be_bytes());
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet[12..14].copy_from_slice(&lost_seq.to_be_bytes());
    packet
}

pub fn build_nack_many(sender_ssrc: u32, media_ssrc: u32, seqs: &[u16]) -> Vec<u8> {
    let mut items = Vec::<(u16, u16)>::new();
    let mut index = 0usize;
    while index < seqs.len() {
        let pid = seqs[index];
        let mut blp = 0u16;
        let mut next = index + 1;
        while next < seqs.len() {
            let distance = seqs[next].wrapping_sub(pid);
            if !(1..=16).contains(&distance) {
                break;
            }
            blp |= 1 << (distance - 1);
            next += 1;
        }
        items.push((pid, blp));
        index = next;
    }
    let mut packet = vec![0u8; 12 + items.len() * 4];
    packet[0] = 0x81;
    packet[1] = 205;
    let length = (packet.len() / 4 - 1) as u16;
    packet[2..4].copy_from_slice(&length.to_be_bytes());
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    for (index, (pid, blp)) in items.into_iter().enumerate() {
        let offset = 12 + index * 4;
        packet[offset..offset + 2].copy_from_slice(&pid.to_be_bytes());
        packet[offset + 2..offset + 4].copy_from_slice(&blp.to_be_bytes());
    }
    packet
}

#[derive(Default)]
pub struct NackTracker {
    streams: HashMap<u32, NackStream>,
}

#[derive(Default)]
struct NackStream {
    highest: Option<u16>,
    missing: BTreeMap<u16, MissingPacket>,
}

#[derive(Clone, Copy)]
struct MissingPacket {
    first_seen_ms: u64,
    last_sent_ms: Option<u64>,
    attempts: u8,
}

impl NackTracker {
    pub fn note(&mut self, ssrc: u32, seq: u16, now_ms: u64) -> Vec<(u32, Vec<u16>)> {
        let stream = self.streams.entry(ssrc).or_default();
        let Some(highest) = stream.highest else {
            stream.highest = Some(seq);
            return Vec::new();
        };
        let ahead = seq.wrapping_sub(highest);
        if ahead == 0 {
            return Self::due(stream, ssrc, now_ms);
        }
        if ahead >= 0x8000 {
            stream.missing.remove(&seq);
            return Self::due(stream, ssrc, now_ms);
        }
        if ahead > NACK_MAX_HOLE {
            stream.highest = Some(seq);
            stream.missing.clear();
            return Vec::new();
        }
        for distance in 1..ahead {
            stream
                .missing
                .entry(highest.wrapping_add(distance))
                .or_insert(MissingPacket {
                    first_seen_ms: now_ms,
                    last_sent_ms: None,
                    attempts: 0,
                });
        }
        stream.highest = Some(seq);
        stream.missing.remove(&seq);
        while stream.missing.len() > NACK_KEEP {
            if let Some(oldest) = stream.missing.keys().next().copied() {
                stream.missing.remove(&oldest);
            } else {
                break;
            }
        }
        Self::due(stream, ssrc, now_ms)
    }

    fn due(stream: &mut NackStream, ssrc: u32, now_ms: u64) -> Vec<(u32, Vec<u16>)> {
        let highest = stream.highest.unwrap_or_default();
        let mut seqs = Vec::new();
        stream.missing.retain(|seq, info| {
            if now_ms.saturating_sub(info.first_seen_ms) > NACK_DROP_MS
                || info.attempts >= NACK_MAX_TRIES
            {
                return false;
            }
            let due_at = info.last_sent_ms.map_or(info.first_seen_ms, |last| {
                last.saturating_add(NACK_RETRY_MS)
            });
            if now_ms >= due_at {
                info.attempts += 1;
                info.last_sent_ms = Some(now_ms);
                seqs.push(*seq);
            }
            true
        });
        if seqs.is_empty() {
            return Vec::new();
        }
        seqs.sort_by_key(|seq| std::cmp::Reverse(highest.wrapping_sub(*seq)));
        vec![(ssrc, seqs)]
    }

    pub fn poll(&mut self, now_ms: u64) -> Vec<(u32, Vec<u16>)> {
        let mut due = Vec::new();
        for (ssrc, stream) in &mut self.streams {
            due.extend(Self::due(stream, *ssrc, now_ms));
        }
        due
    }

    pub fn clear(&mut self) {
        self.streams.clear();
    }
}

#[derive(Default)]
pub struct RxStats {
    clock_hz: u32,
    ssrc: u32,
    started: bool,
    base_seq: u16,
    max_seq: u16,
    cycles: u32,
    received: u64,
    expected_prior: u64,
    received_prior: u64,
    downlink_expected_prior: u64,
    downlink_received_prior: u64,
    jitter: f64,
    transit: Option<f64>,
    seen: VecDeque<u32>,
    seen_set: HashMap<u32, ()>,
    lsr: u32,
    lsr_at_ms: u64,
}

impl RxStats {
    pub fn new(clock_hz: u32) -> Self {
        Self {
            clock_hz,
            ..Default::default()
        }
    }

    pub fn on_packet(&mut self, rtp: &[u8], now_ms: u64) {
        if rtp.len() < 12 {
            return;
        }
        let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
        let timestamp = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
        if !self.started {
            self.started = true;
            self.ssrc = u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]);
            self.base_seq = seq;
            self.max_seq = seq;
        } else {
            let ahead = seq.wrapping_sub(self.max_seq);
            if ahead != 0 && ahead < 0x8000 {
                if seq < self.max_seq {
                    self.cycles = self.cycles.wrapping_add(0x1_0000);
                }
                self.max_seq = seq;
            }
        }
        let extended = self.cycles.wrapping_add(u32::from(seq));
        if self.seen_set.contains_key(&extended) {
            return;
        }
        self.seen.push_back(extended);
        self.seen_set.insert(extended, ());
        if self.seen.len() > 512 {
            if let Some(oldest) = self.seen.pop_front() {
                self.seen_set.remove(&oldest);
            }
        }
        self.received += 1;
        let arrival = (now_ms as f64) * (f64::from(self.clock_hz) / 1000.0);
        let transit = arrival - f64::from(timestamp);
        if let Some(previous) = self.transit {
            self.jitter += ((transit - previous).abs() - self.jitter) / 16.0;
        }
        self.transit = Some(transit);
    }

    pub fn on_sender_report(&mut self, packet: &[u8], now_ms: u64) {
        if packet.len() < 28 {
            return;
        }
        self.lsr = ((u32::from_be_bytes(packet[8..12].try_into().unwrap_or_default()) & 0xffff)
            << 16)
            | (u32::from_be_bytes(packet[12..16].try_into().unwrap_or_default()) >> 16);
        self.lsr_at_ms = now_ms;
    }

    pub fn block(&mut self, now_ms: u64) -> Option<ReportBlock> {
        if !self.started {
            return None;
        }
        let extended_highest = self.cycles.wrapping_add(u32::from(self.max_seq));
        let expected = u64::from(extended_highest.saturating_sub(u32::from(self.base_seq))) + 1;
        let lost = expected as i64 - self.received as i64;
        let expected_interval = expected.saturating_sub(self.expected_prior);
        let received_interval = self.received.saturating_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = expected_interval as i64 - received_interval as i64;
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval * 256) / expected_interval as i64).clamp(0, 255) as u8
        };
        let dlsr = if self.lsr_at_ms > 0 {
            (now_ms.saturating_sub(self.lsr_at_ms) as u128 * 65_536 / 1000) as u32
        } else {
            0
        };
        Some(ReportBlock {
            ssrc: self.ssrc,
            fraction_lost,
            cumulative_lost: lost.clamp(-0x80_0000, 0x7f_ffff) as i32,
            extended_highest_seq: extended_highest,
            jitter: self.jitter.round().clamp(0.0, u32::MAX as f64) as u32,
            lsr: self.lsr,
            dlsr,
        })
    }

    pub fn interval_loss(&mut self) -> Option<f64> {
        if !self.started {
            return None;
        }
        let extended_highest = self.cycles.wrapping_add(u32::from(self.max_seq));
        let expected = u64::from(extended_highest.saturating_sub(u32::from(self.base_seq))) + 1;
        let interval_expected = expected.saturating_sub(self.downlink_expected_prior);
        let interval_received = self.received.saturating_sub(self.downlink_received_prior);
        self.downlink_expected_prior = expected;
        self.downlink_received_prior = self.received;
        if interval_expected == 0 {
            None
        } else {
            Some(
                (interval_expected.saturating_sub(interval_received) as f64)
                    / interval_expected as f64,
            )
        }
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    pub fn ssrc(&self) -> Option<u32> {
        self.started.then_some(self.ssrc)
    }
}

#[derive(Default)]
pub struct RtcpCounters {
    pub twcc_packets: u64,
    pub reports: u64,
    pub pli: u64,
    pub nack: u64,
}

pub struct RtcpFeedback {
    pub local_ssrc: u32,
    pub peer_ssrc: u32,
    pub twcc: TwccRecorder,
    pub audio: RxStats,
    pub video: RxStats,
    pub counters: RtcpCounters,
}

impl RtcpFeedback {
    pub fn new(local_ssrc: u32) -> Self {
        Self {
            local_ssrc,
            peer_ssrc: 0,
            twcc: TwccRecorder::default(),
            audio: RxStats::new(16_000),
            video: RxStats::new(90_000),
            counters: RtcpCounters::default(),
        }
    }

    pub fn on_rtp(&mut self, video: bool, rtp: &[u8], now_ms: u64) {
        if rtp.len() < 12 {
            return;
        }
        if self.peer_ssrc == 0 {
            self.peer_ssrc = u32::from_be_bytes(rtp[8..12].try_into().unwrap_or_default());
        }
        if let Some(seq) = read_tw_seq(rtp) {
            self.twcc.add(seq, now_ms);
        }
        if video {
            self.video.on_packet(rtp, now_ms);
        } else {
            self.audio.on_packet(rtp, now_ms);
        }
    }

    pub fn on_rtcp(&mut self, video: bool, compound: &[u8], now_ms: u64) -> RtcpEvents {
        let mut events = RtcpEvents::default();
        let mut offset = 0usize;
        while offset + 4 <= compound.len() {
            if compound[offset] >> 6 != 2 {
                break;
            }
            let len = (usize::from(u16::from_be_bytes([
                compound[offset + 2],
                compound[offset + 3],
            ])) + 1)
                * 4;
            if len < 4 || offset + len > compound.len() {
                break;
            }
            let packet = &compound[offset..offset + len];
            if packet[1] == 200 {
                if video {
                    self.video.on_sender_report(packet, now_ms);
                } else {
                    self.audio.on_sender_report(packet, now_ms);
                }
            }
            if packet[1] == 200 || packet[1] == 201 {
                let first_block = if packet[1] == 200 { 28 } else { 8 };
                let count = usize::from(packet[0] & 0x1f);
                for index in 0..count {
                    let block_offset = first_block + index * 24;
                    if block_offset + 24 > packet.len() {
                        break;
                    }
                    if let Some(block) =
                        parse_report_block(&packet[block_offset..block_offset + 24])
                    {
                        events.report_blocks.push(block);
                    }
                }
            }
            if packet[1] == 205 {
                let fmt = packet[0] & 0x1f;
                if fmt == 15 {
                    if let Some(feedback) = parse_twcc(packet) {
                        events.twcc_feedback.push(feedback);
                    }
                } else if fmt == 1 {
                    if let Some(feedback) = parse_nack(packet) {
                        events.nack_requests.push(feedback);
                    }
                }
            }
            if packet[1] == 206 {
                let fmt = packet[0] & 0x1f;
                if fmt == 1 && packet.len() >= 12 {
                    events.keyframe_targets.push(u32::from_be_bytes(
                        packet[8..12].try_into().unwrap_or_default(),
                    ));
                } else if fmt == 4 && packet.len() >= 20 {
                    let media = u32::from_be_bytes(packet[8..12].try_into().unwrap_or_default());
                    if media == 0 {
                        events.keyframe_targets.push(u32::from_be_bytes(
                            packet[12..16].try_into().unwrap_or_default(),
                        ));
                    }
                }
            }
            offset += len;
        }
        events
    }

    pub fn twcc_packets(&mut self) -> Vec<Vec<u8>> {
        if self.peer_ssrc == 0 {
            return Vec::new();
        }
        let packets = self.twcc.build(self.local_ssrc, self.peer_ssrc);
        self.counters.twcc_packets += packets.len() as u64;
        packets
    }

    pub fn audio_report(&mut self, now_ms: u64, rtp_ts: u32, packets: u32, octets: u32) -> Vec<u8> {
        let block = self.audio.block(now_ms).into_iter().collect::<Vec<_>>();
        self.counters.reports += 1;
        let sr = build_sr(self.local_ssrc, now_ms, rtp_ts, packets, octets, &block);
        let sdes = build_sdes(self.local_ssrc, "");
        [sr, sdes].concat()
    }

    pub fn video_report(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        let block = self.video.block(now_ms)?;
        self.counters.reports += 1;
        let rr = build_rr(self.local_ssrc, &[block]);
        Some([rr, build_sdes(self.local_ssrc, "")].concat())
    }

    pub fn downlink_audio_loss(&mut self) -> Option<f64> {
        self.audio.interval_loss()
    }

    pub fn pli(&mut self, media_ssrc: u32) -> Vec<u8> {
        self.counters.pli += 1;
        build_pli(self.local_ssrc, media_ssrc).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_transport_wide_sequence_from_rfc8285_extension() {
        let mut rtp = vec![0u8; 20];
        rtp[0] = 0x90;
        rtp[12..14].copy_from_slice(&0xbedeu16.to_be_bytes());
        rtp[14..16].copy_from_slice(&1u16.to_be_bytes());
        rtp[16] = (TWCC_EXT_ID << 4) | 1;
        rtp[17..19].copy_from_slice(&0x1234u16.to_be_bytes());
        assert_eq!(read_tw_seq(&rtp), Some(0x1234));
    }

    #[test]
    fn reports_fractional_and_cumulative_loss() {
        let mut stats = RxStats::new(90_000);
        for (seq, ts) in [(10u16, 0u32), (11, 3000), (13, 9000), (14, 12000)] {
            let mut rtp = vec![0u8; 12];
            rtp[2..4].copy_from_slice(&seq.to_be_bytes());
            rtp[4..8].copy_from_slice(&ts.to_be_bytes());
            rtp[8..12].copy_from_slice(&222u32.to_be_bytes());
            stats.on_packet(&rtp, u64::from(seq) * 33);
        }
        let report = stats.block(1000).unwrap();
        assert_eq!(report.ssrc, 222);
        assert_eq!(report.cumulative_lost, 1);
        assert_eq!(report.fraction_lost, 51);
    }

    #[test]
    fn twcc_build_and_parse_roundtrip_handles_large_and_small_deltas() {
        let mut recorder = TwccRecorder::default();
        for (seq, time) in [(65534u16, 1000u64), (0, 1000), (1, 1200), (2, 900)] {
            recorder.add(seq, time);
        }
        let packets = recorder.build(10, 20);
        assert_eq!(packets.len(), 1);
        let decoded = parse_twcc(&packets[0]).unwrap();
        assert_eq!(decoded.base, 65534);
        assert_eq!(decoded.count, 5);
        assert_eq!(decoded.status, vec![1, 0, 1, 2, 2]);
        assert_eq!(decoded.delta_q, vec![160, 0, 800, -1200]);
    }

    #[test]
    fn nack_tracker_detects_and_retries_bounded_holes() {
        let mut tracker = NackTracker::default();
        assert!(tracker.note(22, 10, 0).is_empty());
        let first = tracker.note(22, 13, 10);
        assert_eq!(first, vec![(22, vec![11, 12])]);
        assert!(tracker.poll(49).is_empty());
        assert_eq!(tracker.poll(50), vec![(22, vec![11, 12])]);
        assert_eq!(tracker.poll(90), vec![(22, vec![11, 12])]);
    }

    #[test]
    fn builds_nack_compacting_next_sixteen_sequences() {
        let packet = build_nack_many(10, 20, &[100, 101, 104, 116, 120]);
        assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), 4);
        assert_eq!(u16::from_be_bytes([packet[12], packet[13]]), 100);
        assert_eq!(u16::from_be_bytes([packet[14], packet[15]]), 0x8009);
        assert_eq!(u16::from_be_bytes([packet[16], packet[17]]), 120);
        assert_eq!(u16::from_be_bytes([packet[18], packet[19]]), 0x0000);
    }

    #[test]
    fn parses_nack_pid_and_bitmask_with_sequence_wrap() {
        let nack = build_nack_many(20, 10, &[65534, 65535, 0, 2]);
        let parsed = parse_nack(&nack).unwrap();
        assert_eq!(parsed.sender_ssrc, 20);
        assert_eq!(parsed.media_ssrc, 10);
        assert_eq!(parsed.lost_sequences, vec![0, 2, 65534, 65535]);
    }

    #[test]
    fn parses_report_blocks_and_twcc_from_compound_feedback() {
        let block = ReportBlock {
            ssrc: 10,
            fraction_lost: 25,
            cumulative_lost: -2,
            extended_highest_seq: 99,
            jitter: 4,
            lsr: 5,
            dlsr: 6,
        };
        let rr = build_rr(20, &[block]);
        let mut feedback = RtcpFeedback::new(10);
        let events = feedback.on_rtcp(true, &rr, 100);
        assert_eq!(events.report_blocks, vec![block]);

        let mut recorder = TwccRecorder::default();
        recorder.add(1, 1000);
        recorder.add(2, 1010);
        let twcc = recorder.build(20, 10).remove(0);
        let events = feedback.on_rtcp(true, &twcc, 100);
        assert_eq!(events.twcc_feedback.len(), 1);
        assert_eq!(events.twcc_feedback[0].count, 2);
    }

    #[test]
    fn parses_pli_request_for_our_ssrc() {
        let mut feedback = RtcpFeedback::new(10);
        let pli = build_pli(20, 10);
        assert_eq!(feedback.on_rtcp(true, &pli, 100).keyframe_targets, vec![10]);
    }
}
