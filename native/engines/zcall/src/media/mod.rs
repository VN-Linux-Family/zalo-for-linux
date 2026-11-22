//! Media transport backend implementing ZRTP over UDP with SRTP and Opus audio.

pub mod rate;

use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::audio::backend::{
    get_source_volume, probe_audio_hardware, pulse_get_source_info, pulse_normalize_own_stream,
    set_source_mute, set_source_volume,
};
use crate::audio::{
    clip_guard::{apply_soft_limiter, ClipGuard},
    compute_rfc6464_level,
    dsp::AudioProcessor,
    group::GroupAudioMixer,
    leveler::MicLeveler,
    Apm, JitterBuffer, OpusEncoder, PulsePlayer, PulseRecorder, FRAME_BYTES, FRAME_SAMPLES,
    SAMPLE_RATE,
};
use crate::crypto::srtp::{
    hmac_sha1, master_from_sess_id, parse_rtp_header_len, protect_rtp, session_keys, sha1,
    unprotect_rtp, unprotect_rtp_with_profile, SrtpKeys,
};
use crate::media::rate::{RateTotals, SendRateController, VideoProfile};
use crate::protocol::group::{
    build_group_init_zrtp_packet, build_group_video_quality_config,
    build_group_video_quality_request, build_share_init_zrtp_packet, build_share_zavi_ping_packet,
    group_video_plan as parse_group_video_plan, parse_room_updated, select_group_video_quality,
    GroupInitLayout, GroupVideoLayer, GroupVideoPlan, GroupVideoQualityRequest,
};
use crate::protocol::rtcp::{build_nack_many, NackTracker, RtcpFeedback};
use crate::protocol::rtp::{
    build_audio_rtp_header, build_group_audio_rtp_header, build_group_video_rtp_header,
    parse_rfc6464_audio_level,
};
use crate::protocol::zrtp::{
    build_change_address_packet, build_end_call_packet, build_init_zrtp_packet, build_ping_packet,
    build_req_forward_packet, build_zavi_ping_packet, msg_types, p2p, parse_server_reply,
    wrap_media_packet,
};
use crate::video::{packetize_frame, VideoAssembler, VideoFrame, VIDEO_PT_INTER, VIDEO_PT_KEY};

#[allow(clippy::too_many_arguments)]
fn spawn_playout_worker(
    running: Arc<AtomicBool>,
    audio_enabled: Arc<AtomicBool>,
    devices: Arc<Mutex<(Option<String>, Option<String>)>>,
    device_generation: Arc<AtomicU64>,
    speaker_off: Arc<AtomicBool>,
    params: Arc<Mutex<Option<MediaSessionParams>>>,
    jitter_buffer: Option<Arc<Mutex<JitterBuffer>>>,
    group_audio: Arc<Mutex<GroupAudioMixer>>,
    audio_dsp: Arc<Mutex<Option<AudioProcessor>>>,
    apm: Option<Arc<Apm>>,
    far_active: Arc<AtomicBool>,
    mic_volume: Arc<AtomicU8>,
    media_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
) {
    let _ = thread::Builder::new()
        .name("zcall-playout".into())
        .spawn(move || {
            let mut speaker_device: Option<String> = None;
            let mut player: Option<PulsePlayer> = None;
            let mut speaker_fault_reported = false;
            let mut active_generation = u64::MAX;
            let mut last_reopen = Instant::now() - Duration::from_secs(1);
            let mut last_far_speech = Instant::now() - Duration::from_secs(1);
            let mut last_diag = Instant::now();
            let mut write_block_ms = 0u64;
            let mut write_count = 0u32;
            let mut max_write_ms = 0u64;
            let mut non_blocking_streak = 0u32;
            let mut pcm_out = [0i16; FRAME_SAMPLES];
            let silence = vec![0u8; FRAME_BYTES];
            let tick = Duration::from_millis(20);

            while running.load(Ordering::Relaxed) {
                if !audio_enabled.load(Ordering::Acquire) {
                    player = None;
                    active_generation = u64::MAX;
                    far_active.store(false, Ordering::Relaxed);
                    thread::sleep(tick);
                    continue;
                }
                let loop_start = Instant::now();
                let generation = device_generation.load(Ordering::Acquire);
                if generation != active_generation {
                    speaker_device = devices
                        .lock()
                        .map(|devices| devices.1.clone())
                        .unwrap_or(None);
                    player = match PulsePlayer::new(speaker_device.as_deref()) {
                        Ok(player) => Some(player),
                        Err(error) => {
                            crate::zcall_log!("[zcall-rust] Pulse playback open failed: {error}");
                            if !speaker_fault_reported {
                                if let Ok(mut events) = media_events.lock() {
                                    events.push_back(GroupMediaEvent::AudioFault("speaker".into()));
                                }
                                speaker_fault_reported = true;
                            }
                            None
                        }
                    };
                    active_generation = generation;
                    non_blocking_streak = 0;
                    last_reopen = Instant::now();
                }

                if player.is_none() && last_reopen.elapsed() >= Duration::from_millis(500) {
                    last_reopen = Instant::now();
                    if let Ok(reopened) = PulsePlayer::new(speaker_device.as_deref()) {
                        crate::zcall_log!("[zcall-rust] Pulse playback stream recovered");
                        player = Some(reopened);
                        non_blocking_streak = 0;
                    }
                }

                let is_group = params
                    .lock()
                    .ok()
                    .and_then(|params| params.as_ref().map(|params| params.is_group))
                    .unwrap_or(false);
                let has_frame = if is_group {
                    group_audio
                        .lock()
                        .map(|mut mixer| mixer.mix_frame(&mut pcm_out) > 0)
                        .unwrap_or(false)
                } else {
                    jitter_buffer
                        .as_ref()
                        .and_then(|jitter| jitter.lock().ok().map(|mut jitter| jitter.pop_frame(&mut pcm_out)))
                        .unwrap_or(false)
                };

                let muted = speaker_off.load(Ordering::Relaxed);
                let mut write_ok = false;
                if let Some(output) = player.as_mut() {
                    let started = Instant::now();
                    let result = if muted || !has_frame {
                        if !muted {
                            far_active.store(false, Ordering::Relaxed);
                        }
                        output.write(&silence)
                    } else {
                        if let Some(apm) = apm.as_ref() {
                            apm.process_render(&pcm_out);
                        } else if let Ok(mut processor) = audio_dsp.lock() {
                            if let Some(processor) = processor.as_mut() {
                                processor.playback(&pcm_out);
                            }
                        }

                        let sum_sq: i64 = pcm_out
                            .iter()
                            .map(|&sample| i64::from(sample) * i64::from(sample))
                            .sum();
                        let mean_sq = sum_sq / FRAME_SAMPLES as i64;
                        if mean_sq > 100_000 {
                            last_far_speech = Instant::now();
                            far_active.store(true, Ordering::Relaxed);
                        }
                        let bytes = bytemuck::cast_slice(&pcm_out);
                        output.write(bytes)
                    };
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    match result {
                        Ok(()) => {
                            write_ok = true;
                            write_block_ms += elapsed_ms;
                            write_count = write_count.saturating_add(1);
                            max_write_ms = max_write_ms.max(elapsed_ms);
                            if elapsed_ms == 0 {
                                non_blocking_streak = non_blocking_streak.saturating_add(1);
                            } else {
                                non_blocking_streak = 0;
                            }
                        }
                        Err(error) => {
                            crate::zcall_log!("[zcall-rust] Pulse playback write failed; reopening: {error}");
                            if !speaker_fault_reported {
                                if let Ok(mut events) = media_events.lock() {
                                    events.push_back(GroupMediaEvent::AudioFault("speaker".into()));
                                }
                                speaker_fault_reported = true;
                            }
                            player = None;
                            last_reopen = Instant::now();
                        }
                    }
                }

                if last_far_speech.elapsed() >= Duration::from_millis(300) {
                    far_active.store(false, Ordering::Relaxed);
                }

                if last_diag.elapsed() >= Duration::from_secs(5) {
                    let jitter_diag = jitter_buffer
                        .as_ref()
                        .and_then(|jitter| jitter.lock().ok().map(|jitter| {
                            let stats = jitter.stats();
                            format!(
                                "rx={} play={} q={}/{} plc={} late_drop={} under={}",
                                stats.rx_packets,
                                stats.played_frames,
                                jitter.queue_len(),
                                jitter.min_target(),
                                stats.plc_frames,
                                stats.late_dropped,
                                stats.underrun_count
                            )
                        }))
                        .unwrap_or_else(|| "group mixer".into());
                    let average_write = if write_count > 0 {
                        write_block_ms / u64::from(write_count)
                    } else {
                        0
                    };
                    let latency = player
                        .as_ref()
                        .and_then(PulsePlayer::latency_ms)
                        .unwrap_or_default();
                    crate::zcall_log!(
                        "[zcall-rust] playout: [{jitter_diag} write={}ms max={}ms pa_lat={}ms] mic={}%",
                        average_write,
                        max_write_ms,
                        latency,
                        mic_volume.load(Ordering::Relaxed).min(100)
                    );
                    last_diag = Instant::now();
                    write_block_ms = 0;
                    write_count = 0;
                    max_write_ms = 0;
                }

                // pa_simple_write follows the device's hardware clock. Sleep only
                // when no device is available or a virtual sink returns immediately.
                if !write_ok || non_blocking_streak > 6 {
                    let elapsed = loop_start.elapsed();
                    if elapsed < tick {
                        thread::sleep(tick - elapsed);
                    }
                }

            }
        });
}

fn ring_ping_interval() -> Duration {
    let value = std::env::var("ZCALL_PING_MS").ok();
    ring_ping_interval_from(value.as_deref())
}

fn ring_ping_interval_from(value: Option<&str>) -> Duration {
    let millis = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (20..=60_000).contains(value))
        .unwrap_or(6_000);
    Duration::from_millis(millis)
}

fn group_share_worker(
    socket: Arc<UdpSocket>,
    share_state: Arc<Mutex<Option<GroupSharePeer>>>,
    events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    running: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    init_packet: Vec<u8>,
) {
    let mut init_tries = 1u8;
    let mut last_init = Instant::now();
    let mut buf = [0u8; 4096];

    while running.load(Ordering::Acquire) && active.load(Ordering::Acquire) {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) if len > 0 => {
                let server = share_state
                    .lock()
                    .ok()
                    .and_then(|peer| peer.as_ref().map(|peer| peer.server));
                if server != Some(from) {
                    continue;
                }
                let packet = &buf[..len];
                if let Some(reply) = parse_server_reply(packet) {
                    if reply.cmd == crate::protocol::zrtp::commands::END_CALL {
                        let is_share = share_state
                            .lock()
                            .ok()
                            .and_then(|peer| peer.as_ref().map(|peer| peer.uid == reply.uid))
                            .unwrap_or(false);
                        if is_share {
                            if let Ok(mut queue) = events.lock() {
                                queue.push_back(GroupMediaEvent::ShareFailed(
                                    "group share peer ended by the SFU".into(),
                                ));
                            }
                            active.store(false, Ordering::Release);
                        }
                    } else if matches!(
                        reply.cmd,
                        crate::protocol::zrtp::commands::INIT_CALL_CALLER
                            | crate::protocol::zrtp::commands::INIT_CALL_CALLEE
                    ) {
                        if let Ok(mut guard) = share_state.lock() {
                            if let Some(peer) = guard.as_mut() {
                                if peer.uid != reply.uid || peer.token != 0 {
                                    continue;
                                }
                                if reply.result == Some(0)
                                    && reply.server_token.is_some_and(|token| token != 0)
                                {
                                    peer.token = reply.server_token.unwrap_or(0);
                                    peer.started_at = Some(Instant::now());
                                    peer.last_ping = Instant::now() - Duration::from_secs(6);
                                    if let Ok(mut queue) = events.lock() {
                                        queue.push_back(GroupMediaEvent::ShareReady);
                                    }
                                } else {
                                    if let Ok(mut queue) = events.lock() {
                                        queue.push_back(GroupMediaEvent::ShareFailed(format!(
                                            "share InitZRTP refused: {:?}",
                                            reply.result
                                        )));
                                    }
                                    peer.active.store(false, Ordering::Release);
                                }
                            }
                        }
                    }
                } else if packet[0] == msg_types::VIDEO_FEC && packet.len() > 5 {
                    let target_uid = share_state
                        .lock()
                        .ok()
                        .and_then(|peer| peer.as_ref().map(|peer| peer.uid));
                    if target_uid.is_some_and(|uid| rtcp_requests_keyframe(&packet[5..], uid)) {
                        if let Ok(mut queue) = events.lock() {
                            queue.push_back(GroupMediaEvent::ShareKeyframeRequested);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                if let Ok(mut queue) = events.lock() {
                    queue.push_back(GroupMediaEvent::ShareFailed(format!(
                        "group share socket receive failed: {error}"
                    )));
                }
                break;
            }
            _ => {}
        }

        let mut outbound = Vec::<(Vec<u8>, SocketAddr)>::new();
        if let Ok(mut guard) = share_state.lock() {
            if let Some(peer) = guard.as_mut() {
                if peer.token == 0 && last_init.elapsed() >= Duration::from_secs(1) {
                    if init_tries >= 3 {
                        if let Ok(mut queue) = events.lock() {
                            queue.push_back(GroupMediaEvent::ShareFailed(
                                "group share peer received no InitZRTP response".into(),
                            ));
                        }
                        peer.active.store(false, Ordering::Release);
                    } else {
                        init_tries += 1;
                        last_init = Instant::now();
                        outbound.push((init_packet.clone(), peer.server));
                    }
                } else if peer.token != 0 && peer.last_ping.elapsed() >= Duration::from_secs(6) {
                    let ping = build_share_zavi_ping_packet(
                        peer.uid,
                        peer.token,
                        peer.control_seq,
                        &peer.sess_id,
                    );
                    peer.control_seq = peer.control_seq.wrapping_add(1);
                    outbound.push((ping, peer.server));
                    if peer.width > 0 && peer.height > 0 {
                        let layer = GroupVideoLayer {
                            id: 0,
                            bitrate_kbps: peer.max_shr_screen_br,
                            width: peer.width,
                            height: peer.height,
                        };
                        let config = build_group_video_quality_config(
                            peer.uid,
                            peer.token,
                            peer.control_seq,
                            &[layer],
                        );
                        peer.control_seq = peer.control_seq.wrapping_add(1);
                        outbound.push((config, peer.server));
                    }
                    peer.last_ping = Instant::now();
                }
                for _ in 0..8 {
                    let Some(packet) = peer.queue.pop_front() else {
                        break;
                    };
                    outbound.push((packet, peer.server));
                }
            }
        }
        for (packet, server) in outbound {
            let _ = socket.send_to(&packet, server);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn rtcp_requests_keyframe(packet: &[u8], target_uid: u32) -> bool {
    let mut offset = 0usize;
    while offset + 12 <= packet.len() {
        if packet[offset] >> 6 != 2 {
            return false;
        }
        let length =
            (u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize + 1) * 4;
        if length < 12 || offset + length > packet.len() {
            return false;
        }
        let format = packet[offset] & 0x1f;
        if packet[offset + 1] == 206 && format == 1 {
            let uid =
                u32::from_be_bytes(packet[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
            if uid == target_uid {
                return true;
            }
        } else if packet[offset + 1] == 206 && format == 4 && length >= 16 {
            let uid = u32::from_be_bytes(
                packet[offset + 12..offset + 16]
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            if uid == target_uid {
                return true;
            }
        }
        offset += length;
    }
    false
}

fn group_event_from_reply(
    reply: &crate::protocol::zrtp::ServerReply,
    local_uid: u32,
) -> Option<GroupMediaEvent> {
    use crate::protocol::zrtp::commands;

    match reply.cmd {
        commands::ROOM_UPDATED if reply.uid == local_uid => {
            let mut members = parse_room_updated(&reply.payload)?;
            members.retain(|uid| *uid != 0 && *uid != local_uid);
            Some(GroupMediaEvent::Roster(members))
        }
        commands::END_CALL if reply.uid == local_uid && matches!(reply.sub_cmd, 1 | 2) => {
            Some(GroupMediaEvent::PeerEnd(reply.sub_cmd))
        }
        commands::END_CALL if reply.uid != 0 && reply.uid != local_uid => {
            Some(GroupMediaEvent::MemberLeft(reply.uid, reply.sub_cmd))
        }
        _ => None,
    }
}

/// Authenticated RFC 3711 rollover tracking with recovery for a peer that
/// restarts its RTP sequence/ROC after a network handover.
#[derive(Debug, Clone, Default)]
struct RocTracker {
    last: Option<u16>,
    roc: u32,
}

impl RocTracker {
    fn guess(&self, seq: u16) -> u32 {
        let Some(last) = self.last else {
            return self.roc;
        };
        if seq < last && last.wrapping_sub(seq) > 0x8000 {
            self.roc.wrapping_add(1)
        } else if seq > last && seq.wrapping_sub(last) > 0x8000 && self.roc > 0 {
            self.roc - 1
        } else {
            self.roc
        }
    }

    fn candidates(&self, seq: u16) -> Vec<u32> {
        let guessed = self.guess(seq);
        let mut values = vec![guessed, 0, guessed.wrapping_add(1)];
        if guessed > 0 {
            values.push(guessed - 1);
        }
        let mut seen = HashSet::new();
        values.retain(|value| seen.insert(*value));
        values
    }

    /// Update only on forward movement or a large backwards jump. Call this
    /// only after SRTP authentication succeeds.
    fn update(&mut self, seq: u16, roc: u32) {
        let Some(last) = self.last else {
            self.last = Some(seq);
            self.roc = roc;
            return;
        };
        let previous = (u64::from(self.roc) << 16) | u64::from(last);
        let next = (u64::from(roc) << 16) | u64::from(seq);
        if next > previous || previous - next > 100 {
            self.last = Some(seq);
            self.roc = roc;
        }
    }
}

// Diagnostic-only compatibility probe. It never accepts/decrypts a packet;
// it reports whether the live tag matches common RTP header/tag/ROC layouts.
fn probe_srtp_auth_layout(rtp: &[u8], keys: &SrtpKeys) -> String {
    if rtp.len() < 16 {
        return "packet-too-short".into();
    }
    let Some(header_len) = parse_rtp_header_len(rtp) else {
        return "invalid-rtp-header".into();
    };
    // Diagnostic-only probe for the SFU's `srtpNoSeq` mode. Try the standard
    // SRTP authenticated region first, then explicitly test whether the peer
    // excludes RTP sequence bytes, zeroes them, or omits ROC. A match here is
    // reported only; it is never accepted as media until a matching strict
    // unprotect path and a regression test are added.
    let header = &rtp[..header_len];
    let mut header_without_seq = Vec::with_capacity(header_len.saturating_sub(2));
    header_without_seq.extend_from_slice(&header[..2]);
    header_without_seq.extend_from_slice(&header[4..]);
    let mut header_zero_seq = header.to_vec();
    header_zero_seq[2..4].fill(0);

    for tag_len in (4usize..=20).rev() {
        if rtp.len() < header_len + tag_len {
            continue;
        }
        let tag_start = rtp.len() - tag_len;
        let tag = &rtp[tag_start..];
        for mki_len in 0..=16 {
            if tag_start < header_len + mki_len {
                continue;
            }
            let body_end = tag_start - mki_len;
            let body = &rtp[header_len..body_end];
            for roc in [Some(0u32), Some(1), Some(2), Some(3), None] {
                let roc_be = roc.map(u32::to_be_bytes);
                let roc_le = roc.map(u32::to_le_bytes);
                let check = |mode: &str, digest: [u8; 20]| -> Option<String> {
                    (digest[..tag_len] == *tag).then(|| {
                        format!(
                            "match:mode={mode}:roc={}:header={header_len}:mki={mki_len}:tag={tag_len}",
                            roc.map(|value| value.to_string()).unwrap_or_else(|| "none".into())
                        )
                    })
                };
                if let Some(roc_bytes) = roc_be.as_ref() {
                    if let Some(result) = check(
                        "standard-be",
                        hmac_sha1(&keys.auth, &[header, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "without-rtp-seq-be",
                        hmac_sha1(&keys.auth, &[&header_without_seq, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "zero-rtp-seq-be",
                        hmac_sha1(&keys.auth, &[&header_zero_seq, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "body-plus-roc-be",
                        hmac_sha1(&keys.auth, &[body, roc_bytes]),
                    ) {
                        return result;
                    }
                }
                if let Some(roc_bytes) = roc_le.as_ref() {
                    if let Some(result) = check(
                        "standard-le",
                        hmac_sha1(&keys.auth, &[header, body, roc_bytes]),
                    ) {
                        return result;
                    }
                }
                if roc.is_none() {
                    if let Some(result) =
                        check("without-roc", hmac_sha1(&keys.auth, &[header, body]))
                    {
                        return result;
                    }
                    if let Some(result) = check(
                        "without-rtp-seq-and-roc",
                        hmac_sha1(&keys.auth, &[&header_without_seq, body]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "zero-rtp-seq-and-roc",
                        hmac_sha1(&keys.auth, &[&header_zero_seq, body]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check("body-only", hmac_sha1(&keys.auth, &[body])) {
                        return result;
                    }
                }
            }
        }
    }
    format!("no-known-auth-layout:parsed_header={header_len}")
}

fn unprotect_with_roc_recovery(
    rtp: &[u8],
    keys: &SrtpKeys,
    tracker: &mut RocTracker,
) -> Option<(crate::crypto::srtp::UnprotectedRtp, u32)> {
    if rtp.len() < 4 {
        return None;
    }
    let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
    for roc in tracker.candidates(seq) {
        if let Some(clear) = unprotect_rtp(rtp, keys, roc) {
            tracker.update(seq, roc);
            return Some((clear, roc));
        }
    }
    None
}

fn group_camera_advertised(video_enabled: bool, camera_enabled: bool) -> bool {
    video_enabled && camera_enabled
}

fn looks_like_group_h264_payload(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1])
        || payload.starts_with(&[0, 0, 0, 1])
        // H.264 FU-A indicator: NAL type 28 is the low five bits;
        // the NRI bits vary with the source stream (commonly 0x7c/0x5c).
        || (payload.len() >= 2
            && payload[0] & 0x1f == 28
            && payload[1] & 0x1f != 0
            && payload[1] & 0x1f <= 23)
}

fn unprotect_group_with_candidates(
    rtp: &[u8],
    primary: Option<&SrtpKeys>,
    fallback: Option<&SrtpKeys>,
    rocs: &[u32],
    profile: Option<(usize, usize)>,
) -> Option<(crate::crypto::srtp::UnprotectedRtp, u32, bool, usize, usize)> {
    // Keep the standard HMAC-SHA1-80/no-MKI profile first. RFC 3711 makes
    // both MKI and authentication-tag lengths configurable, so discover the
    // negotiated profile on the first authenticated group packet rather
    // than assuming a short hard-coded list. The caller caches the returned
    // profile for all subsequent audio/video packets in this session.
    const TAG_LENGTHS: [usize; 6] = [10, 8, 12, 4, 16, 20];
    for (candidate, is_fallback) in [(primary, false), (fallback, true)] {
        let Some(candidate) = candidate else {
            continue;
        };
        for roc in rocs.iter().copied() {
            if let Some((mki_len, tag_len)) = profile {
                if let Some(clear) =
                    unprotect_rtp_with_profile(rtp, candidate, roc, mki_len, tag_len)
                {
                    return Some((clear, roc, is_fallback, mki_len, tag_len));
                }
                continue;
            }
            for tag_len in TAG_LENGTHS {
                for mki_len in 0..=16 {
                    if let Some(clear) =
                        unprotect_rtp_with_profile(rtp, candidate, roc, mki_len, tag_len)
                    {
                        return Some((clear, roc, is_fallback, mki_len, tag_len));
                    }
                }
            }
        }
    }
    None
}

struct AudioDeduplicator {
    seen: HashSet<(u32, u32)>,
    order: VecDeque<(u32, u32)>,
    capacity: usize,
}

impl AudioDeduplicator {
    fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Return true for the first authenticated copy of this SSRC/extended seq.
    fn accept(&mut self, ssrc: u32, extended_seq: u32) -> bool {
        let key = (ssrc, extended_seq);
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.seen.len()
    }
}

fn stop_p2p_path(
    socket: &UdpSocket,
    selected_server: Option<(SocketAddr, u32)>,
    p2p_path: &Mutex<Option<SocketAddr>>,
    p2p_enabled: &AtomicBool,
    params: &MediaSessionParams,
    notify_peer: bool,
) -> bool {
    p2p_enabled.store(false, Ordering::Release);
    let old_path = p2p_path.lock().ok().and_then(|mut path| path.take());
    if notify_peer {
        for seq in 0..3u32 {
            if let Some(peer) = old_path {
                let signal = p2p::signal(!params.is_caller, params.call_id, 2, seq);
                let _ = socket.send_to(&signal, peer);
            }
            if let Some((server, token)) = selected_server {
                let request = build_req_forward_packet(params.uid, token, seq, 2, &[]);
                let _ = socket.send_to(&request, server);
            }
        }
    }
    old_path.is_some()
}

fn send_change_address(
    socket: &UdpSocket,
    selected_server: Option<(SocketAddr, u32)>,
    params: &MediaSessionParams,
    sequence: u32,
) {
    let Some((server, token)) = selected_server else {
        return;
    };
    let change = build_change_address_packet(params.uid, token, sequence, &params.sess_id);
    let _ = socket.send_to(&change, server);

    let ping = build_ping_packet(params.uid, token, sequence, &params.sess_id);
    let _ = socket.send_to(&ping, server);

    if !params.is_group {
        let request = build_req_forward_packet(params.uid, token, 0, 8, &[4, 0, 0, 0, 0, 0, 0, 0]);
        let _ = socket.send_to(&request, server);
    }
}

fn local_ipv4_addresses() -> Vec<std::net::Ipv4Addr> {
    let Ok(addresses) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };

    addresses
        .filter_map(|interface| {
            interface
                .address
                .as_ref()
                .and_then(|address| address.as_sockaddr_in())
                .map(|address| address.ip())
        })
        .filter(|address| !address.is_loopback() && !address.is_unspecified())
        .collect()
}

fn local_ipv4_signature() -> String {
    let mut addresses = local_ipv4_addresses()
        .into_iter()
        .map(|address| address.to_string())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    addresses.join(",")
}

fn read_plain_rtp_payload(rtp: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
    if rtp.len() < 12 || rtp[0] >> 6 != 2 {
        return None;
    }
    let header_len = parse_rtp_header_len(rtp)?;
    let mut end = rtp.len();
    if rtp[0] & 0x20 != 0 {
        let padding = *rtp.last()? as usize;
        if padding == 0 || padding > end.saturating_sub(header_len) {
            return None;
        }
        end -= padding;
    }
    if end <= header_len {
        return None;
    }
    Some((
        u16::from_be_bytes([rtp[2], rtp[3]]),
        u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]),
        rtp[header_len..end].to_vec(),
    ))
}

fn accepts_relay_source(selected: Option<SocketAddr>, from: SocketAddr, is_group: bool) -> bool {
    // The legacy group receiver accepted SFU media/control datagrams without
    // pinning them to the InitZRTP source endpoint. Relays may use another
    // port or host for downlink; SRTP authenticates encrypted media. Preserve
    // strict endpoint pinning for one-to-one calls.
    selected == Some(from) || is_group
}

fn media_debug_reset() {
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-media-debug.log")))
    {
        let _ = std::fs::File::create(path);
    }
}

fn media_debug(message: &str) {
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-media-debug.log")))
    {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{message}");
        }
    }
}

fn next_group_control_sequence(sequence: &AtomicU32) -> u32 {
    sequence.fetch_add(1, Ordering::Relaxed)
}

fn media_receive_timed_out(last_rx_ms: u64, now_ms: u64) -> bool {
    last_rx_ms > 0 && now_ms.saturating_sub(last_rx_ms) >= MEDIA_TIMEOUT_MS
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub struct CandidateServer {
    pub addr: SocketAddr,
    pub token: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupMediaEvent {
    Roster(Vec<u32>),
    MemberLeft(u32, u8),
    PeerEnd(u8),
    ShareReady,
    ShareKeyframeRequested,
    ShareFailed(String),
    MemberAudio(u32),
    MemberVideo(u32),
    MemberSpeaking(u32, bool),
    AudioFault(String),
    EarlyAudio(&'static str),
}

const SPEAK_DBOV: u8 = 50;
const SPEAK_WINDOW: usize = 10;
const SPEAK_ON_FRAMES: usize = 4;
const SPEAK_HOLD: Duration = Duration::from_millis(600);
const MEDIA_TIMEOUT_MS: u64 = 20_000;

fn early_audio_phase(
    is_caller: bool,
    is_group: bool,
    audio_enabled: bool,
    authenticated_packets: u32,
) -> Option<&'static str> {
    if !is_caller || is_group || audio_enabled {
        return None;
    }
    match authenticated_packets {
        1 => Some("first"),
        50 => Some("connect"),
        _ => None,
    }
}

#[derive(Default)]
struct MemberSpeakingTracker {
    history: VecDeque<bool>,
    active: bool,
    last_loud: Option<Instant>,
}

impl MemberSpeakingTracker {
    fn observe(&mut self, level: u8, now: Instant) -> Option<bool> {
        let loud = level <= SPEAK_DBOV;
        self.history.push_back(loud);
        if self.history.len() > SPEAK_WINDOW {
            self.history.pop_front();
        }
        if loud {
            self.last_loud = Some(now);
        }
        let next = if self.active {
            self.last_loud
                .is_some_and(|last| now.duration_since(last) < SPEAK_HOLD)
        } else {
            self.history.iter().filter(|sample| **sample).count() >= SPEAK_ON_FRAMES
        };
        if next == self.active {
            return None;
        }
        self.active = next;
        Some(next)
    }

    fn expire(&mut self, now: Instant) -> bool {
        if self.active
            && self
                .last_loud
                .is_some_and(|last| now.duration_since(last) >= SPEAK_HOLD)
        {
            self.active = false;
            self.last_loud = None;
            self.history.clear();
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
pub struct MediaSessionParams {
    pub call_id: u32,
    pub host_call: u32,
    pub uid: u32,
    pub peer_uid: u32,
    pub sess_id: Vec<u8>,
    pub is_caller: bool,
    pub is_group: bool,
    pub send_udp: bool,
    pub video_enabled: bool,
    pub video_config: Value,
    pub p2p_candidates: Vec<SocketAddr>,
}

type VideoHistory = Arc<Mutex<VecDeque<(u16, Vec<u8>)>>>;

#[derive(Default)]
struct VideoTxState {
    seq: u16,
    roc: u32,
    layer_seq: u16,
    started_at: Option<Instant>,
    key_sent: bool,
}

struct GroupSharePeer {
    socket: Arc<UdpSocket>,
    server: SocketAddr,
    uid: u32,
    token: u32,
    sess_id: Vec<u8>,
    call_id: u32,
    active: Arc<AtomicBool>,
    seq: u16,
    twcc_seq: u16,
    layer_seq: u16,
    control_seq: u32,
    started_at: Option<Instant>,
    timestamp_base: u32,
    last_ping: Instant,
    width: u16,
    height: u16,
    max_shr_screen_br: u32,
    sfu_id: u8,
    sfu_long: bool,
    key_sent: bool,
    keyframe_requested: bool,
    queue: VecDeque<Vec<u8>>,
}

pub struct ZrtcMediaBackend {
    socket: Arc<UdpSocket>,
    running: Arc<AtomicBool>,
    audio_enabled: Arc<AtomicBool>,
    selected_server: Arc<Mutex<Option<(SocketAddr, u32)>>>,
    ring_servers: Arc<Mutex<Vec<(SocketAddr, u32)>>>,
    keys: Arc<Mutex<Option<SrtpKeys>>>,
    // Group SFU receive can use a distinct session master from zrtcConfig.srtpKey.
    // Keep it receive-only and accept it only after SRTP authentication succeeds.
    rx_fallback_keys: Arc<Mutex<Option<SrtpKeys>>>,
    params: Arc<Mutex<Option<MediaSessionParams>>>,
    last_ping: Arc<Mutex<Instant>>,
    ring_ping_running: Arc<AtomicBool>,
    public_addr: Arc<Mutex<Option<String>>>,
    muted: Arc<AtomicBool>,
    speaker_off: Arc<AtomicBool>,
    devices: Arc<Mutex<(Option<String>, Option<String>)>>,
    device_generation: Arc<AtomicU64>,
    video_frames: Arc<Mutex<VecDeque<VideoFrame>>>,
    video_history: VideoHistory,
    video_tx: Mutex<VideoTxState>,
    twcc_seq: Arc<AtomicU16>,
    rtcp: Arc<Mutex<Option<RtcpFeedback>>>,
    nack: Arc<Mutex<NackTracker>>,
    local_keyframe_request: Arc<AtomicBool>,
    last_peer_pli: Mutex<Option<Instant>>,
    p2p_path: Arc<Mutex<Option<SocketAddr>>>,
    p2p_enabled: Arc<AtomicBool>,
    p2p_seq: Arc<AtomicU16>,
    send_rate: Arc<Mutex<Option<SendRateController>>>,
    pending_video_profile: Arc<Mutex<Option<VideoProfile>>>,
    group_video_plan: Arc<Mutex<Option<GroupVideoPlan>>>,
    group_video_wanted: Arc<Mutex<HashMap<u32, u8>>>,
    group_video_render_width: Arc<Mutex<HashMap<u32, u32>>>,
    group_control_seq: Arc<AtomicU32>,
    group_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    share_peer: Arc<Mutex<Option<GroupSharePeer>>>,
    remote_share_uid: Arc<AtomicU32>,
    last_video_sent_ms: Arc<AtomicU64>,
    last_media_rx_ms: Arc<AtomicU64>,
    first_video_ui_drain_logged: AtomicBool,
    mic_volume: Arc<AtomicU8>,
    mic_volume_dirty: Arc<AtomicBool>,
    mic_manual: Arc<AtomicBool>,
    pending_mic_volume: Arc<Mutex<Option<u8>>>,
    mic_level: Arc<AtomicU8>,
    mic_level_dirty: Arc<AtomicBool>,
    tx_audio_packets: Arc<AtomicU64>,
    rx_audio_packets: Arc<AtomicU64>,
    tx_video_packets: Arc<AtomicU64>,
}

impl ZrtcMediaBackend {
    pub fn new() -> Result<Self, String> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Failed to bind UDP socket: {}", e))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|e| format!("Failed to set socket read timeout: {}", e))?;

        let backend = Self {
            socket: Arc::new(socket),
            running: Arc::new(AtomicBool::new(false)),
            audio_enabled: Arc::new(AtomicBool::new(false)),
            selected_server: Arc::new(Mutex::new(None)),
            ring_servers: Arc::new(Mutex::new(Vec::new())),
            keys: Arc::new(Mutex::new(None)),
            rx_fallback_keys: Arc::new(Mutex::new(None)),
            params: Arc::new(Mutex::new(None)),
            last_ping: Arc::new(Mutex::new(Instant::now())),
            ring_ping_running: Arc::new(AtomicBool::new(false)),
            public_addr: Arc::new(Mutex::new(None)),
            muted: Arc::new(AtomicBool::new(false)),
            speaker_off: Arc::new(AtomicBool::new(false)),
            devices: Arc::new(Mutex::new((None, None))),
            device_generation: Arc::new(AtomicU64::new(0)),
            video_frames: Arc::new(Mutex::new(VecDeque::new())),
            video_history: Arc::new(Mutex::new(VecDeque::new())),
            video_tx: Mutex::new(VideoTxState::default()),
            twcc_seq: Arc::new(AtomicU16::new(0)),
            rtcp: Arc::new(Mutex::new(None)),
            nack: Arc::new(Mutex::new(NackTracker::default())),
            local_keyframe_request: Arc::new(AtomicBool::new(false)),
            last_peer_pli: Mutex::new(None),
            p2p_path: Arc::new(Mutex::new(None)),
            p2p_enabled: Arc::new(AtomicBool::new(false)),
            p2p_seq: Arc::new(AtomicU16::new(0)),
            send_rate: Arc::new(Mutex::new(None)),
            pending_video_profile: Arc::new(Mutex::new(None)),
            group_video_plan: Arc::new(Mutex::new(None)),
            group_video_wanted: Arc::new(Mutex::new(HashMap::new())),
            group_video_render_width: Arc::new(Mutex::new(HashMap::new())),
            group_control_seq: Arc::new(AtomicU32::new(0)),
            group_events: Arc::new(Mutex::new(VecDeque::new())),
            share_peer: Arc::new(Mutex::new(None)),
            remote_share_uid: Arc::new(AtomicU32::new(0)),
            last_video_sent_ms: Arc::new(AtomicU64::new(0)),
            last_media_rx_ms: Arc::new(AtomicU64::new(0)),
            first_video_ui_drain_logged: AtomicBool::new(false),
            mic_volume: Arc::new(AtomicU8::new(100)),
            mic_volume_dirty: Arc::new(AtomicBool::new(true)),
            mic_manual: Arc::new(AtomicBool::new(false)),
            pending_mic_volume: Arc::new(Mutex::new(None)),
            mic_level: Arc::new(AtomicU8::new(127)),
            mic_level_dirty: Arc::new(AtomicBool::new(false)),
            tx_audio_packets: Arc::new(AtomicU64::new(0)),
            rx_audio_packets: Arc::new(AtomicU64::new(0)),
            tx_video_packets: Arc::new(AtomicU64::new(0)),
        };

        Ok(backend)
    }

    pub fn prepare(
        &self,
        params: MediaSessionParams,
        servers: &[SocketAddr],
    ) -> Result<Option<SocketAddr>, String> {
        media_debug_reset();
        media_debug(&format!(
            "session-start mode={} uid={} video_enabled={} send_udp={} relay_candidates={}",
            if params.is_group {
                "group"
            } else {
                "one-to-one"
            },
            params.uid,
            params.video_enabled,
            params.send_udp,
            servers.len()
        ));
        self.audio_enabled.store(false, Ordering::Release);
        self.tx_audio_packets.store(0, Ordering::Relaxed);
        self.rx_audio_packets.store(0, Ordering::Relaxed);
        self.tx_video_packets.store(0, Ordering::Relaxed);
        self.stop_group_share();
        self.remote_share_uid.store(0, Ordering::Release);
        self.ring_ping_running.store(false, Ordering::SeqCst);
        *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
        *self.selected_server.lock().map_err(|e| e.to_string())? = None;
        *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        *self.keys.lock().map_err(|e| e.to_string())? = None;
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = None;
        self.video_tx
            .lock()
            .map_err(|e| e.to_string())
            .map(|mut state| *state = VideoTxState::default())?;
        self.video_frames.lock().map_err(|e| e.to_string())?.clear();
        self.video_history
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        *self.rtcp.lock().map_err(|e| e.to_string())? =
            params.send_udp.then(|| RtcpFeedback::new(params.uid));
        *self.send_rate.lock().map_err(|e| e.to_string())? =
            if params.send_udp && params.video_enabled {
                Some(SendRateController::new(&params.video_config, params.uid))
            } else {
                None
            };
        *self
            .pending_video_profile
            .lock()
            .map_err(|e| e.to_string())? = None;
        self.last_video_sent_ms.store(0, Ordering::Relaxed);
        self.last_media_rx_ms.store(0, Ordering::Relaxed);
        self.first_video_ui_drain_logged
            .store(false, Ordering::Relaxed);
        let group_plan = if params.is_group {
            parse_group_video_plan(&params.video_config)
        } else {
            None
        };
        *self.group_video_plan.lock().map_err(|e| e.to_string())? = group_plan;
        self.group_video_wanted
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        self.group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        self.group_control_seq.store(0, Ordering::Relaxed);
        self.group_events.lock().map_err(|e| e.to_string())?.clear();
        self.nack.lock().map_err(|e| e.to_string())?.clear();
        self.local_keyframe_request.store(false, Ordering::Relaxed);
        *self.last_peer_pli.lock().map_err(|e| e.to_string())? = None;
        *self.p2p_path.lock().map_err(|e| e.to_string())? = None;
        self.p2p_enabled
            .store(!params.p2p_candidates.is_empty(), Ordering::Release);
        self.p2p_seq.store(0, Ordering::Relaxed);

        if !params.send_udp {
            let selected = servers.first().copied();
            *self.params.lock().map_err(|e| e.to_string())? = Some(params);
            if let Some(addr) = selected {
                *self.selected_server.lock().map_err(|e| e.to_string())? = Some((addr, 0));
            }
            return Ok(selected);
        }
        if params.is_group {
            return self.prepare_group(params, servers);
        }
        let master = master_from_sess_id(&params.sess_id)
            .map_err(|err| format!("invalid call-v2 session id: {err}"))?;
        let session = session_keys(&master)
            .map_err(|err| format!("failed to derive SRTP session keys: {err}"))?;
        *self.keys.lock().map_err(|e| e.to_string())? = Some(session);
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = None;

        let init_pkt = build_init_zrtp_packet(
            params.is_caller,
            params.uid,
            params.peer_uid,
            params.call_id,
            &params.sess_id,
            &[],
            0x02,
        );

        let allowed: HashSet<SocketAddr> = servers.iter().copied().collect();
        let mut responded = HashSet::new();
        let mut accepted: Vec<(SocketAddr, u32, Option<String>)> = Vec::new();
        let mut buf = [0u8; 1500];
        let started = Instant::now();
        let mut next_retry = Duration::from_millis(1000);
        let max_wait = Duration::from_millis(3200);
        let mut attempt = 0u8;

        while started.elapsed() < max_wait && responded.len() < allowed.len() {
            if attempt == 0 || (attempt < 3 && started.elapsed() >= next_retry) {
                for &server in servers {
                    if !responded.contains(&server) {
                        let _ = self.socket.send_to(&init_pkt, server);
                    }
                }
                attempt += 1;
                next_retry = Duration::from_millis(1000 * u64::from(attempt));
            }

            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    // Do not let an unrelated datagram select or poison the relay session.
                    if !allowed.contains(&from) || responded.contains(&from) {
                        continue;
                    }
                    if let Some(reply) = parse_server_reply(&buf[..len]) {
                        if reply.cmd != crate::protocol::zrtp::commands::INIT_CALL_CALLER
                            && reply.cmd != crate::protocol::zrtp::commands::INIT_CALL_CALLEE
                        {
                            continue;
                        }
                        responded.insert(from);
                        if reply.result == Some(0) {
                            if let Some(token) = reply.server_token {
                                accepted.push((from, token, reply.public_addr));
                            }
                        }
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut => {}
                Err(err) => return Err(format!("ZRTP receive failed: {err}")),
            }
        }

        // Preserve response arrival order: the first successful relay is the
        // lowest-latency path and is selected for the connected media session.
        let candidates: Vec<(SocketAddr, u32)> = accepted
            .iter()
            .map(|(addr, token, _)| (*addr, *token))
            .collect();
        let fastest = candidates.first().copied();
        if let Some((addr, token)) = fastest {
            let public = accepted.first().and_then(|(_, _, public)| public.clone());
            *self.selected_server.lock().map_err(|e| e.to_string())? = Some((addr, token));
            *self.public_addr.lock().map_err(|e| e.to_string())? = public;
            *self.ring_servers.lock().map_err(|e| e.to_string())? = candidates;
            self.start_ring_ping();
        } else {
            *self.selected_server.lock().map_err(|e| e.to_string())? = None;
            *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
            *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        }

        *self.params.lock().map_err(|e| e.to_string())? = Some(params);
        Ok(fastest.map(|(addr, _)| addr))
    }

    /// Group calls use the SFU's separate InitZRTP layout and derive media
    /// protection from zrtcConfig.srtpKey (or plain RTP when it is empty).
    /// Candidate layouts are advanced only after an explicit server refusal.
    fn prepare_group(
        &self,
        params: MediaSessionParams,
        servers: &[SocketAddr],
    ) -> Result<Option<SocketAddr>, String> {
        let plan = self
            .group_video_plan
            .lock()
            .ok()
            .and_then(|plan| plan.clone());
        media_debug(&format!(
            "group-video-plan sfu_id={:?} sfu_long={} encode={}x{}@{}fps send_layers={:?}",
            plan.as_ref().and_then(|plan| plan.sfu_id),
            plan.as_ref().is_some_and(|plan| plan.sfu_long),
            plan.as_ref().map(|plan| plan.encode.width).unwrap_or(0),
            plan.as_ref().map(|plan| plan.encode.height).unwrap_or(0),
            plan.as_ref().map(|plan| plan.encode.fps).unwrap_or(0),
            plan.as_ref()
                .map(|plan| plan.send.iter().map(|layer| layer.id).collect::<Vec<_>>())
                .unwrap_or_default()
        ));
        let group_key = params
            .video_config
            .get("srtpKey")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let master_fp = sha1(&[group_key.as_bytes()]);
        let master_fp = master_fp[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // These SFU flags are relevant to the wire SRTP profile. Log only
        // scalar type/value metadata; never serialize the full config/key.
        let config_scalar = |name: &str| {
            params
                .video_config
                .get(name)
                .map(|value| match value {
                    Value::Null => "null".to_string(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.to_string(),
                    Value::String(value) => format!("string(len={})", value.len()),
                    Value::Array(_) => "array".to_string(),
                    Value::Object(_) => "object".to_string(),
                })
                .unwrap_or_else(|| "missing".to_string())
        };
        let srtp_mode = config_scalar("srtpMode");
        let srtp_no_seq = config_scalar("srtpNoSeq");
        let audio_srtp_no_seq = config_scalar("audSrtpNoSeq");
        media_debug(&format!(
            "group-config uid={} video_enabled={} srtp={} key_bytes={} key_fp={} srtp_mode={} srtp_no_seq={} audio_srtp_no_seq={} relay_candidates={} video_layers={}",
            params.uid,
            params.video_enabled,
            !group_key.is_empty(),
            group_key.len(),
            master_fp,
            srtp_mode,
            srtp_no_seq,
            audio_srtp_no_seq,
            servers.len(),
            plan.as_ref().map(|plan| plan.layers.len()).unwrap_or(0)
        ));
        if servers.is_empty() {
            *self.params.lock().map_err(|e| e.to_string())? = Some(params);
            return Ok(None);
        }

        let primary = if group_key.is_empty() {
            None
        } else {
            let master = master_from_sess_id(group_key.as_bytes())
                .map_err(|error| format!("invalid group srtpKey: {error}"))?;
            Some(
                session_keys(&master)
                    .map_err(|error| format!("failed to derive group SRTP keys: {error}"))?,
            )
        };
        let primary_present = primary.is_some();
        *self.keys.lock().map_err(|e| e.to_string())? = primary;

        // The legacy group path uses zrtcConfig.srtpKey when present and plain
        // RTP when it is absent. Some SFU receive paths may nevertheless use
        // callSetting.session; keep that as a receive-only authenticated
        // candidate. A failed HMAC never falls through as decrypted media.
        let fallback = if params.sess_id.as_slice() != group_key.as_bytes() {
            master_from_sess_id(&params.sess_id)
                .ok()
                .and_then(|master| session_keys(&master).ok())
        } else {
            None
        };
        let session_fp = sha1(&[&params.sess_id]);
        let session_fp = session_fp[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        media_debug(&format!(
            "group-key-candidates primary={} fallback_session={} session_bytes={} session_fp={}",
            primary_present,
            fallback.is_some(),
            params.sess_id.len(),
            session_fp
        ));
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = fallback;

        let forced = std::env::var("ZCALL_GROUP_INIT_LAYOUT").ok();
        let mut layouts = Vec::new();
        if let Some(value) = forced.as_deref() {
            let layout = GroupInitLayout::parse(value)
                .ok_or_else(|| format!("unknown ZCALL_GROUP_INIT_LAYOUT: {value}"))?;
            layouts.push(layout);
        } else {
            let saved = std::env::var("ZCALL_LOG_DIR")
                .ok()
                .map(|directory| std::path::PathBuf::from(directory).join("group-init-layout"))
                .and_then(|path| std::fs::read_to_string(path).ok());
            if let Some(layout) = saved.as_deref().and_then(GroupInitLayout::parse) {
                layouts.push(layout);
            }
            for layout in GroupInitLayout::ALL {
                if !layouts.contains(&layout) {
                    layouts.push(layout);
                }
            }
        }

        let allowed: HashSet<SocketAddr> = servers.iter().copied().collect();
        let expected_cmd = if params.is_caller {
            crate::protocol::zrtp::commands::INIT_CALL_CALLER
        } else {
            crate::protocol::zrtp::commands::INIT_CALL_CALLEE
        };

        for layout in layouts {
            let packet = build_group_init_zrtp_packet(
                params.is_caller,
                params.uid,
                &params.sess_id,
                params.call_id,
                params.host_call,
                params.video_enabled,
                layout,
            );
            let mut responded = HashSet::new();
            let mut accepted = Vec::<(SocketAddr, u32, Option<String>)>::new();
            let mut buf = [0u8; 1500];
            let started = Instant::now();
            let max_wait = Duration::from_millis(3200);
            let mut attempt = 0u8;
            let mut next_retry = Duration::from_millis(1000);

            while started.elapsed() < max_wait && responded.len() < allowed.len() {
                if attempt == 0 || (attempt < 3 && started.elapsed() >= next_retry) {
                    for server in servers {
                        if !responded.contains(server) {
                            let _ = self.socket.send_to(&packet, server);
                        }
                    }
                    attempt += 1;
                    next_retry = Duration::from_millis(1000 * u64::from(attempt));
                }

                match self.socket.recv_from(&mut buf) {
                    Ok((len, from)) => {
                        if !allowed.contains(&from) || responded.contains(&from) {
                            continue;
                        }
                        let Some(reply) = parse_server_reply(&buf[..len]) else {
                            continue;
                        };
                        if reply.cmd != expected_cmd {
                            continue;
                        }
                        responded.insert(from);
                        if reply.result == Some(0) {
                            if let Some(token) = reply.server_token {
                                accepted.push((from, token, reply.public_addr));
                            }
                        }
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(error) => return Err(format!("group ZRTP receive failed: {error}")),
                }
            }

            if let Some((server, token, public)) = accepted.first().cloned() {
                *self.selected_server.lock().map_err(|e| e.to_string())? = Some((server, token));
                *self.public_addr.lock().map_err(|e| e.to_string())? = public;
                *self.ring_servers.lock().map_err(|e| e.to_string())? = accepted
                    .iter()
                    .map(|(address, media_token, _)| (*address, *media_token))
                    .collect();
                *self.params.lock().map_err(|e| e.to_string())? = Some(params);
                if forced.is_none() {
                    if let Ok(directory) = std::env::var("ZCALL_LOG_DIR") {
                        let path = std::path::PathBuf::from(directory);
                        let _ = std::fs::create_dir_all(&path);
                        let _ = std::fs::write(path.join("group-init-layout"), layout.name());
                    }
                }
                crate::zcall_log!(
                    "[zcall-rust] group InitZRTP accepted with layout {}",
                    layout.name()
                );
                media_debug(&format!(
                    "init-accepted layout={} accepted_relays={}",
                    layout.name(),
                    accepted.len()
                ));
                return Ok(Some(server));
            }

            if responded.is_empty() {
                // A silent relay is a network/route failure, not evidence that
                // another speculative payload layout will be accepted.
                *self.params.lock().map_err(|e| e.to_string())? = Some(params);
                return Ok(None);
            }
            crate::zcall_log!(
                "[zcall-rust] group InitZRTP layout {} refused by {}/{} server(s)",
                layout.name(),
                responded.len(),
                allowed.len()
            );
        }

        *self.selected_server.lock().map_err(|e| e.to_string())? = None;
        *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
        *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        *self.params.lock().map_err(|e| e.to_string())? = Some(params);
        Ok(None)
    }

    pub fn media_timed_out(&self) -> bool {
        let params = self.params.lock().ok().and_then(|params| params.clone());
        let Some(params) = params else {
            return false;
        };
        if params.is_group || !params.send_udp || !self.running.load(Ordering::Acquire) {
            return false;
        }
        media_receive_timed_out(self.last_media_rx_ms.load(Ordering::Acquire), unix_now_ms())
    }

    pub fn start(&self) {
        self.start_with_audio(true);
    }

    /// Start the packet transport without opening hardware audio devices.
    /// Used by transport-only tests and diagnostics; production calls use
    /// start(), which enables capture, decode and playback.
    pub fn start_with_audio(&self, audio_enabled: bool) {
        self.ring_ping_running.store(false, Ordering::SeqCst);
        self.finish_ring_selection();
        let send_udp = self
            .params
            .lock()
            .ok()
            .and_then(|p| p.as_ref().map(|p| p.send_udp))
            .unwrap_or(false);
        if !send_udp {
            return;
        }
        if self.running.swap(true, Ordering::SeqCst) {
            if audio_enabled && !self.audio_enabled.swap(true, Ordering::AcqRel) {
                let (microphone, speaker) = self
                    .devices
                    .lock()
                    .map(|devices| devices.clone())
                    .unwrap_or_default();
                probe_audio_hardware(speaker.as_deref(), microphone.as_deref());
            }
            return;
        }
        self.audio_enabled.store(audio_enabled, Ordering::Release);
        self.last_media_rx_ms
            .store(unix_now_ms(), Ordering::Release);

        if audio_enabled {
            let (microphone, speaker) = self
                .devices
                .lock()
                .map(|devices| devices.clone())
                .unwrap_or_default();
            probe_audio_hardware(speaker.as_deref(), microphone.as_deref());
        }

        let muted = self.muted.clone();
        let speaker_off = self.speaker_off.clone();
        let running = self.running.clone();
        let socket = self.socket.clone();
        let selected_server = self.selected_server.clone();
        let keys = self.keys.clone();
        let params = self.params.clone();
        let last_ping = self.last_ping.clone();
        let tx_group_plan = self.group_video_plan.clone();
        let tx_group_wanted = self.group_video_wanted.clone();
        let tx_group_control_seq = self.group_control_seq.clone();
        let audio_config = params
            .lock()
            .ok()
            .and_then(|params| params.as_ref().map(|params| params.video_config.clone()))
            .unwrap_or(Value::Null);
        let far_active = Arc::new(AtomicBool::new(false));
        let aec_enabled = audio_config
            .get("audioEchoCancellation")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let ns_enabled = audio_config
            .get("audioNoiseSuppression")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let hpf_enabled = audio_config
            .get("audioHighPassFilter")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let apm = match Apm::new(SAMPLE_RATE as i32, 1, aec_enabled, ns_enabled, hpf_enabled) {
            Ok(apm) => {
                crate::zcall_log!(
                    "[zcall-rust] WebRTC APM enabled: AEC={aec_enabled} NS={ns_enabled} HPF={hpf_enabled}"
                );
                Some(Arc::new(apm))
            }
            Err(error) => {
                crate::zcall_log!(
                    "[zcall-rust] WebRTC APM unavailable, using SpeexDSP fallback: {error}"
                );
                None
            }
        };
        let audio_dsp = if apm.is_none() {
            let mut fallback_config = audio_config.clone();
            if !fallback_config.is_object() {
                fallback_config = serde_json::json!({});
            }
            fallback_config["audioMicGain"] = Value::Bool(false);
            match AudioProcessor::new(&fallback_config) {
                Ok(processor) => {
                    crate::zcall_log!(
                        "[zcall-rust] SpeexDSP fallback: AEC={} NS={} HPF={} (PR MicLeveler owns gain)",
                        processor.is_echo_cancellation_enabled(),
                        processor.is_noise_suppression_enabled(),
                        processor.is_high_pass_enabled()
                    );
                    Arc::new(Mutex::new(Some(processor)))
                }
                Err(error) => {
                    crate::zcall_log!("[zcall-rust] SpeexDSP fallback unavailable: {error}");
                    Arc::new(Mutex::new(None))
                }
            }
        } else {
            Arc::new(Mutex::new(None))
        };

        let bind_params = self.params.lock().ok().and_then(|params| params.clone());
        if let Some(bind_params) = bind_params.filter(|params| !params.p2p_candidates.is_empty()) {
            let bind_socket = socket.clone();
            let bind_running = running.clone();
            let bind_path = self.p2p_path.clone();
            let bind_enabled = self.p2p_enabled.clone();
            let bind_seq = self.p2p_seq.clone();
            thread::Builder::new()
                .name("zcall-p2p-bind".into())
                .spawn(move || {
                    let mut attempt_started = Instant::now();
                    let mut last_send = Instant::now() - Duration::from_millis(100);
                    let mut was_enabled = false;
                    while bind_running.load(Ordering::Relaxed) {
                        let path_is_up =
                            bind_path.lock().map(|path| path.is_some()).unwrap_or(false);
                        let enabled = bind_enabled.load(Ordering::Acquire);
                        if !enabled {
                            was_enabled = false;
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        if !was_enabled {
                            // P2P_BIND_GIVE_UP_MS = 15 s. Explicit subCmd 3
                            // or a fresh start signal begins another bind window.
                            attempt_started = Instant::now();
                            last_send = Instant::now() - Duration::from_millis(100);
                            was_enabled = true;
                        }
                        if path_is_up {
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        if attempt_started.elapsed() >= Duration::from_secs(15) {
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        let interval = if attempt_started.elapsed() < Duration::from_secs(1) {
                            Duration::from_millis(100)
                        } else {
                            Duration::from_millis(400)
                        };
                        if last_send.elapsed() >= interval {
                            last_send = Instant::now();
                            let sequence = bind_seq.fetch_add(1, Ordering::Relaxed);
                            for (index, candidate) in bind_params.p2p_candidates.iter().enumerate()
                            {
                                let packet = p2p::bind(
                                    !bind_params.is_caller,
                                    bind_params.call_id,
                                    sequence,
                                    index as u32,
                                );
                                let _ = bind_socket.send_to(&packet, candidate);
                            }
                        }
                        thread::sleep(Duration::from_millis(25));
                    }
                })
                .ok();
        }

        let params_for_playout = params.clone();
        let jitter_buffer = match JitterBuffer::new(SAMPLE_RATE as i32, 1, 10) {
            Ok(mut buffer) => {
                buffer.set_min_target(3);
                Some(Arc::new(Mutex::new(buffer)))
            }
            Err(error) => {
                crate::zcall_log!("[zcall-rust] audio jitter buffer unavailable: {error}");
                None
            }
        };
        let group_audio = Arc::new(Mutex::new(GroupAudioMixer::new()));
        spawn_playout_worker(
            running.clone(),
            self.audio_enabled.clone(),
            self.devices.clone(),
            self.device_generation.clone(),
            speaker_off.clone(),
            params_for_playout,
            jitter_buffer.clone(),
            group_audio.clone(),
            audio_dsp.clone(),
            apm.clone(),
            far_active.clone(),
            self.mic_volume.clone(),
            self.group_events.clone(),
        );

        // UDP receive thread: authenticate/depacketize and queue audio. Playback
        // is intentionally independent and follows the hardware clock.
        let rx_running = running.clone();
        let rx_audio_packets = self.rx_audio_packets.clone();
        let rx_socket = socket.clone();
        let rx_keys = keys.clone();
        let rx_fallback_keys = self.rx_fallback_keys.clone();
        let rx_selected = selected_server.clone();
        let video_frames = self.video_frames.clone();
        let rx_params = params.clone();
        let rx_rtcp = self.rtcp.clone();
        let rx_nack = self.nack.clone();
        let rx_keyframe_request = self.local_keyframe_request.clone();
        let rx_p2p_path = self.p2p_path.clone();
        let rx_p2p_enabled = self.p2p_enabled.clone();
        let rx_video_history = self.video_history.clone();
        let rx_group_events = self.group_events.clone();
        let rx_share_peer = self.share_peer.clone();
        let rx_remote_share_uid = self.remote_share_uid.clone();
        let rx_send_rate = self.send_rate.clone();
        let rx_last_media_rx_ms = self.last_media_rx_ms.clone();
        let rx_audio_enabled = self.audio_enabled.clone();
        let rx_tx_audio_packets = self.tx_audio_packets.clone();
        let rx_last_video_sent_ms = self.last_video_sent_ms.clone();
        let rx_tx_video_packets = self.tx_video_packets.clone();

        thread::spawn(move || {
            let rx_jitter_buffer = jitter_buffer;
            let rx_group_audio = group_audio;
            let mut video_assemblers = HashMap::<u32, VideoAssembler>::new();
            let mut audio_roc = RocTracker::default();
            let mut video_rocs = HashMap::<u32, RocTracker>::new();
            let mut last_video_pli = HashMap::<u32, Instant>::new();
            let mut audio_dedup = AudioDeduplicator::new(256);
            let mut member_audio_seen = HashSet::<u32>::new();
            let mut member_speaking = HashMap::<u32, MemberSpeakingTracker>::new();
            let mut last_audio_rx = Instant::now();
            let mut last_p2p_rx: Option<Instant> = None;
            let mut last_recovery = Instant::now() - Duration::from_secs(3);
            let mut local_ip_sig = local_ipv4_signature();
            let mut next_network_check = Instant::now() + Duration::from_millis(250);
            let mut change_address_retries = VecDeque::<(Instant, &'static str)>::new();
            let mut change_addr_seq = 0u32;
            let mut relay_audio_streak = 0u32;
            let mut rx_audio_second = 0u32;
            let mut early_audio_rx_count = 0u32;
            let mut group_rx_audio_packets = 0u64;
            let mut group_rx_video_packets = 0u64;
            let mut group_rx_audio_decoded = 0u64;
            let mut group_rx_video_decoded = 0u64;
            let mut group_rx_audio_bad = 0u64;
            let mut group_rx_video_bad = 0u64;
            let mut rx_video_packets = 0u64;
            let mut rx_video_decoded = 0u64;
            let mut rx_video_bad = 0u64;
            let mut rx_video_first = 0u8;
            let mut group_rx_other_host = 0u64;
            let mut group_rx_alt_port = 0u64;
            let mut group_rx_last_summary = Instant::now();
            let mut group_rx_audio_first = 0u8;
            let mut group_rx_video_first = 0u8;
            let mut group_audio_srtp_profile: Option<(usize, usize)> = None;
            let mut group_video_srtp_profile: Option<(usize, usize)> = None;
            let mut group_audio_srtp_profile_discovery_attempts = 0u8;
            let mut group_video_srtp_profile_discovery_attempts = 0u8;
            const GROUP_SRTP_PROFILE_DISCOVERY_LIMIT: u8 = 4;
            let mut group_audio_auth_reported = false;
            let mut group_audio_auth_failed_reported = false;
            let mut group_video_auth_reported = false;
            let mut group_video_auth_failed_reported = false;
            let mut group_video_frame_reported = false;
            let mut buf = [0u8; 2048];

            while rx_running.load(Ordering::Relaxed) {
                if let Ok((len, from)) = rx_socket.recv_from(&mut buf) {
                    let pkt = &buf[..len];
                    if pkt.is_empty() {
                        continue;
                    }

                    let params_snapshot = rx_params.lock().ok().and_then(|params| params.clone());
                    if params_snapshot
                        .as_ref()
                        .is_some_and(|params| params.is_group)
                        && matches!(pkt[0], msg_types::AUDIO_RTP_DOWN | msg_types::VIDEO_DOWN)
                    {
                        let selected = rx_selected
                            .lock()
                            .ok()
                            .and_then(|selected| selected.map(|(addr, _)| addr));
                        let same_host = selected.is_some_and(|selected| selected.ip() == from.ip());
                        let source = if selected == Some(from) {
                            "selected"
                        } else if same_host {
                            group_rx_alt_port = group_rx_alt_port.saturating_add(1);
                            "same-host-alt-port"
                        } else {
                            group_rx_other_host = group_rx_other_host.saturating_add(1);
                            "other-host"
                        };
                        let ssrc = (pkt.len() >= 13)
                            .then(|| u32::from_be_bytes([pkt[9], pkt[10], pkt[11], pkt[12]]));
                        if pkt[0] == msg_types::AUDIO_RTP_DOWN {
                            group_rx_audio_packets = group_rx_audio_packets.saturating_add(1);
                            if group_rx_audio_first < 3 {
                                media_debug(&format!(
                                    "udp-audio source={source} bytes={} ssrc={}",
                                    pkt.len(),
                                    ssrc.map(|uid| uid.to_string())
                                        .unwrap_or_else(|| "short".into())
                                ));
                                group_rx_audio_first += 1;
                            }
                        } else {
                            group_rx_video_packets = group_rx_video_packets.saturating_add(1);
                            if group_rx_video_first < 3 {
                                media_debug(&format!(
                                    "udp-video source={source} bytes={} ssrc={}",
                                    pkt.len(),
                                    ssrc.map(|uid| uid.to_string())
                                        .unwrap_or_else(|| "short".into())
                                ));
                                group_rx_video_first += 1;
                            }
                        }
                    }
                    let mut direct = false;
                    let (msg_type, payload) = if pkt[0] == msg_types::P2P_EXT {
                        let Some(params_snapshot) = params_snapshot.as_ref() else {
                            continue;
                        };
                        let Some((header, body)) = p2p::parse(pkt) else {
                            continue;
                        };
                        if header.call_id != params_snapshot.call_id
                            || header.is_callee != params_snapshot.is_caller
                            || (!params_snapshot.p2p_candidates.is_empty()
                                && !params_snapshot
                                    .p2p_candidates
                                    .iter()
                                    .any(|candidate| candidate.ip() == from.ip()))
                        {
                            continue;
                        }
                        last_p2p_rx = Some(Instant::now());
                        match header.kind {
                            p2p::BIND_PEER => {
                                if let Some(answer) = p2p::bind_answer(
                                    !params_snapshot.is_caller,
                                    params_snapshot.call_id,
                                    body,
                                    &format!("{}|{}", from.ip(), from.port()),
                                ) {
                                    let _ = rx_socket.send_to(&answer, from);
                                }
                                continue;
                            }
                            p2p::SIGNAL => {
                                match body.first() {
                                    Some(1) => rx_p2p_enabled.store(true, Ordering::Release),
                                    Some(2) => rx_p2p_enabled.store(false, Ordering::Release),
                                    _ => {}
                                }
                                if matches!(body.first(), Some(1 | 2)) {
                                    if let Ok(mut path) = rx_p2p_path.lock() {
                                        *path = None;
                                    }
                                    relay_audio_streak = 0;
                                }
                                continue;
                            }
                            p2p::KEEPALIVE => {
                                if matches!(body.first(), Some(1 | 2)) {
                                    // Publish the direct path before replying. The
                                    // peer may send media immediately after it sees
                                    // the pong, so the TX worker must already route
                                    // duplicate packets to this endpoint.
                                    if let Ok(mut path) = rx_p2p_path.lock() {
                                        *path = Some(from);
                                    }
                                    if body.first() == Some(&1) {
                                        if let Some(pong) =
                                            p2p::pong(!params_snapshot.is_caller, pkt)
                                        {
                                            let _ = rx_socket.send_to(&pong, from);
                                        }
                                    }
                                }
                                continue;
                            }
                            p2p::AUDIO => {
                                direct = true;
                                (msg_types::AUDIO_RTP_DOWN, body)
                            }
                            p2p::VIDEO => {
                                direct = true;
                                (msg_types::VIDEO_DOWN, body)
                            }
                            p2p::AUDIO_RTCP => (msg_types::AUDIO_FEC, body),
                            p2p::VIDEO_RTCP => (msg_types::VIDEO_FEC, body),
                            _ => continue,
                        }
                    } else {
                        let selected_from = rx_selected
                            .lock()
                            .ok()
                            .and_then(|selected| selected.map(|(addr, _)| addr));
                        let is_group = params_snapshot
                            .as_ref()
                            .is_some_and(|params| params.is_group);
                        if !accepts_relay_source(selected_from, from, is_group) {
                            continue;
                        }
                        match pkt[0] {
                            msg_types::AUDIO_RTP_DOWN | msg_types::VIDEO_DOWN => {
                                (pkt[0], &pkt[1..])
                            }
                            msg_types::AUDIO_FEC | msg_types::VIDEO_FEC => {
                                if pkt.len() < 9 {
                                    continue;
                                }
                                let token = u32::from_le_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]);
                                let expected_token = rx_selected
                                    .lock()
                                    .ok()
                                    .and_then(|selected| selected.map(|(_, token)| token));
                                if expected_token != Some(token) {
                                    continue;
                                }
                                (pkt[0], &pkt[5..])
                            }
                            other => (other, pkt),
                        }
                    };
                    if msg_type == msg_types::VIDEO_DOWN {
                        rx_video_packets = rx_video_packets.saturating_add(1);
                        if rx_video_first < 3 && payload.len() >= 12 {
                            let ssrc = u32::from_be_bytes([
                                payload[8],
                                payload[9],
                                payload[10],
                                payload[11],
                            ]);
                            let mode = if params_snapshot
                                .as_ref()
                                .is_some_and(|params| params.is_group)
                            {
                                "group"
                            } else {
                                "one-to-one"
                            };
                            media_debug(&format!(
                                "rx-video mode={mode} path={} bytes={} ssrc={}",
                                if direct { "direct" } else { "relay" },
                                payload.len(),
                                ssrc
                            ));
                            rx_video_first += 1;
                        }
                    }

                    match msg_type {
                        msg_types::AUDIO_FEC | msg_types::VIDEO_FEC => {
                            let is_video = msg_type == msg_types::VIDEO_FEC;
                            let now_ms = unix_now_ms();
                            let targets = rx_rtcp
                                .lock()
                                .ok()
                                .and_then(|mut feedback| {
                                    feedback
                                        .as_mut()
                                        .map(|feedback| feedback.on_rtcp(is_video, payload, now_ms))
                                })
                                .unwrap_or_default();
                            if let Ok(mut controller) = rx_send_rate.lock() {
                                if let Some(controller) = controller.as_mut() {
                                    controller.on_feedback(
                                        &targets.twcc_feedback,
                                        &targets.report_blocks,
                                        now_ms,
                                    );
                                }
                            }
                            let local_uid = rx_params
                                .lock()
                                .ok()
                                .and_then(|params| params.as_ref().map(|params| params.uid));
                            if local_uid.is_some_and(|uid| targets.keyframe_targets.contains(&uid))
                            {
                                rx_keyframe_request.store(true, Ordering::Relaxed);
                            }
                            if is_video {
                                let params =
                                    rx_params.lock().ok().and_then(|params| params.clone());
                                if let Some(params) = params {
                                    if let Some((relay, token)) = *rx_selected.lock().unwrap() {
                                        for nack in targets.nack_requests {
                                            if nack.media_ssrc != 0 && nack.media_ssrc != params.uid
                                            {
                                                continue;
                                            }
                                            for sequence in nack.lost_sequences {
                                                let cached = rx_video_history.lock().ok().and_then(
                                                    |history| {
                                                        history
                                                            .iter()
                                                            .rev()
                                                            .find(|(seq, _)| *seq == sequence)
                                                            .map(|(_, packet)| packet.clone())
                                                    },
                                                );
                                                if let Some(protected) = cached {
                                                    let relay_packet = wrap_media_packet(
                                                        msg_types::VIDEO_RTP,
                                                        token,
                                                        &protected,
                                                    );
                                                    let _ = rx_socket.send_to(&relay_packet, relay);
                                                    if let Some(peer) = rx_p2p_path
                                                        .lock()
                                                        .ok()
                                                        .and_then(|path| *path)
                                                    {
                                                        let direct_packet = p2p::wrap(
                                                            !params.is_caller,
                                                            params.call_id,
                                                            p2p::VIDEO,
                                                            &protected,
                                                        );
                                                        let _ =
                                                            rx_socket.send_to(&direct_packet, peer);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        msg_types::AUDIO_RTP_DOWN => {
                            // The relay and P2P path carry identical SRTP sequence
                            // spaces. Authenticate first, then deliver each SSRC/ROC/seq
                            // exactly once to the relevant jitter buffer.
                            let is_group = params_snapshot
                                .as_ref()
                                .is_some_and(|params| params.is_group);
                            if payload.len() >= 12 {
                                let rtp_body = payload;
                                let seq = u16::from_be_bytes([rtp_body[2], rtp_body[3]]);
                                let ssrc = u32::from_be_bytes([
                                    rtp_body[8],
                                    rtp_body[9],
                                    rtp_body[10],
                                    rtp_body[11],
                                ]);
                                let local_uid = params_snapshot
                                    .as_ref()
                                    .map(|params| params.uid)
                                    .unwrap_or_default();

                                if is_group {
                                    if ssrc != local_uid {
                                        let candidates = rx_group_audio
                                            .lock()
                                            .map(|mixer| mixer.candidate_rocs(ssrc, seq))
                                            .unwrap_or_else(|_| vec![0]);
                                        let mut decoded = None;
                                        let mut fallback_key_used = false;
                                        let mut auth_profile = (0usize, 10usize);
                                        let profile_to_try =
                                            group_audio_srtp_profile.or_else(|| {
                                                (group_audio_srtp_profile_discovery_attempts
                                                    >= GROUP_SRTP_PROFILE_DISCOVERY_LIMIT)
                                                    .then_some((0, 10))
                                            });
                                        if group_audio_srtp_profile.is_none()
                                            && group_audio_srtp_profile_discovery_attempts
                                                < GROUP_SRTP_PROFILE_DISCOVERY_LIMIT
                                        {
                                            group_audio_srtp_profile_discovery_attempts += 1;
                                        }
                                        if let (Ok(keys), Ok(fallback)) =
                                            (rx_keys.lock(), rx_fallback_keys.lock())
                                        {
                                            let primary = keys.as_ref();
                                            let fallback = fallback.as_ref();
                                            if let Some((
                                                clear,
                                                roc,
                                                used_fallback,
                                                mki_len,
                                                tag_len,
                                            )) = unprotect_group_with_candidates(
                                                rtp_body,
                                                primary,
                                                fallback,
                                                &candidates,
                                                profile_to_try,
                                            ) {
                                                decoded =
                                                    Some((clear.seq, clear.ssrc, roc, clear.opus));
                                                fallback_key_used = used_fallback;
                                                auth_profile = (mki_len, tag_len);
                                                group_audio_srtp_profile = Some(auth_profile);
                                            }
                                            // Preserve legacy plain-RTP mode only when
                                            // zrtcConfig.srtpKey is absent. Try the
                                            // authenticated session candidate first.
                                            if decoded.is_none() && primary.is_none() {
                                                if let Some((clear_seq, clear_ssrc, opus)) =
                                                    read_plain_rtp_payload(rtp_body)
                                                {
                                                    decoded = Some((
                                                        clear_seq,
                                                        clear_ssrc,
                                                        candidates[0],
                                                        opus,
                                                    ));
                                                }
                                            }
                                        }
                                        if let Some((clear_seq, clear_ssrc, _, opus)) =
                                            decoded.as_ref()
                                        {
                                            group_rx_audio_decoded =
                                                group_rx_audio_decoded.saturating_add(1);
                                            if !group_audio_auth_reported {
                                                media_debug(&format!(
                                                    "group-audio-auth=ok ssrc={clear_ssrc} seq={clear_seq} key_source={} mki_bytes={} tag_bytes={} opus_bytes={}",
                                                    if fallback_key_used { "session" } else { "group" },
                                                    auth_profile.0,
                                                    auth_profile.1,
                                                    opus.len()
                                                ));
                                                group_audio_auth_reported = true;
                                            }
                                        } else {
                                            group_rx_audio_bad =
                                                group_rx_audio_bad.saturating_add(1);
                                            if !group_audio_auth_failed_reported {
                                                if std::env::var("ZCALL_GROUP_AUTH_DUMP")
                                                    .is_ok_and(|value| value == "1")
                                                {
                                                    let packet_hex = rtp_body
                                                        .iter()
                                                        .map(|byte| format!("{byte:02x}"))
                                                        .collect::<String>();
                                                    media_debug(&format!(
                                                        "group-audio-auth-dump={packet_hex}"
                                                    ));
                                                }
                                                let fallback_probe = rx_fallback_keys
                                                    .lock()
                                                    .ok()
                                                    .and_then(|keys| {
                                                        keys.as_ref().map(|keys| {
                                                            probe_srtp_auth_layout(rtp_body, keys)
                                                        })
                                                    })
                                                    .unwrap_or_else(|| "no-keys".into());
                                                media_debug(&format!(
                                                    "group-audio-auth=failed ssrc={ssrc} seq={seq} srtp_keys={} fallback_probe={} rtp_bytes={}",
                                                    rx_keys.lock().map(|keys| keys.is_some()).unwrap_or(false),
                                                    fallback_probe,
                                                    rtp_body.len()
                                                ));
                                                group_audio_auth_failed_reported = true;
                                            }
                                        }
                                        if let Some((clear_seq, clear_ssrc, roc, opus)) = decoded {
                                            if rx_share_peer
                                                .lock()
                                                .ok()
                                                .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                                                == Some(clear_ssrc)
                                                || rx_remote_share_uid.load(Ordering::Acquire)
                                                    == clear_ssrc
                                            {
                                                continue;
                                            }
                                            let ext_seq = (roc << 16) | u32::from(clear_seq);
                                            last_audio_rx = Instant::now();
                                            rx_last_media_rx_ms
                                                .store(unix_now_ms(), Ordering::Release);
                                            let new_source = if direct {
                                                relay_audio_streak = 0;
                                                if let Ok(mut path) = rx_p2p_path.lock() {
                                                    *path = Some(from);
                                                }
                                                true
                                            } else {
                                                if rx_p2p_path
                                                    .lock()
                                                    .map(|path| path.is_some())
                                                    .unwrap_or(false)
                                                {
                                                    relay_audio_streak =
                                                        relay_audio_streak.saturating_add(1);
                                                    if relay_audio_streak >= 25 {
                                                        let selected = *rx_selected
                                                            .lock()
                                                            .ok()
                                                            .as_deref()
                                                            .unwrap_or(&None);
                                                        if let Some(params) =
                                                            params_snapshot.as_ref()
                                                        {
                                                            stop_p2p_path(
                                                                &rx_socket,
                                                                selected,
                                                                &rx_p2p_path,
                                                                &rx_p2p_enabled,
                                                                params,
                                                                true,
                                                            );
                                                        }
                                                        relay_audio_streak = 0;
                                                    }
                                                }
                                                false
                                            };
                                            let _ = new_source;
                                            if audio_dedup.accept(clear_ssrc, ext_seq) {
                                                rx_audio_second = rx_audio_second.saturating_add(1);
                                                rx_audio_packets.fetch_add(1, Ordering::Relaxed);
                                                if params_snapshot
                                                    .as_ref()
                                                    .is_some_and(|params| params.uid != clear_ssrc)
                                                    && member_audio_seen.insert(clear_ssrc)
                                                {
                                                    if let Ok(mut events) = rx_group_events.lock() {
                                                        events.push_back(
                                                            GroupMediaEvent::MemberAudio(
                                                                clear_ssrc,
                                                            ),
                                                        );
                                                    }
                                                }
                                                if let Some(level) =
                                                    parse_rfc6464_audio_level(rtp_body)
                                                {
                                                    if let Some(on) = member_speaking
                                                        .entry(clear_ssrc)
                                                        .or_default()
                                                        .observe(level, Instant::now())
                                                    {
                                                        if let Ok(mut events) =
                                                            rx_group_events.lock()
                                                        {
                                                            events.push_back(
                                                                GroupMediaEvent::MemberSpeaking(
                                                                    clear_ssrc, on,
                                                                ),
                                                            );
                                                        }
                                                    }
                                                }
                                                if let Ok(mut mixer) = rx_group_audio.lock() {
                                                    mixer.push_packet(
                                                        clear_ssrc, clear_seq, roc, opus,
                                                    );
                                                }
                                                if let Ok(mut feedback) = rx_rtcp.lock() {
                                                    if let Some(feedback) = feedback.as_mut() {
                                                        feedback.on_rtp(
                                                            false,
                                                            rtp_body,
                                                            unix_now_ms(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    let mut decoded = None;
                                    if let Ok(keys) = rx_keys.lock() {
                                        if let Some(keys) = keys.as_ref() {
                                            decoded = unprotect_with_roc_recovery(
                                                rtp_body,
                                                keys,
                                                &mut audio_roc,
                                            )
                                            .map(|(clear, roc)| {
                                                (clear.seq, clear.ssrc, roc, clear.opus)
                                            });
                                        } else if let Some((clear_seq, clear_ssrc, opus)) =
                                            read_plain_rtp_payload(rtp_body)
                                        {
                                            let roc = audio_roc.guess(clear_seq);
                                            audio_roc.update(clear_seq, roc);
                                            decoded = Some((clear_seq, clear_ssrc, roc, opus));
                                        }
                                    }
                                    if let Some((clear_seq, clear_ssrc, roc, opus)) = decoded {
                                        if rx_share_peer
                                            .lock()
                                            .ok()
                                            .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                                            == Some(clear_ssrc)
                                            || rx_remote_share_uid.load(Ordering::Acquire)
                                                == clear_ssrc
                                        {
                                            continue;
                                        }
                                        let ext_seq = (roc << 16) | u32::from(clear_seq);
                                        last_audio_rx = Instant::now();
                                        if direct {
                                            relay_audio_streak = 0;
                                            if let Ok(mut path) = rx_p2p_path.lock() {
                                                *path = Some(from);
                                            }
                                        } else if rx_p2p_path
                                            .lock()
                                            .map(|path| path.is_some())
                                            .unwrap_or(false)
                                        {
                                            relay_audio_streak =
                                                relay_audio_streak.saturating_add(1);
                                            if relay_audio_streak >= 25 {
                                                let selected = *rx_selected.lock().unwrap();
                                                if let Some(params) = params_snapshot.as_ref() {
                                                    stop_p2p_path(
                                                        &rx_socket,
                                                        selected,
                                                        &rx_p2p_path,
                                                        &rx_p2p_enabled,
                                                        params,
                                                        true,
                                                    );
                                                }
                                                relay_audio_streak = 0;
                                            }
                                        }

                                        if audio_dedup.accept(clear_ssrc, ext_seq) {
                                            rx_last_media_rx_ms
                                                .store(unix_now_ms(), Ordering::Release);
                                            rx_audio_second = rx_audio_second.saturating_add(1);
                                            rx_audio_packets.fetch_add(1, Ordering::Relaxed);
                                            let early_caller =
                                                params_snapshot.as_ref().is_some_and(|params| {
                                                    params.is_caller && !params.is_group
                                                });
                                            if early_caller
                                                && !rx_audio_enabled.load(Ordering::Acquire)
                                            {
                                                early_audio_rx_count =
                                                    early_audio_rx_count.saturating_add(1);
                                                if let Some(phase) = early_audio_phase(
                                                    true,
                                                    false,
                                                    false,
                                                    early_audio_rx_count,
                                                ) {
                                                    if let Ok(mut events) = rx_group_events.lock() {
                                                        events.push_back(
                                                            GroupMediaEvent::EarlyAudio(phase),
                                                        );
                                                    }
                                                }
                                            }
                                            if let Ok(mut feedback) = rx_rtcp.lock() {
                                                if let Some(feedback) = feedback.as_mut() {
                                                    feedback.on_rtp(false, rtp_body, unix_now_ms());
                                                }
                                            }
                                            if let Some(jitter) = rx_jitter_buffer.as_ref() {
                                                if let Ok(mut jitter) = jitter.lock() {
                                                    jitter.push(ext_seq, opus);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        msg_types::VIDEO_DOWN => {
                            if payload.len() >= 12 {
                                let rtp_body = payload;
                                let ssrc = u32::from_be_bytes([
                                    rtp_body[8],
                                    rtp_body[9],
                                    rtp_body[10],
                                    rtp_body[11],
                                ]);
                                if params_snapshot
                                    .as_ref()
                                    .is_some_and(|params| params.is_group && params.uid == ssrc)
                                    && !std::env::var("ZCALL_CAM_LOOPBACK")
                                        .is_ok_and(|value| value == "1")
                                {
                                    continue;
                                }
                                let local_share_uid = rx_share_peer
                                    .lock()
                                    .ok()
                                    .and_then(|peer| peer.as_ref().map(|peer| peer.uid));
                                let is_local_share = local_share_uid == Some(ssrc);
                                let is_share = is_local_share
                                    || rx_remote_share_uid.load(Ordering::Acquire) == ssrc;
                                if is_local_share
                                    && !std::env::var("ZCALL_SHARE_LOOPBACK")
                                        .is_ok_and(|value| value == "1")
                                {
                                    continue;
                                }
                                let marker = (rtp_body[1] & 0x80) != 0;
                                let pt = rtp_body[1] & 0x7f;
                                let is_group_video = params_snapshot
                                    .as_ref()
                                    .is_some_and(|params| params.is_group);
                                if is_group_video || pt == VIDEO_PT_KEY || pt == VIDEO_PT_INTER {
                                    let mut auth_profile = (0usize, 10usize);
                                    let profile_to_try = group_video_srtp_profile.or_else(|| {
                                        (group_video_srtp_profile_discovery_attempts
                                            >= GROUP_SRTP_PROFILE_DISCOVERY_LIMIT)
                                            .then_some((0, 10))
                                    });
                                    if is_group_video
                                        && group_video_srtp_profile.is_none()
                                        && group_video_srtp_profile_discovery_attempts
                                            < GROUP_SRTP_PROFILE_DISCOVERY_LIMIT
                                    {
                                        group_video_srtp_profile_discovery_attempts += 1;
                                    }
                                    let (decoded, fallback_key_used) = if is_share {
                                        let tracker = video_rocs.entry(ssrc).or_default();
                                        let decoded = read_plain_rtp_payload(rtp_body).map(
                                            |(clear_seq, clear_ssrc, opus)| {
                                                let roc = tracker.guess(clear_seq);
                                                tracker.update(clear_seq, roc);
                                                (clear_seq, clear_ssrc, roc, opus)
                                            },
                                        );
                                        (decoded, false)
                                    } else {
                                        let mut fallback_key_used = false;
                                        let mut decoded = None;
                                        if let (Ok(keys), Ok(fallback)) =
                                            (rx_keys.lock(), rx_fallback_keys.lock())
                                        {
                                            let tracker = video_rocs.entry(ssrc).or_default();
                                            let seq =
                                                u16::from_be_bytes([rtp_body[2], rtp_body[3]]);
                                            let rocs = tracker.candidates(seq);
                                            if let Some((
                                                clear,
                                                roc,
                                                used_fallback,
                                                mki_len,
                                                tag_len,
                                            )) = unprotect_group_with_candidates(
                                                rtp_body,
                                                keys.as_ref(),
                                                fallback.as_ref(),
                                                &rocs,
                                                profile_to_try,
                                            ) {
                                                tracker.update(seq, roc);
                                                decoded =
                                                    Some((clear.seq, clear.ssrc, roc, clear.opus));
                                                fallback_key_used = used_fallback;
                                                auth_profile = (mki_len, tag_len);
                                                group_video_srtp_profile = Some(auth_profile);
                                            }
                                            // No configured primary SRTP key means
                                            // legacy plain-RTP mode; retain it after
                                            // the optional session candidate fails.
                                            if decoded.is_none() && keys.is_none() {
                                                decoded = read_plain_rtp_payload(rtp_body).map(
                                                    |(clear_seq, clear_ssrc, opus)| {
                                                        let roc = tracker.guess(clear_seq);
                                                        tracker.update(clear_seq, roc);
                                                        (clear_seq, clear_ssrc, roc, opus)
                                                    },
                                                );
                                            }
                                        }
                                        (decoded, fallback_key_used)
                                    };
                                    if let Some((clear_seq, clear_ssrc, _, data)) = decoded.as_ref()
                                    {
                                        rx_video_decoded = rx_video_decoded.saturating_add(1);
                                        if is_group_video {
                                            group_rx_video_decoded =
                                                group_rx_video_decoded.saturating_add(1);
                                            if !group_video_auth_reported {
                                                media_debug(&format!(
                                                    "group-video-auth=ok ssrc={clear_ssrc} seq={clear_seq} pt={pt} marker={marker} key_source={} mki_bytes={} tag_bytes={} payload_bytes={}",
                                                    if fallback_key_used { "session" } else { "group" },
                                                    auth_profile.0,
                                                    auth_profile.1,
                                                    data.len()
                                                ));
                                                group_video_auth_reported = true;
                                            }
                                        }
                                    } else {
                                        rx_video_bad = rx_video_bad.saturating_add(1);
                                        if is_group_video {
                                            group_rx_video_bad =
                                                group_rx_video_bad.saturating_add(1);
                                            if !group_video_auth_failed_reported {
                                                // Opt-in, one-packet capture for diagnosing SFU-side
                                                // SRTP/header mutation. Never log key material or payload by default.
                                                if std::env::var("ZCALL_GROUP_AUTH_DUMP")
                                                    .is_ok_and(|value| value == "1")
                                                {
                                                    let packet_hex = rtp_body
                                                        .iter()
                                                        .map(|byte| format!("{byte:02x}"))
                                                        .collect::<String>();
                                                    media_debug(&format!(
                                                        "group-video-auth-dump={packet_hex}"
                                                    ));
                                                }
                                                let header_len =
                                                    parse_rtp_header_len(rtp_body).unwrap_or(0);
                                                let header_hex = rtp_body
                                                    .get(..header_len.min(32))
                                                    .unwrap_or_default()
                                                    .iter()
                                                    .map(|byte| format!("{byte:02x}"))
                                                    .collect::<String>();
                                                let auth_fp = rx_keys
                                                    .lock()
                                                    .ok()
                                                    .and_then(|keys| {
                                                        keys.as_ref()
                                                            .map(|keys| sha1(&[&keys.auth]))
                                                    })
                                                    .map(|digest| {
                                                        digest[..6]
                                                            .iter()
                                                            .map(|byte| format!("{byte:02x}"))
                                                            .collect::<String>()
                                                    })
                                                    .unwrap_or_else(|| "none".into());
                                                let auth_probe = rx_keys
                                                    .lock()
                                                    .ok()
                                                    .and_then(|keys| {
                                                        keys.as_ref().map(|keys| {
                                                            probe_srtp_auth_layout(rtp_body, keys)
                                                        })
                                                    })
                                                    .unwrap_or_else(|| "no-keys".into());
                                                let fallback_probe = rx_fallback_keys
                                                    .lock()
                                                    .ok()
                                                    .and_then(|keys| {
                                                        keys.as_ref().map(|keys| {
                                                            probe_srtp_auth_layout(rtp_body, keys)
                                                        })
                                                    })
                                                    .unwrap_or_else(|| "no-keys".into());
                                                let plain_h264_candidate = read_plain_rtp_payload(
                                                    rtp_body,
                                                )
                                                .is_some_and(|(_, _, payload)| {
                                                    looks_like_group_h264_payload(&payload)
                                                });
                                                media_debug(&format!(
                                                    "group-video-auth=failed ssrc={ssrc} seq={} pt={pt} marker={marker} plain_h264_candidate={plain_h264_candidate} srtp_keys={} auth_fp={} auth_probe={} fallback_probe={} rtp_bytes={} rtp_header_bytes={header_len} rtp_header={header_hex}",
                                                    u16::from_be_bytes([rtp_body[2], rtp_body[3]]),
                                                    rx_keys.lock().map(|keys| keys.is_some()).unwrap_or(false),
                                                    auth_fp,
                                                    auth_probe,
                                                    fallback_probe,
                                                    rtp_body.len()
                                                ));
                                                group_video_auth_failed_reported = true;
                                            }
                                        }
                                    }
                                    if let Some((clear_seq, clear_ssrc, _roc, data)) = decoded {
                                        rx_last_media_rx_ms.store(unix_now_ms(), Ordering::Release);
                                        if direct {
                                            if let Ok(mut path) = rx_p2p_path.lock() {
                                                *path = Some(from);
                                            }
                                        }
                                        let now_ms = unix_now_ms();
                                        if let Ok(mut feedback) = rx_rtcp.lock() {
                                            if let Some(feedback) = feedback.as_mut() {
                                                feedback.on_rtp(true, rtp_body, now_ms);
                                            }
                                        }
                                        if let Ok(mut tracker) = rx_nack.lock() {
                                            let requests =
                                                tracker.note(clear_ssrc, clear_seq, now_ms);
                                            if !requests.is_empty() {
                                                let selected = *rx_selected.lock().unwrap();
                                                if let Some((server, token)) = selected {
                                                    if let Some(params) =
                                                        rx_params.lock().unwrap().clone()
                                                    {
                                                        for (media_ssrc, missing) in requests {
                                                            let report = build_nack_many(
                                                                params.uid, media_ssrc, &missing,
                                                            );
                                                            let packet = wrap_media_packet(
                                                                msg_types::VIDEO_FEC,
                                                                token,
                                                                &report,
                                                            );
                                                            let _ =
                                                                rx_socket.send_to(&packet, server);
                                                            if let Some(peer) = rx_p2p_path
                                                                .lock()
                                                                .ok()
                                                                .and_then(|path| *path)
                                                            {
                                                                let direct_packet = p2p::wrap(
                                                                    !params.is_caller,
                                                                    params.call_id,
                                                                    p2p::VIDEO_RTCP,
                                                                    &report,
                                                                );
                                                                let _ = rx_socket
                                                                    .send_to(&direct_packet, peer);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        let is_group = is_group_video;
                                        if is_group || pt == VIDEO_PT_KEY || pt == VIDEO_PT_INTER {
                                            let timestamp = u32::from_be_bytes([
                                                rtp_body[4],
                                                rtp_body[5],
                                                rtp_body[6],
                                                rtp_body[7],
                                            ]);
                                            let is_new_video_source = is_group
                                                && !is_share
                                                && !video_assemblers.contains_key(&clear_ssrc);
                                            let assembler =
                                                video_assemblers.entry(clear_ssrc).or_insert_with(
                                                    || VideoAssembler::with_group(is_group),
                                                );
                                            if is_new_video_source {
                                                if let Ok(mut events) = rx_group_events.lock() {
                                                    events.push_back(GroupMediaEvent::MemberVideo(
                                                        clear_ssrc,
                                                    ));
                                                }
                                            }
                                            let frames = assembler.push_packet(
                                                clear_seq, _roc, timestamp, &data, marker, pt,
                                            );
                                            if is_group
                                                && !frames.is_empty()
                                                && !group_video_frame_reported
                                            {
                                                let frame = &frames[0];
                                                media_debug(&format!(
                                                    "group-video-frame=assembled ssrc={clear_ssrc} frames={} key={} bytes={}",
                                                    frames.len(),
                                                    frame.key,
                                                    frame.data.len()
                                                ));
                                                group_video_frame_reported = true;
                                            }
                                            let request_keyframe =
                                                assembler.take_keyframe_request();
                                            if request_keyframe {
                                                let now = Instant::now();
                                                let can_request = last_video_pli
                                                    .get(&clear_ssrc)
                                                    .is_none_or(|last| {
                                                        now.duration_since(*last)
                                                            >= Duration::from_secs(1)
                                                    });
                                                if can_request {
                                                    last_video_pli.insert(clear_ssrc, now);
                                                    let report = rx_rtcp.lock().ok().and_then(
                                                        |mut feedback| {
                                                            feedback.as_mut().map(|feedback| {
                                                                feedback.pli(clear_ssrc)
                                                            })
                                                        },
                                                    );
                                                    if let Some(report) = report {
                                                        if let Some((server, token)) =
                                                            *rx_selected.lock().unwrap()
                                                        {
                                                            let packet = wrap_media_packet(
                                                                msg_types::VIDEO_FEC,
                                                                token,
                                                                &report,
                                                            );
                                                            let _ =
                                                                rx_socket.send_to(&packet, server);
                                                            if let Some(peer) = rx_p2p_path
                                                                .lock()
                                                                .ok()
                                                                .and_then(|path| *path)
                                                            {
                                                                if let Some(params) =
                                                                    rx_params.lock().ok().and_then(
                                                                        |params| params.clone(),
                                                                    )
                                                                {
                                                                    let direct_packet = p2p::wrap(
                                                                        !params.is_caller,
                                                                        params.call_id,
                                                                        p2p::VIDEO_RTCP,
                                                                        &report,
                                                                    );
                                                                    let _ = rx_socket.send_to(
                                                                        &direct_packet,
                                                                        peer,
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            if let Ok(mut queue) = video_frames.lock() {
                                                for mut frame in frames {
                                                    if is_group {
                                                        frame.src = Some(clear_ssrc);
                                                    }
                                                    if queue.len() >= 8 {
                                                        queue.pop_front();
                                                    }
                                                    queue.push_back(frame);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        msg_types::SERVER_REPLY => {
                            if let Some(reply) = parse_server_reply(pkt) {
                                let selected_from = rx_selected
                                    .lock()
                                    .ok()
                                    .and_then(|selected| selected.map(|(addr, _)| addr));
                                let group_uid = rx_params.lock().ok().and_then(|params| {
                                    params.as_ref().filter(|p| p.is_group).map(|p| p.uid)
                                });
                                if selected_from == Some(from) || group_uid.is_some() {
                                    if let Some(local_uid) = group_uid {
                                        if let Some(event) =
                                            group_event_from_reply(&reply, local_uid)
                                        {
                                            if let Ok(mut events) = rx_group_events.lock() {
                                                events.push_back(event);
                                                while events.len() > 128 {
                                                    events.pop_front();
                                                }
                                            }
                                        }
                                    }
                                    if reply.cmd == crate::protocol::zrtp::commands::CHANGE_ADDRESS
                                    {
                                        crate::zcall_log!(
                                            "[zcall-rust] relay acknowledged ChangeAddress"
                                        );
                                    } else if reply.cmd
                                        == crate::protocol::zrtp::commands::REQ_FORWARD
                                        && matches!(reply.sub_cmd, 2 | 3)
                                    {
                                        if let Ok(mut path) = rx_p2p_path.lock() {
                                            *path = None;
                                        }
                                        rx_p2p_enabled.store(reply.sub_cmd == 3, Ordering::Release);
                                        last_p2p_rx = None;
                                        relay_audio_streak = 0;
                                    }
                                    let mut selected = rx_selected.lock().unwrap();
                                    if let Some((addr, _)) = *selected {
                                        if addr == from {
                                            *selected = Some((
                                                from,
                                                reply.server_token.unwrap_or(reply.token),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let recovery_now = Instant::now();
                if recovery_now >= next_network_check {
                    next_network_check = recovery_now + Duration::from_millis(500);
                    let current_signature = local_ipv4_signature();
                    let network_changed =
                        !current_signature.is_empty() && current_signature != local_ip_sig;
                    if !current_signature.is_empty() {
                        local_ip_sig = current_signature;
                    }

                    let params_now = rx_params.lock().ok().and_then(|params| params.clone());
                    let selected_now = *rx_selected.lock().unwrap();

                    if network_changed {
                        if let Some(params) = params_now.as_ref() {
                            crate::zcall_log!(
                                "[zcall-rust] local IPv4 changed ({}); stopping P2P and sending ChangeAddress",
                                local_ip_sig
                            );
                            stop_p2p_path(
                                &rx_socket,
                                selected_now,
                                &rx_p2p_path,
                                &rx_p2p_enabled,
                                params,
                                true,
                            );
                            send_change_address(&rx_socket, selected_now, params, change_addr_seq);
                            change_addr_seq = change_addr_seq.wrapping_add(1);
                            change_address_retries.push_back((
                                recovery_now + Duration::from_secs(1),
                                "local-ip-change retry 1",
                            ));
                            change_address_retries.push_back((
                                recovery_now + Duration::from_millis(2500),
                                "local-ip-change retry 2",
                            ));
                            last_recovery = recovery_now;
                        }
                    } else if rx_p2p_path
                        .lock()
                        .map(|path| path.is_some())
                        .unwrap_or(false)
                        && last_p2p_rx.is_some_and(|last| {
                            recovery_now.duration_since(last) > Duration::from_millis(2500)
                        })
                    {
                        if let Some(params) = params_now.as_ref() {
                            crate::zcall_log!(
                                "[zcall-rust] P2P receive timed out; requesting relay fallback"
                            );
                            stop_p2p_path(
                                &rx_socket,
                                selected_now,
                                &rx_p2p_path,
                                &rx_p2p_enabled,
                                params,
                                true,
                            );
                            send_change_address(&rx_socket, selected_now, params, change_addr_seq);
                            change_addr_seq = change_addr_seq.wrapping_add(1);
                            relay_audio_streak = 0;
                            last_recovery = recovery_now;
                        }
                    } else if !params_now.as_ref().is_some_and(|params| params.is_group)
                        && recovery_now.duration_since(last_audio_rx) > Duration::from_secs(2)
                        && recovery_now.duration_since(last_audio_rx) < Duration::from_secs(20)
                        && recovery_now.duration_since(last_recovery) > Duration::from_millis(2500)
                    {
                        if let Some(params) = params_now.as_ref() {
                            crate::zcall_log!(
                                "[zcall-rust] audio receive stalled; requesting relay fallback and ChangeAddress"
                            );
                            stop_p2p_path(
                                &rx_socket,
                                selected_now,
                                &rx_p2p_path,
                                &rx_p2p_enabled,
                                params,
                                true,
                            );
                            send_change_address(&rx_socket, selected_now, params, change_addr_seq);
                            change_addr_seq = change_addr_seq.wrapping_add(1);
                            relay_audio_streak = 0;
                            last_recovery = recovery_now;
                        }
                    }
                }

                let speaking_now = Instant::now();
                for (uid, tracker) in &mut member_speaking {
                    if tracker.expire(speaking_now) {
                        if let Ok(mut events) = rx_group_events.lock() {
                            events.push_back(GroupMediaEvent::MemberSpeaking(*uid, false));
                        }
                    }
                }

                while change_address_retries
                    .front()
                    .is_some_and(|(due, _)| recovery_now >= *due)
                {
                    if let Some((_, reason)) = change_address_retries.pop_front() {
                        if let Some(params) =
                            rx_params.lock().ok().and_then(|params| params.clone())
                        {
                            let selected = *rx_selected.lock().unwrap();
                            crate::zcall_log!("[zcall-rust] sending {reason}");
                            send_change_address(&rx_socket, selected, &params, change_addr_seq);
                            change_addr_seq = change_addr_seq.wrapping_add(1);
                        }
                    }
                }

                let summary_mode = rx_params.lock().ok().and_then(|params| {
                    params.as_ref().map(|params| {
                        if params.is_group {
                            "group"
                        } else {
                            "one-to-one"
                        }
                    })
                });
                if let Some(mode) = summary_mode {
                    if group_rx_last_summary.elapsed() >= Duration::from_secs(5) {
                        media_debug(&format!(
                            "summary mode={mode} rx_video={} decoded_video={} bad_video={} tx_video={} tx_audio={} rx_audio_delivered={} group_rx_audio={} group_decoded_audio={} group_bad_audio={} group_rx_video={} group_decoded_video={} group_bad_video={} other_host={} same_host_alt_port={} last_video_tx_ms={}",
                            rx_video_packets,
                            rx_video_decoded,
                            rx_video_bad,
                            rx_tx_video_packets.load(Ordering::Relaxed),
                            rx_tx_audio_packets.load(Ordering::Relaxed),
                            rx_audio_packets.load(Ordering::Relaxed),
                            group_rx_audio_packets,
                            group_rx_audio_decoded,
                            group_rx_audio_bad,
                            group_rx_video_packets,
                            group_rx_video_decoded,
                            group_rx_video_bad,
                            group_rx_other_host,
                            group_rx_alt_port,
                            rx_last_video_sent_ms.load(Ordering::Relaxed)
                        ));
                        group_rx_last_summary = Instant::now();
                    }
                }

                if let Ok(mut tracker) = rx_nack.lock() {
                    let requests = tracker.poll(unix_now_ms());
                    if !requests.is_empty() {
                        let selected = *rx_selected.lock().unwrap();
                        if let Some((server, token)) = selected {
                            let local_uid = rx_params
                                .lock()
                                .unwrap()
                                .as_ref()
                                .map(|params| params.uid)
                                .unwrap_or_default();
                            for (ssrc, missing) in requests {
                                let report = build_nack_many(local_uid, ssrc, &missing);
                                let packet =
                                    wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
                                let _ = rx_socket.send_to(&packet, server);
                                if let Some(peer) = rx_p2p_path.lock().ok().and_then(|path| *path) {
                                    let params = rx_params.lock().unwrap().clone();
                                    if let Some(params) = params {
                                        let direct_packet = p2p::wrap(
                                            !params.is_caller,
                                            params.call_id,
                                            p2p::VIDEO_RTCP,
                                            &report,
                                        );
                                        let _ = rx_socket.send_to(&direct_packet, peer);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        // Audio recording & transmit thread
        let tx_running = running.clone();
        let tx_audio_packets = self.tx_audio_packets.clone();
        let tx_socket = socket.clone();
        let tx_keys = keys.clone();
        let tx_params = params.clone();
        let tx_selected = selected_server.clone();
        let tx_muted = muted.clone();
        let tx_twcc = self.twcc_seq.clone();
        let tx_devices = self.devices.clone();
        let tx_device_generation = self.device_generation.clone();
        let tx_rtcp = self.rtcp.clone();
        let tx_p2p_path = self.p2p_path.clone();
        let tx_rate = self.send_rate.clone();
        let tx_last_video = self.last_video_sent_ms.clone();
        let tx_pending_profile = self.pending_video_profile.clone();
        let tx_audio_dsp = audio_dsp.clone();
        let tx_apm = apm.clone();
        let tx_far_active = far_active.clone();
        let tx_mic_volume = self.mic_volume.clone();
        let tx_mic_volume_dirty = self.mic_volume_dirty.clone();
        let tx_mic_manual = self.mic_manual.clone();
        let tx_pending_mic_volume = self.pending_mic_volume.clone();
        let tx_mic_level = self.mic_level.clone();
        let tx_mic_level_dirty = self.mic_level_dirty.clone();
        let tx_audio_enabled = self.audio_enabled.clone();
        let tx_media_events = self.group_events.clone();

        thread::spawn(move || {
            let mut recorder: Option<PulseRecorder> = None;
            let mut mic_fault_reported = false;
            let initial_microphone = tx_devices
                .lock()
                .map(|devices| devices.0.clone())
                .unwrap_or(None);
            let mut clip_guard = ClipGuard::new(initial_microphone.clone(), tx_mic_volume.clone())
                .with_controls(Some(tx_far_active.clone()), tx_mic_manual.clone());
            clip_guard.initialize_device();
            let mut guard_device = initial_microphone;
            let mut leveler = MicLeveler::new(Some(tx_far_active.clone()));
            let mic_gain_enabled =
                std::env::var("ZCALL_MIC_GAIN").map_or(true, |value| value != "0");
            let mut last_volume_reported = tx_mic_volume.load(Ordering::Relaxed);
            let mut active_device_generation = u64::MAX;
            let mut last_mic_reopen = Instant::now() - Duration::from_secs(1);
            let mut last_level_at = Instant::now() - Duration::from_millis(100);
            let mut encoder = match OpusEncoder::new(SAMPLE_RATE as i32, 1, 32000) {
                Ok(encoder) => Some(encoder),
                Err(err) => {
                    crate::zcall_log!("[zcall-rust] Opus encoder unavailable: {err}");
                    None
                }
            };
            let mut opus_buf = vec![0u8; 1024];
            let mut pcm_buf = vec![0u8; FRAME_BYTES];

            let mut seq: u16 = 0;
            let mut audio_roc: u32 = 0;
            let mut ts: u32 = 0;
            let mut ping_seq: u32 = 0;
            let mut last_twcc = Instant::now();
            let mut last_report = Instant::now();
            let mut audio_packets = 0u32;
            let mut audio_octets = 0u32;

            while tx_running.load(Ordering::Relaxed) {
                if let Ok(mut pending) = tx_pending_mic_volume.lock() {
                    if let Some(percent) = pending.take() {
                        if percent > 0 {
                            let _ = set_source_mute(guard_device.as_deref(), false);
                            pulse_normalize_own_stream();
                        }
                        clip_guard.mark_manual_write();
                        if set_source_volume(guard_device.as_deref(), percent).is_ok() {
                            let actual = get_source_volume(guard_device.as_deref())
                                .map(|value| value.min(100))
                                .unwrap_or(percent);
                            tx_mic_volume.store(actual, Ordering::Relaxed);
                            crate::zcall_log!("[zcall-rust] mic volume: {actual}%");
                        } else {
                            tx_mic_volume.store(percent, Ordering::Relaxed);
                            crate::zcall_log!(
                                "[zcall-rust] warning: failed to apply manual mic volume"
                            );
                        }
                        last_volume_reported = tx_mic_volume.load(Ordering::Relaxed);
                        tx_mic_volume_dirty.store(true, Ordering::Release);
                    }
                }

                let device_generation = tx_device_generation.load(Ordering::Acquire);
                if device_generation != active_device_generation {
                    let microphone = tx_devices
                        .lock()
                        .map(|devices| devices.0.clone())
                        .unwrap_or(None);
                    let audio_enabled = tx_audio_enabled.load(Ordering::Acquire);
                    let switch_result = if audio_enabled {
                        PulseRecorder::new(microphone.as_deref()).map(Some)
                    } else if pulse_get_source_info(microphone.as_deref()).is_some() {
                        Ok(None)
                    } else {
                        Err("selected microphone source could not be resolved".to_owned())
                    };

                    match switch_result {
                        Ok(new_recorder) => {
                            recorder = new_recorder;
                            if audio_enabled {
                                // The capture stream must exist before normalizing its
                                // source-output volume/mute state in PulseAudio/PipeWire.
                                pulse_normalize_own_stream();
                            }
                            if microphone != guard_device {
                                clip_guard.set_device(microphone.clone());
                                clip_guard.initialize_device();
                                guard_device = microphone;
                                tx_mic_manual.store(false, Ordering::Release);
                                leveler = MicLeveler::new(Some(tx_far_active.clone()));
                                last_volume_reported = tx_mic_volume.load(Ordering::Relaxed);
                                tx_mic_volume_dirty.store(true, Ordering::Release);
                            }
                            mic_fault_reported = false;
                        }
                        Err(error) => {
                            crate::zcall_log!(
                                "[zcall-rust] microphone switch rejected; keeping previous device: {error}"
                            );
                            // Roll back the requested selection as well as retaining the
                            // previous recorder and ClipGuard target.
                            if let Ok(mut devices) = tx_devices.lock() {
                                devices.0 = guard_device.clone();
                            }
                            if !mic_fault_reported {
                                if let Ok(mut events) = tx_media_events.lock() {
                                    events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                                }
                                mic_fault_reported = true;
                            }
                        }
                    }
                    active_device_generation = device_generation;
                    last_mic_reopen = Instant::now();
                }
                if recorder.is_none()
                    && tx_audio_enabled.load(Ordering::Acquire)
                    && last_mic_reopen.elapsed() >= Duration::from_millis(500)
                {
                    last_mic_reopen = Instant::now();
                    let microphone = tx_devices
                        .lock()
                        .map(|devices| devices.0.clone())
                        .unwrap_or(None);
                    match PulseRecorder::new(microphone.as_deref()) {
                        Ok(reopened) => {
                            crate::zcall_log!("[zcall-rust] Pulse microphone stream recovered");
                            recorder = Some(reopened);
                            pulse_normalize_own_stream();
                        }
                        Err(error) => {
                            if !mic_fault_reported {
                                if let Ok(mut events) = tx_media_events.lock() {
                                    events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                                }
                                mic_fault_reported = true;
                            }
                            crate::zcall_log!(
                                "[zcall-rust] Pulse microphone reopen failed: {error}"
                            );
                        }
                    }
                }
                if clip_guard.sync_os_volume() {
                    if let Ok(reopened) = PulseRecorder::new(None) {
                        recorder = Some(reopened);
                        pulse_normalize_own_stream();
                        clip_guard.initialize_device();
                        leveler = MicLeveler::new(Some(tx_far_active.clone()));
                        last_volume_reported = tx_mic_volume.load(Ordering::Relaxed);
                        tx_mic_volume_dirty.store(true, Ordering::Release);
                    }
                }

                // Read 20 ms PCM from mic
                let mut recorder_failed = false;
                if let Some(ref mut r) = recorder {
                    if r.read(&mut pcm_buf).is_ok() {
                        let mut pcm_s16: Vec<i16> = pcm_buf
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| i16::from_le_bytes(*pair))
                            .collect();
                        if tx_muted.load(Ordering::Relaxed) || clip_guard.is_os_muted() {
                            pcm_s16.fill(0);
                        } else {
                            // ClipGuard measures unprocessed hardware PCM before APM.
                            clip_guard.feed(&pcm_s16);
                            if let Some(apm) = tx_apm.as_ref() {
                                apm.process_capture(&mut pcm_s16);
                            } else if let Ok(mut processor) = tx_audio_dsp.lock() {
                                if let Some(processor) = processor.as_mut() {
                                    processor.capture(&mut pcm_s16);
                                }
                            }
                            if mic_gain_enabled {
                                leveler.process(&mut pcm_s16);
                            }
                        }
                        apply_soft_limiter(&mut pcm_s16);

                        let level = compute_rfc6464_level(&pcm_s16);
                        if last_level_at.elapsed() >= Duration::from_millis(100) {
                            tx_mic_level.store(level, Ordering::Relaxed);
                            tx_mic_level_dirty.store(true, Ordering::Release);
                            last_level_at = Instant::now();
                        }
                        let current_volume = tx_mic_volume.load(Ordering::Relaxed);
                        if current_volume != last_volume_reported {
                            last_volume_reported = current_volume;
                            tx_mic_volume_dirty.store(true, Ordering::Release);
                        }

                        if let Some(ref mut enc) = encoder {
                            if let Ok(encoded_len) = enc.encode(&pcm_s16, &mut opus_buf) {
                                let opus_data = &opus_buf[..encoded_len];

                                let (server_opt, p_opt, k_opt) = {
                                    let s = *tx_selected.lock().unwrap();
                                    let p = tx_params.lock().unwrap().as_ref().map(|p| {
                                        (p.uid, p.send_udp, p.call_id, p.is_caller, p.is_group)
                                    });
                                    let k = tx_keys
                                        .lock()
                                        .unwrap()
                                        .as_ref()
                                        .map(|k| (k.enc, k.auth, k.salt));
                                    (s, p, k)
                                };

                                if let (
                                    Some((server, token)),
                                    Some((uid, send_udp, call_id, is_caller, is_group)),
                                ) = (server_opt, p_opt)
                                {
                                    if send_udp && (is_group || k_opt.is_some()) {
                                        let tw_seq = tx_twcc.fetch_add(1, Ordering::Relaxed);
                                        if let Ok(mut controller) = tx_rate.lock() {
                                            if let Some(controller) = controller.as_mut() {
                                                controller.on_sent(tw_seq, unix_now_ms());
                                            }
                                        }
                                        let rtp_header = if is_group {
                                            build_group_audio_rtp_header(
                                                seq,
                                                ts,
                                                uid,
                                                tw_seq,
                                                false,
                                                112,
                                                unix_now_ms(),
                                                level,
                                            )
                                        } else {
                                            build_audio_rtp_header(seq, ts, uid, tw_seq, false, 112)
                                                .to_vec()
                                        };
                                        let media_payload =
                                            if let Some((enc_k, auth_k, salt_k)) = k_opt {
                                                let keys = SrtpKeys {
                                                    enc: enc_k,
                                                    auth: auth_k,
                                                    salt: salt_k,
                                                };
                                                protect_rtp(
                                                    &rtp_header,
                                                    opus_data,
                                                    &keys,
                                                    seq,
                                                    uid,
                                                    audio_roc,
                                                )
                                            } else {
                                                let mut plain = rtp_header;
                                                plain.extend_from_slice(opus_data);
                                                plain
                                            };
                                        let media_pkt = wrap_media_packet(
                                            msg_types::AUDIO_RTP,
                                            token,
                                            &media_payload,
                                        );
                                        let _ = tx_socket.send_to(&media_pkt, server);
                                        if !is_group {
                                            if let Some(peer) =
                                                tx_p2p_path.lock().ok().and_then(|path| *path)
                                            {
                                                let direct = p2p::wrap(
                                                    !is_caller,
                                                    call_id,
                                                    p2p::AUDIO,
                                                    &media_payload,
                                                );
                                                let _ = tx_socket.send_to(&direct, peer);
                                            }
                                        }
                                        audio_packets = audio_packets.wrapping_add(1);
                                        tx_audio_packets.fetch_add(1, Ordering::Relaxed);
                                        audio_octets =
                                            audio_octets.wrapping_add(encoded_len as u32);
                                        seq = seq.wrapping_add(1);
                                        if seq == 0 {
                                            audio_roc = audio_roc.wrapping_add(1);
                                        }
                                        ts = ts.wrapping_add(320);
                                    }
                                }
                            }
                        }
                    } else {
                        if !mic_fault_reported {
                            if let Ok(mut events) = tx_media_events.lock() {
                                events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                            }
                            mic_fault_reported = true;
                        }
                        recorder_failed = true;
                        thread::sleep(Duration::from_millis(5));
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
                if recorder_failed {
                    recorder = None;
                    last_mic_reopen = Instant::now();
                }

                // Zalo's send-side bandwidth estimator depends on regular
                // transport-wide feedback. Audio RTCP carries TWCC + SR + SDES;
                // video RTCP carries RR + SDES when we have received video.
                if last_twcc.elapsed() >= Duration::from_millis(50) {
                    last_twcc = Instant::now();
                    let selected = *tx_selected.lock().unwrap();
                    if let Some((server, token)) = selected {
                        let reports = tx_rtcp
                            .lock()
                            .ok()
                            .and_then(|mut feedback| {
                                feedback.as_mut().map(RtcpFeedback::twcc_packets)
                            })
                            .unwrap_or_default();
                        for report in reports {
                            let packet = wrap_media_packet(msg_types::AUDIO_FEC, token, &report);
                            let _ = tx_socket.send_to(&packet, server);
                            if let Some(peer) = tx_p2p_path.lock().ok().and_then(|path| *path) {
                                if let Some(params) =
                                    tx_params.lock().ok().and_then(|params| params.clone())
                                {
                                    let direct = p2p::wrap(
                                        !params.is_caller,
                                        params.call_id,
                                        p2p::AUDIO_RTCP,
                                        &report,
                                    );
                                    let _ = tx_socket.send_to(&direct, peer);
                                }
                            }
                        }
                    }
                }
                if last_report.elapsed() >= Duration::from_secs(1) {
                    last_report = Instant::now();
                    let selected = *tx_selected.lock().unwrap();
                    if let Some((server, token)) = selected {
                        let reports = tx_rtcp.lock().ok().and_then(|mut feedback| {
                            let feedback = feedback.as_mut()?;
                            let now = unix_now_ms();
                            let downlink_loss = feedback.downlink_audio_loss();
                            let audio = feedback.audio_report(now, ts, audio_packets, audio_octets);
                            let video = feedback.video_report(now);
                            Some((audio, video, downlink_loss))
                        });
                        if let Some((audio_report, video_report, downlink_loss)) = reports {
                            let now = unix_now_ms();
                            let sending_video =
                                now.saturating_sub(tx_last_video.load(Ordering::Relaxed)) < 1_500;
                            let changed = tx_rate.lock().ok().and_then(|mut controller| {
                                controller.as_mut().and_then(|controller| {
                                    controller.tick(now, sending_video, downlink_loss)
                                })
                            });
                            if let Some(profile) = changed {
                                if let Ok(mut pending) = tx_pending_profile.lock() {
                                    *pending = Some(profile);
                                }
                            }
                            let packet =
                                wrap_media_packet(msg_types::AUDIO_FEC, token, &audio_report);
                            let _ = tx_socket.send_to(&packet, server);
                            if let Some(peer) = tx_p2p_path.lock().ok().and_then(|path| *path) {
                                if let Some(params) =
                                    tx_params.lock().ok().and_then(|params| params.clone())
                                {
                                    let direct = p2p::wrap(
                                        !params.is_caller,
                                        params.call_id,
                                        p2p::AUDIO_RTCP,
                                        &audio_report,
                                    );
                                    let _ = tx_socket.send_to(&direct, peer);
                                }
                            }
                            if let Some(video_report) = video_report {
                                let packet =
                                    wrap_media_packet(msg_types::VIDEO_FEC, token, &video_report);
                                let _ = tx_socket.send_to(&packet, server);
                                if let Some(peer) = tx_p2p_path.lock().ok().and_then(|path| *path) {
                                    if let Some(params) =
                                        tx_params.lock().ok().and_then(|params| params.clone())
                                    {
                                        let direct = p2p::wrap(
                                            !params.is_caller,
                                            params.call_id,
                                            p2p::VIDEO_RTCP,
                                            &video_report,
                                        );
                                        let _ = tx_socket.send_to(&direct, peer);
                                    }
                                }
                            }
                        }
                    }
                }

                // Send 6-second keepalive ping
                if last_ping.lock().unwrap().elapsed() >= Duration::from_secs(6) {
                    *last_ping.lock().unwrap() = Instant::now();
                    let (server_opt, p_opt) = {
                        let s = *tx_selected.lock().unwrap();
                        let p = tx_params
                            .lock()
                            .unwrap()
                            .as_ref()
                            .map(|p| (p.uid, p.sess_id.clone(), p.is_group, p.video_enabled));
                        (s, p)
                    };
                    if let (Some((server, token)), Some((uid, sess_id, is_group, video_on))) =
                        (server_opt, p_opt)
                    {
                        let ping_pkt = if is_group {
                            let group_camera_enabled = !std::env::var("ZCALL_GROUP_CAMERA")
                                .is_ok_and(|value| value == "0");
                            // The camera state is independent of SRTP mode. The
                            // encrypted group path still sends camera RTP; gating
                            // this bit on `group_plain` made the SFU advertise us
                            // as camera-off while packets were being transmitted.
                            let ping_video_on =
                                group_camera_advertised(video_on, group_camera_enabled);
                            let seq = next_group_control_sequence(&tx_group_control_seq);
                            media_debug(&format!(
                                "periodic-zavi-ping seq={seq} video_on={ping_video_on}"
                            ));
                            build_zavi_ping_packet(uid, token, seq, &sess_id, ping_video_on)
                        } else {
                            ping_seq = ping_seq.wrapping_add(1);
                            build_ping_packet(uid, token, ping_seq, &sess_id)
                        };
                        let _ = tx_socket.send_to(&ping_pkt, server);

                        if is_group {
                            let plan = tx_group_plan.lock().ok().and_then(|plan| plan.clone());
                            if let Some(plan) = plan.filter(|plan| !plan.send.is_empty()) {
                                let seq = next_group_control_sequence(&tx_group_control_seq);
                                let packet =
                                    build_group_video_quality_config(uid, token, seq, &plan.send);
                                let _ = tx_socket.send_to(&packet, server);
                            }
                            let entries = tx_group_wanted
                                .lock()
                                .map(|wanted| {
                                    wanted
                                        .iter()
                                        .map(|(uid, quality_id)| GroupVideoQualityRequest {
                                            uid: *uid,
                                            quality_id: if std::env::var("ZCALL_GROUP_QUALITY")
                                                .is_ok_and(|value| value == "0")
                                            {
                                                0xff
                                            } else {
                                                *quality_id
                                            },
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            if !entries.is_empty() {
                                let seq = next_group_control_sequence(&tx_group_control_seq);
                                let packet =
                                    build_group_video_quality_request(uid, token, seq, &entries);
                                let _ = tx_socket.send_to(&packet, server);
                            }
                        }
                    }
                }
            }
        });
    }

    fn finish_ring_selection(&self) {
        let servers = self
            .ring_servers
            .lock()
            .map(|mut servers| std::mem::take(&mut *servers))
            .unwrap_or_default();
        if servers.is_empty() {
            return;
        }
        let params = self.params.lock().ok().and_then(|params| params.clone());
        let selected = self.selected_server.lock().ok().and_then(|server| *server);
        let Some(params) = params else {
            return;
        };
        for (server, token) in servers {
            let packet = if selected == Some((server, token)) {
                if params.is_group {
                    // The group SFU expects its dedicated ZaviPing control
                    // message immediately after InitZRTP. The legacy
                    // _sendPing() never used the one-to-one PING command here.
                    let camera_enabled =
                        !std::env::var("ZCALL_GROUP_CAMERA").is_ok_and(|value| value == "0");
                    let video_on = group_camera_advertised(params.video_enabled, camera_enabled);
                    let seq = next_group_control_sequence(&self.group_control_seq);
                    build_zavi_ping_packet(params.uid, token, seq, &params.sess_id, video_on)
                } else {
                    build_ping_packet(params.uid, token, 0, &params.sess_id)
                }
            } else {
                build_end_call_packet(params.uid, token, 0, params.call_id)
            };
            let _ = self.socket.send_to(&packet, server);
        }
        if params.is_group {
            if let Ok(mut last_ping) = self.last_ping.lock() {
                *last_ping = Instant::now();
            }
            media_debug("initial-keepalive type=zavi-ping");
        }
    }

    fn start_ring_ping(&self) {
        self.start_ring_ping_with_interval(ring_ping_interval());
    }

    fn start_ring_ping_with_interval(&self, interval: Duration) {
        let candidates = self
            .ring_servers
            .lock()
            .map(|servers| servers.clone())
            .unwrap_or_default();
        if candidates.is_empty() {
            return;
        }
        self.ring_ping_running.store(true, Ordering::SeqCst);
        let active = self.ring_ping_running.clone();
        let socket = self.socket.clone();
        let params = self.params.clone();
        thread::spawn(move || {
            let mut seq = 0u32;
            while active.load(Ordering::Acquire) {
                thread::sleep(interval);
                if !active.load(Ordering::Acquire) {
                    break;
                }
                let Some((uid, sess_id)) = params.lock().ok().and_then(|params| {
                    params
                        .as_ref()
                        .map(|params| (params.uid, params.sess_id.clone()))
                }) else {
                    continue;
                };
                for (server, token) in &candidates {
                    let packet = build_ping_packet(uid, *token, seq, &sess_id);
                    let _ = socket.send_to(&packet, server);
                }
                seq = seq.wrapping_add(1);
            }
        });
    }

    pub fn public_address(&self) -> Option<String> {
        self.public_addr.lock().ok().and_then(|addr| addr.clone())
    }

    pub fn note_debug_event(&self, message: &str) {
        media_debug(message);
    }

    /// Send one encoded H.264 Annex-B frame through the active relay. Group
    /// calls use the SFU RTP extension and plain RTP when the negotiated group
    /// session has no SRTP key, matching the existing Zalo group transport.
    pub fn send_video_frame(&self, key: bool, annex_b: &[u8]) -> Result<usize, String> {
        if !self.running.load(Ordering::Relaxed) || annex_b.is_empty() {
            if key && !annex_b.is_empty() {
                media_debug("video keyframe dropped: media backend is not running");
            }
            return Ok(0);
        }
        let params_guard = self.params.lock().map_err(|e| e.to_string())?;
        let params = params_guard.as_ref().ok_or("no active media session")?;
        if !params.video_enabled {
            if key {
                media_debug("video keyframe dropped: media session video_enabled=false");
            }
            return Ok(0);
        }
        let (server, token) = self
            .selected_server
            .lock()
            .map_err(|e| e.to_string())?
            .ok_or("no selected media relay")?;
        let keys_guard = self.keys.lock().map_err(|e| e.to_string())?;
        let keys = keys_guard.as_ref();
        if params.is_group && !params.video_enabled {
            return Ok(0);
        }
        let group_plain = params.is_group
            && keys.is_none()
            && !std::env::var("ZCALL_GROUP_CAMERA").is_ok_and(|value| value == "0");
        if keys.is_none() && !group_plain {
            if key {
                media_debug("video keyframe dropped: SRTP keys are not ready");
            }
            return Ok(0);
        }
        let plan = if params.is_group {
            self.group_video_plan
                .lock()
                .map_err(|e| e.to_string())?
                .clone()
        } else {
            None
        };
        let chunks = packetize_frame(annex_b, 1068);
        let mut state = self.video_tx.lock().map_err(|e| e.to_string())?;
        if !key && !state.key_sent {
            self.local_keyframe_request.store(true, Ordering::Release);
            return Ok(0);
        }
        if key {
            state.key_sent = true;
        }
        let started = *state.started_at.get_or_insert_with(Instant::now);
        let timestamp = started.elapsed().as_millis().saturating_mul(90) as u32;
        let capture_ms = unix_now_ms();
        let payload_type = if key { VIDEO_PT_KEY } else { VIDEO_PT_INTER };
        let mut sent = 0usize;

        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 && index % 4 == 0 {
                thread::sleep(Duration::from_millis(1));
            }
            let seq = state.seq;
            let roc = state.roc;
            let twcc = self.twcc_seq.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut controller) = self.send_rate.lock() {
                if let Some(controller) = controller.as_mut() {
                    controller.on_sent(twcc, capture_ms);
                }
            }
            let header = if group_plain {
                if let Some(plan) = plan.as_ref().filter(|plan| plan.sfu_id.is_some()) {
                    state.layer_seq = state.layer_seq.wrapping_add(1);
                    build_group_video_rtp_header(
                        seq,
                        timestamp,
                        params.uid,
                        twcc,
                        index + 1 == chunks.len(),
                        payload_type,
                        plan.sfu_long,
                        plan.sfu_id.unwrap_or(0),
                        state.layer_seq,
                        capture_ms,
                        unix_now_ms(),
                    )
                } else {
                    build_audio_rtp_header(
                        seq,
                        timestamp,
                        params.uid,
                        twcc,
                        index + 1 == chunks.len(),
                        payload_type,
                    )
                    .to_vec()
                }
            } else {
                build_audio_rtp_header(
                    seq,
                    timestamp,
                    params.uid,
                    twcc,
                    index + 1 == chunks.len(),
                    payload_type,
                )
                .to_vec()
            };
            let packet_payload = if group_plain {
                let mut payload = header;
                payload.extend_from_slice(chunk);
                payload
            } else {
                protect_rtp(
                    &header,
                    chunk,
                    keys.expect("SRTP keys required for encrypted video"),
                    seq,
                    params.uid,
                    roc,
                )
            };
            if let Ok(mut history) = self.video_history.lock() {
                history.push_back((seq, packet_payload.clone()));
                while history.len() > 512 {
                    history.pop_front();
                }
            }
            let packet = wrap_media_packet(msg_types::VIDEO_RTP, token, &packet_payload);
            self.socket
                .send_to(&packet, server)
                .map_err(|err| format!("video RTP send failed: {err}"))?;
            self.last_video_sent_ms.store(capture_ms, Ordering::Relaxed);
            let video_packet_count = self.tx_video_packets.fetch_add(1, Ordering::Relaxed) + 1;
            if video_packet_count == 1 {
                media_debug(&format!(
                    "tx-video-first mode={} key={} rtp_bytes={} packet_type={}",
                    if params.is_group {
                        "group"
                    } else {
                        "one-to-one"
                    },
                    key,
                    packet_payload.len(),
                    msg_types::VIDEO_RTP
                ));
            }
            if !params.is_group {
                if let Some(peer) = self.p2p_path.lock().ok().and_then(|path| *path) {
                    let direct = p2p::wrap(
                        !params.is_caller,
                        params.call_id,
                        p2p::VIDEO,
                        &packet_payload,
                    );
                    let _ = self.socket.send_to(&direct, peer);
                }
            }
            sent += 1;

            state.seq = state.seq.wrapping_add(1);
            if state.seq == 0 {
                state.roc = state.roc.wrapping_add(1);
            }
        }
        Ok(sent)
    }

    pub fn video_profile(&self) -> Option<VideoProfile> {
        if let Some(plan) = self
            .group_video_plan
            .lock()
            .ok()
            .and_then(|plan| plan.clone())
        {
            return Some(VideoProfile {
                rung: plan.start as u32 + 1,
                width: plan.encode.width,
                height: plan.encode.height,
                fps: plan.encode.fps,
                bitrate: plan.encode.bitrate,
                keyframe_interval_ms: plan.encode.keyframe_interval_ms,
            });
        }
        self.send_rate
            .lock()
            .ok()
            .and_then(|controller| controller.as_ref().map(SendRateController::profile))
    }

    /// Update the local group-camera bit carried in InitZRTP/ZaviPing.
    pub fn set_group_camera(&self, on: bool) -> Result<(), String> {
        let mut params = self.params.lock().map_err(|e| e.to_string())?;
        let Some(params) = params.as_mut() else {
            return Ok(());
        };
        if params.is_group {
            params.video_enabled = on;
        }
        Ok(())
    }

    /// Announce the local SFU layer that this process is actually encoding.
    pub fn send_group_video_quality_config(&self) -> Result<bool, String> {
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if !params.is_group {
            return Ok(false);
        }
        let Some(plan) = self
            .group_video_plan
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
        else {
            return Ok(false);
        };
        if plan.send.is_empty() {
            return Ok(false);
        }
        let Some((server, token)) = *self.selected_server.lock().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let seq = next_group_control_sequence(&self.group_control_seq);
        let packet = build_group_video_quality_config(params.uid, token, seq, &plan.send);
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("group VidQualConfig send failed: {error}"))?;
        Ok(true)
    }

    /// Request one remote member's SFU video layer.
    pub fn request_group_video(&self, uid: u32) -> Result<bool, String> {
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if !params.is_group || uid == 0 || uid == params.uid {
            return Ok(false);
        }
        let width = self
            .group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .get(&uid)
            .copied()
            .unwrap_or(0);
        let quality_id = if std::env::var("ZCALL_GROUP_QUALITY").is_ok_and(|value| value == "0") {
            0xff
        } else if std::env::var("ZCALL_SHARE_LOOPBACK").is_ok_and(|value| value == "1")
            && self
                .share_peer
                .lock()
                .ok()
                .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                == Some(uid)
        {
            0
        } else {
            self.group_video_plan
                .lock()
                .map_err(|e| e.to_string())?
                .as_ref()
                .map(|plan| select_group_video_quality(&plan.layers, width))
                .unwrap_or(0xff)
        };
        {
            let mut wanted = self.group_video_wanted.lock().map_err(|e| e.to_string())?;
            if wanted.get(&uid) == Some(&quality_id) {
                return Ok(false);
            }
            wanted.insert(uid, quality_id);
        }
        let Some((server, token)) = *self.selected_server.lock().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let seq = next_group_control_sequence(&self.group_control_seq);
        let packet = build_group_video_quality_request(
            params.uid,
            token,
            seq,
            &[GroupVideoQualityRequest { uid, quality_id }],
        );
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("group VidQualReq send failed: {error}"))?;
        media_debug(&format!(
            "subscribe-video uid={} quality_id={quality_id}",
            uid
        ));
        Ok(true)
    }

    /// Track the current share SSRC so its plain RTP is not mistaken for SRTP.
    pub fn set_group_share_remote_uid(&self, share_uid: u32) {
        self.remote_share_uid.store(share_uid, Ordering::Release);
    }

    /// Open the dedicated SFU peer used by group screen sharing.
    pub fn start_group_share(&self, share_uid: u32) -> Result<(), String> {
        if share_uid == 0 {
            return Err("invalid group share UID".into());
        }
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if !params.is_group || !params.send_udp || !self.running.load(Ordering::Acquire) {
            return Err("group screen share requires an active UDP group call".into());
        }
        let (server, _) = self
            .selected_server
            .lock()
            .map_err(|e| e.to_string())?
            .ok_or("no selected group SFU")?;
        self.stop_group_share();

        let socket = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .map_err(|error| format!("bind group share UDP socket: {error}"))?,
        );
        socket
            .set_nonblocking(true)
            .map_err(|error| format!("set group share socket nonblocking: {error}"))?;
        let active = Arc::new(AtomicBool::new(true));
        let plan = self
            .group_video_plan
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        let max_shr_screen_br = params
            .video_config
            .get("maxShrScreenBr")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .unwrap_or(1500.0)
            .trunc() as u32;
        let (sfu_id, sfu_long) = plan
            .and_then(|plan| plan.sfu_id.map(|id| (id, plan.sfu_long)))
            .unwrap_or((12, false));
        let init = build_share_init_zrtp_packet(share_uid, &params.sess_id);
        socket
            .send_to(&init, server)
            .map_err(|error| format!("send group share InitZRTP: {error}"))?;

        let peer = GroupSharePeer {
            socket: socket.clone(),
            server,
            uid: share_uid,
            token: 0,
            sess_id: params.sess_id.clone(),
            call_id: params.call_id,
            active: active.clone(),
            seq: (unix_now_ms() as u16) & 0x7fff,
            twcc_seq: 1,
            layer_seq: 0,
            control_seq: 0,
            started_at: None,
            timestamp_base: (unix_now_ms() as u32)
                .wrapping_mul(90)
                .wrapping_add(share_uid.rotate_left(7)),
            last_ping: Instant::now(),
            width: 0,
            height: 0,
            max_shr_screen_br,
            sfu_id,
            sfu_long,
            key_sent: false,
            keyframe_requested: false,
            queue: VecDeque::new(),
        };
        *self.share_peer.lock().map_err(|e| e.to_string())? = Some(peer);

        let share_state = self.share_peer.clone();
        let share_events = self.group_events.clone();
        let running = self.running.clone();
        thread::Builder::new()
            .name("zcall-group-share".into())
            .spawn(move || {
                group_share_worker(socket, share_state, share_events, running, active, init);
            })
            .map_err(|error| {
                self.stop_group_share();
                format!("spawn group share worker: {error}")
            })?;
        if std::env::var("ZCALL_SHARE_LOOPBACK").is_ok_and(|value| value == "1") {
            let _ = self.request_group_video(share_uid);
        }
        Ok(())
    }

    /// Stop only the share peer; the group camera/audio peer remains active.
    pub fn stop_group_share(&self) {
        let peer = self.share_peer.lock().ok().and_then(|mut peer| peer.take());
        if let Some(mut peer) = peer {
            peer.active.store(false, Ordering::Release);
            peer.queue.clear();
            if peer.token != 0 {
                let end =
                    build_end_call_packet(peer.uid, peer.token, peer.control_seq, peer.call_id);
                let _ = peer.socket.send_to(&end, peer.server);
            }
        }
    }

    /// Queue one encoded screen frame on the share peer, never on the camera SSRC.
    pub fn send_group_share_frame(
        &self,
        key: bool,
        annex_b: &[u8],
        width: u32,
        height: u32,
    ) -> Result<usize, String> {
        if annex_b.is_empty() || width < 2 || height < 2 {
            return Ok(0);
        }
        let mut guard = self.share_peer.lock().map_err(|e| e.to_string())?;
        let Some(peer) = guard.as_mut() else {
            if key {
                media_debug("group video keyframe dropped: share peer is not initialized");
            }
            return Ok(0);
        };
        if peer.token == 0 {
            if key {
                media_debug("group video keyframe dropped: share peer token is not ready");
            }
            return Ok(0);
        }
        let width = width.min(u16::MAX as u32) as u16;
        let height = height.min(u16::MAX as u32) as u16;
        if peer.width != width || peer.height != height {
            peer.width = width;
            peer.height = height;
            let layer = GroupVideoLayer {
                id: 0,
                bitrate_kbps: peer.max_shr_screen_br,
                width,
                height,
            };
            let packet =
                build_group_video_quality_config(peer.uid, peer.token, peer.control_seq, &[layer]);
            peer.control_seq = peer.control_seq.wrapping_add(1);
            let _ = peer.socket.send_to(&packet, peer.server);
        }
        if !key && !peer.key_sent {
            if !peer.keyframe_requested {
                peer.keyframe_requested = true;
                if let Ok(mut events) = self.group_events.lock() {
                    events.push_back(GroupMediaEvent::ShareKeyframeRequested);
                }
            }
            return Ok(0);
        }
        if key {
            peer.key_sent = true;
            peer.keyframe_requested = false;
        }
        let started = *peer.started_at.get_or_insert_with(Instant::now);
        let timestamp = peer
            .timestamp_base
            .wrapping_add(started.elapsed().as_millis().saturating_mul(90) as u32);
        let capture_ms = unix_now_ms();
        let chunks = packetize_frame(annex_b, 1068);
        for (index, chunk) in chunks.iter().enumerate() {
            let twcc = peer.twcc_seq;
            peer.layer_seq = peer.layer_seq.wrapping_add(1);
            let header = build_group_video_rtp_header(
                peer.seq,
                timestamp,
                peer.uid,
                twcc,
                index + 1 == chunks.len(),
                if key { VIDEO_PT_KEY } else { VIDEO_PT_INTER },
                peer.sfu_long,
                peer.sfu_id,
                peer.layer_seq,
                capture_ms,
                unix_now_ms(),
            );
            let mut payload = header;
            payload.extend_from_slice(chunk);
            peer.queue.push_back(wrap_media_packet(
                msg_types::VIDEO_RTP,
                peer.token,
                &payload,
            ));
            peer.seq = peer.seq.wrapping_add(1);
            peer.twcc_seq = peer.twcc_seq.wrapping_add(1);
        }
        while peer.queue.len() > 512 {
            peer.queue.pop_front();
        }
        Ok(chunks.len())
    }

    /// Re-evaluate a member's requested quality after its UI tile is resized.
    pub fn set_group_member_render_width(&self, uid: u32, width: u32) -> Result<bool, String> {
        if uid == 0 {
            return Ok(false);
        }
        self.group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .insert(uid, width);
        let already_receiving = self
            .group_video_wanted
            .lock()
            .map_err(|e| e.to_string())?
            .contains_key(&uid);
        if !already_receiving {
            return Ok(false);
        }
        self.request_group_video(uid)
    }

    pub fn take_video_profile_change(&self) -> Option<VideoProfile> {
        self.pending_video_profile
            .lock()
            .ok()
            .and_then(|mut profile| profile.take())
    }

    pub fn send_rate_totals(&self) -> Option<RateTotals> {
        self.send_rate
            .lock()
            .ok()
            .and_then(|controller| controller.as_ref().map(SendRateController::totals))
    }

    /// Authenticated audio RTP packet totals, matching the legacy call-finish payload.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.rx_audio_packets.load(Ordering::Acquire),
            self.tx_audio_packets.load(Ordering::Acquire),
        )
    }

    pub fn take_local_keyframe_request(&self) -> bool {
        self.local_keyframe_request.swap(false, Ordering::AcqRel)
    }

    pub fn request_peer_keyframe(&self, media_ssrc: Option<u32>) -> Result<bool, String> {
        let now = Instant::now();
        {
            let mut last = self
                .last_peer_pli
                .lock()
                .map_err(|error| error.to_string())?;
            if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(1)) {
                return Ok(false);
            }
            *last = Some(now);
        }
        let mut feedback = self.rtcp.lock().map_err(|error| error.to_string())?;
        let Some(feedback) = feedback.as_mut() else {
            return Ok(false);
        };
        let Some(peer_ssrc) = media_ssrc
            .filter(|ssrc| *ssrc != 0)
            .or_else(|| feedback.video.ssrc())
            .or_else(|| (feedback.peer_ssrc != 0).then_some(feedback.peer_ssrc))
        else {
            return Ok(false);
        };
        let Some((server, token)) = *self
            .selected_server
            .lock()
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let report = feedback.pli(peer_ssrc);
        let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("send RTCP PLI failed: {error}"))?;
        if let Some(peer) = self.p2p_path.lock().ok().and_then(|path| *path) {
            if let Some(params) = self.params.lock().ok().and_then(|params| params.clone()) {
                let direct = p2p::wrap(!params.is_caller, params.call_id, p2p::VIDEO_RTCP, &report);
                let _ = self.socket.send_to(&direct, peer);
            }
        }
        Ok(true)
    }

    pub fn drain_group_events(&self) -> Vec<GroupMediaEvent> {
        self.group_events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn drain_video_frames(&self) -> Vec<VideoFrame> {
        let frames = self
            .video_frames
            .lock()
            .map(|mut queue| queue.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        if !frames.is_empty()
            && !self
                .first_video_ui_drain_logged
                .swap(true, Ordering::AcqRel)
        {
            let frame = &frames[0];
            media_debug(&format!(
                "group-video-frame=sent-to-ui src={:?} key={} bytes={}",
                frame.src,
                frame.key,
                frame.data.len()
            ));
        }
        frames
    }

    pub fn set_p2p_candidates(&self, candidates: Vec<SocketAddr>) -> Result<(), String> {
        let mut params = self.params.lock().map_err(|error| error.to_string())?;
        let Some(params) = params.as_mut() else {
            return Err("cannot set P2P candidates before media preparation".into());
        };
        params.p2p_candidates = candidates;
        self.p2p_enabled
            .store(!params.p2p_candidates.is_empty(), Ordering::Release);
        Ok(())
    }

    pub fn p2p_candidates(&self, include_local: bool) -> Vec<serde_json::Value> {
        let mut candidates = Vec::new();
        if let Some(public) = self.public_address() {
            if let Some((ip, port)) = public.split_once('|') {
                if let Ok(port) = port.parse::<u16>() {
                    candidates.push(serde_json::json!({"ip":ip,"port":port,"type":1}));
                }
            }
        }
        if include_local {
            candidates.extend(self.local_candidates());
        }
        candidates
    }

    pub fn local_candidates(&self) -> Vec<serde_json::Value> {
        let port = self.socket.local_addr().map(|a| a.port()).unwrap_or(0);
        if port == 0 {
            return Vec::new();
        }
        local_ipv4_addresses()
            .into_iter()
            .map(|ip| serde_json::json!({"ip":ip.to_string(),"port":port,"type":0}))
            .collect()
    }

    pub fn set_mute(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_speaker_off(&self, off: bool) {
        self.speaker_off.store(off, Ordering::Relaxed);
    }

    /// User-selected hardware microphone volume; a manual adjustment disables
    /// automatic ClipGuard changes for the remainder of the call.
    pub fn set_mic_volume(&self, percent: u8) {
        let percent = percent.min(100);
        self.mic_manual.store(true, Ordering::Relaxed);
        if let Ok(mut pending) = self.pending_mic_volume.lock() {
            *pending = Some(percent);
        }
    }

    pub fn set_mic_auto_gain(&self, enabled: bool) {
        self.mic_manual.store(!enabled, Ordering::Release);
    }

    pub fn mic_auto_gain(&self) -> bool {
        !self.mic_manual.load(Ordering::Acquire)
    }

    pub fn mic_volume(&self) -> u8 {
        self.mic_volume.load(Ordering::Relaxed).min(100)
    }

    pub fn take_mic_volume_update(&self) -> Option<u8> {
        self.mic_volume_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| self.mic_volume())
    }

    pub fn take_mic_level_update(&self) -> Option<u8> {
        self.mic_level_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| self.mic_level.load(Ordering::Relaxed).min(127))
    }

    /// Active PulseSimple streams are reopened on their owning worker thread.
    /// The media session, SRTP state and jitter buffer are retained.
    pub fn set_device(&self, kind: &str, name: &str) {
        if let Ok(mut devices) = self.devices.lock() {
            let selected = if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            };
            let slot = match kind {
                "mic" => &mut devices.0,
                "speaker" => &mut devices.1,
                _ => return,
            };
            if *slot != selected {
                *slot = selected;
                self.device_generation.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    pub fn selected_devices(&self) -> (Option<String>, Option<String>) {
        self.devices
            .lock()
            .map(|devices| devices.clone())
            .unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop_group_share();
        self.ring_ping_running.store(false, Ordering::SeqCst);
        self.audio_enabled.store(false, Ordering::Release);
        self.running.store(false, Ordering::SeqCst);
        if let Ok(mut queue) = self.video_frames.lock() {
            queue.clear();
        }
        if let Ok(mut history) = self.video_history.lock() {
            history.clear();
        }
        if let Ok(mut state) = self.video_tx.lock() {
            *state = VideoTxState::default();
        }
        if let Ok(mut feedback) = self.rtcp.lock() {
            *feedback = None;
        }
        if let Ok(mut nack) = self.nack.lock() {
            nack.clear();
        }
        if let Ok(mut path) = self.p2p_path.lock() {
            *path = None;
        }
        self.p2p_enabled.store(false, Ordering::Release);
        if let Ok(mut controller) = self.send_rate.lock() {
            *controller = None;
        }
        if let Ok(mut pending) = self.pending_video_profile.lock() {
            *pending = None;
        }

        let params = self.params.lock().ok().and_then(|p| p.clone());
        let selected = self.selected_server.lock().ok().and_then(|s| *s);
        let pending = self
            .ring_servers
            .lock()
            .map(|mut servers| std::mem::take(&mut *servers))
            .unwrap_or_default();
        if let Some(params) = params.as_ref() {
            for (server, token) in pending {
                if selected != Some((server, token)) {
                    let end = build_end_call_packet(params.uid, token, 1, params.call_id);
                    let _ = self.socket.send_to(&end, server);
                }
            }
        }
        if let (Some(params), Some((server, token))) = (params, selected) {
            if params.send_udp && token != 0 {
                let end_packet = build_end_call_packet(params.uid, token, 1, params.call_id);
                let _ = self.socket.send_to(&end_packet, server);
            }
        }
        if let Ok(mut selected) = self.selected_server.lock() {
            *selected = None;
        }
        if let Ok(mut servers) = self.ring_servers.lock() {
            servers.clear();
        }
        if let Ok(mut public) = self.public_addr.lock() {
            *public = None;
        }
    }
}

#[cfg(test)]
mod audio_dedup_tests {
    use super::*;

    #[test]
    fn stats_expose_legacy_rx_and_tx_audio_packet_totals() {
        let backend = ZrtcMediaBackend::new().unwrap();
        backend.rx_audio_packets.store(17, Ordering::Relaxed);
        backend.tx_audio_packets.store(23, Ordering::Relaxed);
        assert_eq!(backend.stats(), (17, 23));
    }

    #[test]
    fn early_audio_phases_are_only_emitted_for_receive_only_one_to_one_callers() {
        assert_eq!(early_audio_phase(true, false, false, 1), Some("first"));
        assert_eq!(early_audio_phase(true, false, false, 50), Some("connect"));
        assert_eq!(early_audio_phase(true, false, false, 2), None);
        assert_eq!(early_audio_phase(false, false, false, 1), None);
        assert_eq!(early_audio_phase(true, true, false, 1), None);
        assert_eq!(early_audio_phase(true, false, true, 1), None);
        assert_eq!(early_audio_phase(true, false, true, 50), None);
    }

    #[test]
    fn rejects_relay_and_p2p_duplicates_but_evicts_old_entries() {
        let mut dedup = AudioDeduplicator::new(3);
        assert!(dedup.accept(10, 100));
        assert!(!dedup.accept(10, 100));
        assert!(dedup.accept(20, 100));
        assert!(dedup.accept(10, 101));
        assert_eq!(dedup.len(), 3);
        assert!(dedup.accept(30, 1));
        assert_eq!(dedup.len(), 3);
        assert!(dedup.accept(10, 100)); // oldest key was evicted
    }
}

#[cfg(test)]
mod roc_recovery_tests {
    use super::*;
    use crate::crypto::srtp::{protect_rtp, session_keys};
    use crate::protocol::rtp::build_audio_rtp_header;

    #[test]
    fn recovers_authenticated_packet_after_peer_resets_roc_to_zero() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let mut sender = RocTracker::default();
        sender.update(65_530, 0);
        sender.update(10, 1);
        let header = build_audio_rtp_header(11, 320, 77, 3, false, 112);
        let protected = protect_rtp(&header, b"opus", &keys, 11, 77, 0);

        let mut receiver = RocTracker {
            last: Some(65_000),
            roc: 2,
        };
        let (clear, roc) = unprotect_with_roc_recovery(&protected, &keys, &mut receiver)
            .expect("authenticate reset-ROC packet");
        assert_eq!(clear.seq, 11);
        assert_eq!(clear.ssrc, 77);
        assert_eq!(clear.opus, b"opus");
        assert_eq!(roc, 0);
        assert_eq!(receiver.roc, 0);
        assert_eq!(receiver.last, Some(11));
    }

    #[test]
    fn guessed_roc_is_tried_before_recovery_candidates_and_rollover_advances() {
        let keys = session_keys(&[0x13; 30]).unwrap();
        let header = build_audio_rtp_header(2, 640, 55, 8, false, 112);
        let protected = protect_rtp(&header, b"next", &keys, 2, 55, 1);
        let mut receiver = RocTracker {
            last: Some(65_530),
            roc: 0,
        };
        let (_, roc) = unprotect_with_roc_recovery(&protected, &keys, &mut receiver).unwrap();
        assert_eq!(roc, 1);
        assert_eq!(receiver.last, Some(2));
        assert_eq!(receiver.roc, 1);
    }

    #[test]
    fn group_receive_tries_authenticated_session_fallback_when_primary_key_is_wrong() {
        let primary = session_keys(&[0x11; 30]).unwrap();
        let fallback = session_keys(&[0x42; 30]).unwrap();
        let header = build_audio_rtp_header(17, 960, 0x1234, 9, false, 112);
        let protected = protect_rtp(&header, b"authenticated fallback", &fallback, 17, 0x1234, 0);
        let (clear, roc, used_fallback, mki_len, tag_len) = unprotect_group_with_candidates(
            &protected,
            Some(&primary),
            Some(&fallback),
            &[0, 1],
            None,
        )
        .expect("fallback key should authenticate");
        assert_eq!(clear.seq, 17);
        assert_eq!(clear.ssrc, 0x1234);
        assert_eq!(clear.opus, b"authenticated fallback");
        assert_eq!(roc, 0);
        assert!(used_fallback);
        assert_eq!(mki_len, 0);
        assert_eq!(tag_len, 10);
    }

    #[test]
    fn group_receive_prefers_primary_key_when_both_candidates_are_available() {
        let primary = session_keys(&[0x42; 30]).unwrap();
        let fallback = session_keys(&[0x11; 30]).unwrap();
        let header = build_audio_rtp_header(18, 960, 0x1234, 10, false, 112);
        let protected = protect_rtp(&header, b"primary key", &primary, 18, 0x1234, 0);
        let (_, _, used_fallback, mki_len, tag_len) = unprotect_group_with_candidates(
            &protected,
            Some(&primary),
            Some(&fallback),
            &[0],
            None,
        )
        .expect("primary key should authenticate");
        assert!(!used_fallback);
        assert_eq!(mki_len, 0);
        assert_eq!(tag_len, 10);
    }

    #[test]
    fn group_receive_accepts_only_authenticated_mki_packets() {
        let primary = session_keys(&[0x53; 30]).unwrap();
        let header = build_audio_rtp_header(20, 960, 0x1234, 11, false, 112);
        let protected = protect_rtp(&header, b"group payload with MKI", &primary, 20, 0x1234, 0);
        let tag_start = protected.len() - 10;
        let mut packet = protected[..tag_start].to_vec();
        packet.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        packet.extend_from_slice(&protected[tag_start..]);
        let (clear, roc, fallback, mki_len, tag_len) =
            unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None)
                .expect("valid MKI packet should authenticate");
        assert_eq!(clear.opus, b"group payload with MKI");
        assert_eq!(roc, 0);
        assert!(!fallback);
        assert_eq!(mki_len, 4);
        assert_eq!(tag_len, 10);

        let last = packet.len() - 1;
        packet[last] ^= 1;
        assert!(
            unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None).is_none()
        );
    }

    #[test]
    fn group_receive_discovers_and_caches_nonstandard_mki_and_tag_lengths() {
        let primary = session_keys(&[0x54; 30]).unwrap();
        let header = build_audio_rtp_header(21, 960, 0x1234, 12, false, 112);
        let protected = protect_rtp(&header, b"variable group profile", &primary, 21, 0x1234, 0);
        let tag_start = protected.len() - 10;
        let mut packet = protected[..tag_start].to_vec();
        packet.extend_from_slice(&[0x10, 0x20, 0x30]);
        packet.extend_from_slice(&protected[tag_start..tag_start + 8]);

        let (clear, roc, fallback, mki_len, tag_len) =
            unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None)
                .expect("variable MKI/tag profile should authenticate");
        assert_eq!(clear.opus, b"variable group profile");
        assert_eq!(roc, 0);
        assert!(!fallback);
        assert_eq!((mki_len, tag_len), (3, 8));

        let (_, _, _, cached_mki, cached_tag) = unprotect_group_with_candidates(
            &packet,
            Some(&primary),
            None,
            &[0],
            Some((mki_len, tag_len)),
        )
        .expect("cached profile should authenticate without profile discovery");
        assert_eq!((cached_mki, cached_tag), (3, 8));
    }

    #[test]
    fn authenticated_old_reordered_packet_does_not_rewind_tracker() {
        let mut tracker = RocTracker {
            last: Some(500),
            roc: 1,
        };
        tracker.update(450, 1);
        assert_eq!(tracker.last, Some(500));
        assert_eq!(tracker.roc, 1);
        tracker.update(350, 1);
        assert_eq!(tracker.last, Some(350));
    }

    #[test]
    fn local_ipv4_signature_is_sorted_and_excludes_loopback() {
        let signature = local_ipv4_signature();
        assert!(!signature.split(',').any(|ip| ip == "127.0.0.1"));
        let mut parts = signature
            .split(',')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let original = parts.clone();
        parts.sort();
        parts.dedup();
        assert_eq!(parts, original);
    }
}

#[cfg(test)]
mod video_transport_tests {
    use super::*;
    use crate::crypto::srtp::{session_keys, unprotect_rtp};
    use crate::protocol::rtp::build_group_audio_rtp_header;

    #[test]
    fn group_prepare_tries_next_init_layout_only_after_server_refusal() {
        use crate::protocol::zrtp::{
            build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
        };
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let relay_thread = thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (len, client) = relay
                .recv_from(&mut buf)
                .expect("receive first group InitZRTP");
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            assert_eq!(buf[0], zrtp_types::CONTROL);
            assert_eq!(buf[20], 0x04); // first candidate: zero layout
            let mut refusal = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                1,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            )
            .to_vec();
            refusal.extend_from_slice(&3u32.to_le_bytes());
            relay.send_to(&refusal, client).unwrap();

            let (len, client) = relay
                .recv_from(&mut buf)
                .expect("receive fallback group InitZRTP");
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            assert_eq!(buf[20], 0x06); // second candidate: sub6
            let mut accepted = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                2,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            )
            .to_vec();
            accepted.extend_from_slice(&0u32.to_le_bytes());
            accepted.extend_from_slice(&77u32.to_le_bytes());
            accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
            accepted.extend_from_slice(&0u16.to_le_bytes());
            relay.send_to(&accepted, client).unwrap();
        });

        let backend = ZrtcMediaBackend::new().unwrap();
        let selected = backend
            .prepare(
                MediaSessionParams {
                    call_id: 77,
                    host_call: 0x1234,
                    uid: 111,
                    peer_uid: 222,
                    sess_id: b"this-session-is-not-used-for-group-srtp".to_vec(),
                    is_caller: true,
                    is_group: true,
                    send_udp: true,
                    video_enabled: true,
                    video_config: serde_json::json!({ "srtpKey": "" }),
                    p2p_candidates: Vec::new(),
                },
                &[relay_addr],
            )
            .unwrap();
        relay_thread.join().unwrap();
        assert_eq!(selected, Some(relay_addr));
        assert_eq!(
            *backend.selected_server.lock().unwrap(),
            Some((relay_addr, 0xaabb_ccdd))
        );
        assert!(backend.keys.lock().unwrap().is_none());
        assert!(backend.params.lock().unwrap().as_ref().unwrap().is_group);
        backend.stop();
    }

    #[test]
    fn group_camera_uses_sfu_rtp_header_and_announces_quality_layers() {
        use crate::protocol::zrtp::{
            build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
        };
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let init_socket = relay.try_clone().unwrap();
        let relay_thread = thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (len, client) = init_socket
                .recv_from(&mut buf)
                .expect("receive group InitZRTP");
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            assert_eq!(buf[0], zrtp_types::CONTROL);
            let mut accepted = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                1,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            )
            .to_vec();
            accepted.extend_from_slice(&0u32.to_le_bytes());
            accepted.extend_from_slice(&77u32.to_le_bytes());
            accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
            accepted.extend_from_slice(&0u16.to_le_bytes());
            init_socket.send_to(&accepted, client).unwrap();
        });

        let backend = ZrtcMediaBackend::new().unwrap();
        let config = serde_json::json!({
            "maxWidth":1280,
            "maxHeight":720,
            "groupcall":{
                "codec":60,
                "temporalNum":1,
                "spatialLayers":[
                    {"factorNum":1,"factorDen":3,"bitrateKbps":300,"fps":15},
                    {"factorNum":1,"factorDen":2,"bitrateKbps":700,"fps":24},
                    {"factorNum":3,"factorDen":4,"bitrateKbps":1200,"fps":30}
                ]
            }
        });
        backend
            .prepare(
                MediaSessionParams {
                    call_id: 77,
                    host_call: 0x1234,
                    uid: 111,
                    peer_uid: 222,
                    sess_id: b"group-session-not-used-with-empty-srtp-key".to_vec(),
                    is_caller: true,
                    is_group: true,
                    send_udp: true,
                    video_enabled: true,
                    video_config: config,
                    p2p_candidates: Vec::new(),
                },
                &[relay_addr],
            )
            .unwrap();
        relay_thread.join().unwrap();
        assert!(backend.keys.lock().unwrap().is_none());
        assert_eq!(
            backend
                .video_profile()
                .map(|profile| (profile.width, profile.height)),
            Some((640, 360))
        );

        backend.start_with_audio(false);
        assert!(backend.send_group_video_quality_config().unwrap());
        assert!(backend.request_group_video(222).unwrap());
        assert!(backend.set_group_member_render_width(222, 700).unwrap());
        assert_eq!(
            backend
                .send_video_frame(true, &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0])
                .unwrap(),
            1
        );

        let mut saw_config = false;
        let mut requested_quality = Vec::new();
        let mut video_header = None;
        for _ in 0..8 {
            let mut packet = [0u8; 1500];
            let Ok((len, _)) = relay.recv_from(&mut packet) else {
                break;
            };
            let packet = &packet[..len];
            if packet.first() == Some(&zrtp_types::CONTROL) && packet.len() >= 24 {
                match packet[20] {
                    0x0c => saw_config = true,
                    0x0d if packet.len() >= 29 => requested_quality.push(packet[28]),
                    _ => {}
                }
            } else if packet.first() == Some(&zrtp_types::VIDEO_RTP) {
                video_header = Some(packet[5..].to_vec());
            }
        }
        assert!(saw_config, "SFU layer configuration should be announced");
        assert!(requested_quality.contains(&0));
        assert!(requested_quality.contains(&3));
        let rtp = video_header.expect("group H.264 RTP should reach the SFU");
        assert_eq!(rtp[0], 0x90);
        assert_eq!(rtp[23] >> 4, 12);
        assert_eq!(rtp[24], 0x03);
        assert_eq!(u16::from_be_bytes([rtp[25], rtp[26]]), 1);
        backend.stop();
    }

    #[test]
    fn group_camera_media_is_bidirectional_and_reports_remote_ssrc() {
        let relay = UdpSocket::bind("127.0.0.1:0").expect("bind group relay");
        relay
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        // SFU downlink media may use another UDP source port than the
        // InitZRTP endpoint; the legacy group receiver accepted that path.
        let alternate_relay =
            UdpSocket::bind("127.0.0.1:0").expect("bind alternate SFU media source");
        let backend = ZrtcMediaBackend::new().expect("create group media backend");
        let config = serde_json::json!({
            "maxWidth":640,"maxHeight":360,
            "groupcall":{"codec":60,"temporalNum":1,"spatialLayers":[
                {"factorNum":1,"factorDen":1,"bitrateKbps":400,"fps":15}
            ]}
        });
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 123,
            host_call: 111,
            uid: 111,
            peer_uid: 111,
            sess_id: vec![0x42; 30],
            is_caller: false,
            is_group: true,
            send_udp: true,
            video_enabled: true,
            video_config: config.clone(),
            p2p_candidates: Vec::new(),
        });
        *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
        // Model an SFU that uses a distinct receive session key: the configured
        // group key must fail authentication, then the receive-only session key
        // must authenticate the remote RTP before it reaches H.264 assembly.
        *backend.keys.lock().unwrap() = Some(session_keys(&[0x11; 30]).unwrap());
        *backend.rx_fallback_keys.lock().unwrap() = Some(session_keys(&[0x42; 30]).unwrap());
        *backend.group_video_plan.lock().unwrap() =
            Some(parse_group_video_plan(&config).expect("build group camera layers"));
        let bound = backend.socket.local_addr().unwrap();
        let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        backend.start_with_audio(false);

        let local_frame = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            0x88, 0x84, 0x21, 0x01,
        ];
        backend.set_group_camera(false).unwrap();
        assert_eq!(backend.send_video_frame(true, &local_frame).unwrap(), 0);
        backend.set_group_camera(true).unwrap();
        assert!(backend.send_group_video_quality_config().unwrap());
        // A tile resize before the first valid remote video frame must only
        // remember its width; the legacy client did not subscribe yet.
        assert!(!backend.set_group_member_render_width(222, 500).unwrap());
        assert!(backend.request_group_video(222).unwrap());
        assert!(!backend.set_group_member_render_width(222, 700).unwrap());
        assert!(backend.send_video_frame(true, &local_frame).unwrap() > 0);

        let mut tx_camera_rtp = None;
        for _ in 0..16 {
            let mut packet = [0u8; 1500];
            let Ok((len, _)) = relay.recv_from(&mut packet) else {
                break;
            };
            if len > 25 && packet[0] == msg_types::VIDEO_RTP {
                let rtp = &packet[5..len];
                if u32::from_be_bytes(rtp[8..12].try_into().unwrap()) == 111 && rtp[0] & 0x10 != 0 {
                    tx_camera_rtp = Some(rtp.to_vec());
                    break;
                }
            }
        }
        let tx_camera_rtp = tx_camera_rtp.expect("local camera RTP should reach the SFU");
        let primary_keys = session_keys(&[0x11; 30]).unwrap();
        let tx_clear = unprotect_rtp(&tx_camera_rtp, &primary_keys, 0)
            .expect("SFU uplink camera RTP should authenticate with the configured group key");
        assert_eq!(tx_clear.opus, local_frame);

        let remote_frame = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            0x88, 0x84, 0x21, 0x02,
        ];
        let now = unix_now_ms();
        let rtp = build_group_video_rtp_header(
            7,
            9000,
            222,
            77,
            true,
            VIDEO_PT_KEY,
            false,
            12,
            1,
            now,
            now,
        );
        let keys = session_keys(&[0x42; 30]).unwrap();
        // Real UDP downlink: first a packet authenticated by the configured
        // group key, then one authenticated by the receive-only session fallback.
        let primary_protected = protect_rtp(&rtp, &remote_frame, &primary_keys, 7, 222, 0);
        let mut packet = vec![msg_types::VIDEO_DOWN];
        packet.extend_from_slice(&primary_protected);
        alternate_relay.send_to(&packet, local).unwrap();
        let rtp_fallback = build_group_video_rtp_header(
            8,
            9900,
            223,
            78,
            true,
            VIDEO_PT_KEY,
            false,
            12,
            2,
            now,
            now,
        );
        let fallback_frame = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            0x88, 0x84, 0x21, 0x03,
        ];
        let fallback_protected = protect_rtp(&rtp_fallback, &fallback_frame, &keys, 8, 223, 0);
        let mut fallback_packet = vec![msg_types::VIDEO_DOWN];
        fallback_packet.extend_from_slice(&fallback_protected);
        alternate_relay.send_to(&fallback_packet, local).unwrap();

        let mut received = Vec::new();
        let mut events = Vec::new();
        for _ in 0..100 {
            received.extend(backend.drain_video_frames());
            events.extend(backend.drain_group_events());
            let received_222 = received.iter().any(|frame| frame.src == Some(222));
            let received_223 = received.iter().any(|frame| frame.src == Some(223));
            if received_222
                && received_223
                && events.contains(&GroupMediaEvent::MemberVideo(222))
                && events.contains(&GroupMediaEvent::MemberVideo(223))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received.len(), 2);
        assert!(received
            .iter()
            .any(|frame| frame.src == Some(222) && frame.data == remote_frame));
        assert!(received
            .iter()
            .any(|frame| frame.src == Some(223) && frame.data == fallback_frame));
        assert!(events.contains(&GroupMediaEvent::MemberVideo(222)));
        assert!(events.contains(&GroupMediaEvent::MemberVideo(223)));
        backend.stop();
    }

    #[test]
    fn group_sfu_room_and_end_call_replies_become_engine_events() {
        use crate::protocol::zrtp::commands;

        let roster = crate::protocol::zrtp::ServerReply {
            msg_type: msg_types::SERVER_REPLY,
            seq: 1,
            uid: 111,
            token: 0,
            cmd: commands::ROOM_UPDATED,
            sub_cmd: 0,
            payload: [222u32.to_le_bytes(), 333u32.to_le_bytes()].concat(),
            result: None,
            call_id: None,
            server_token: None,
            public_addr: None,
        };
        assert_eq!(
            group_event_from_reply(&roster, 111),
            Some(GroupMediaEvent::Roster(vec![222, 333]))
        );

        let member_left = crate::protocol::zrtp::ServerReply {
            uid: 222,
            cmd: commands::END_CALL,
            sub_cmd: 3,
            payload: Vec::new(),
            ..roster.clone()
        };
        assert_eq!(
            group_event_from_reply(&member_left, 111),
            Some(GroupMediaEvent::MemberLeft(222, 3))
        );

        let kicked = crate::protocol::zrtp::ServerReply {
            uid: 111,
            sub_cmd: 2,
            ..member_left.clone()
        };
        assert_eq!(
            group_event_from_reply(&kicked, 111),
            Some(GroupMediaEvent::PeerEnd(2))
        );

        let ignored = crate::protocol::zrtp::ServerReply {
            uid: 111,
            sub_cmd: 0,
            ..kicked
        };
        assert_eq!(group_event_from_reply(&ignored, 111), None);
    }

    #[test]
    fn p2p_handover_notifies_peer_and_requests_relay_fallback() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        for target in [&peer, &relay] {
            target
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
        }
        let params = MediaSessionParams {
            call_id: 123,
            host_call: 111,
            uid: 111,
            peer_uid: 222,
            sess_id: b"p2p-fallback-session".to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        };
        let path = Mutex::new(Some(peer.local_addr().unwrap()));
        let enabled = AtomicBool::new(true);
        assert!(stop_p2p_path(
            &socket,
            Some((relay.local_addr().unwrap(), 0xaabb_ccdd)),
            &path,
            &enabled,
            &params,
            true,
        ));
        assert!(!enabled.load(Ordering::Acquire));
        assert_eq!(*path.lock().unwrap(), None);

        for expected_seq in 0..3u32 {
            let mut packet = [0u8; 128];
            let (len, _) = peer.recv_from(&mut packet).unwrap();
            let (header, body) = p2p::parse(&packet[..len]).unwrap();
            assert_eq!(header.kind, p2p::SIGNAL);
            assert_eq!(header.call_id, 123);
            assert!(!header.is_callee);
            assert_eq!(body[0], 2);
            assert_eq!(
                u32::from_le_bytes(body[1..5].try_into().unwrap()),
                expected_seq
            );

            let (_len, _) = relay.recv_from(&mut packet).unwrap();
            assert_eq!(packet[0], msg_types::CONTROL);
            assert_eq!(
                u32::from_le_bytes(packet[14..18].try_into().unwrap()),
                0xaabb_ccdd
            );
            assert_eq!(
                u16::from_le_bytes(packet[18..20].try_into().unwrap()),
                crate::protocol::zrtp::commands::REQ_FORWARD
            );
            assert_eq!(packet[20], 2);
        }
        assert!(!stop_p2p_path(
            &socket, None, &path, &enabled, &params, true
        ));
    }

    #[test]
    fn network_change_sends_change_address_ping_and_relay_request() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let params = MediaSessionParams {
            call_id: 123,
            host_call: 111,
            uid: 111,
            peer_uid: 222,
            sess_id: b"network-change-session".to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        };
        send_change_address(
            &socket,
            Some((relay.local_addr().unwrap(), 0x1122_3344)),
            &params,
            9,
        );
        let mut commands_seen = Vec::new();
        for _ in 0..3 {
            let mut packet = [0u8; 256];
            let (len, _) = relay.recv_from(&mut packet).unwrap();
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            commands_seen.push(u16::from_le_bytes(packet[18..20].try_into().unwrap()));
        }
        assert_eq!(
            commands_seen,
            vec![
                crate::protocol::zrtp::commands::CHANGE_ADDRESS,
                crate::protocol::zrtp::commands::PING,
                crate::protocol::zrtp::commands::REQ_FORWARD,
            ]
        );
    }

    #[test]
    fn ring_ping_interval_honors_zcall_ping_ms_and_keeps_a_safe_default() {
        assert_eq!(
            ring_ping_interval_from(Some("100")),
            Duration::from_millis(100)
        );
        assert_eq!(ring_ping_interval_from(None), Duration::from_secs(6));
        assert_eq!(ring_ping_interval_from(Some("0")), Duration::from_secs(6));
        assert_eq!(
            ring_ping_interval_from(Some("70000")),
            Duration::from_secs(6)
        );
    }

    #[test]
    fn ring_ping_reaches_every_candidate_at_the_configured_interval_and_stops() {
        let relay_a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_b = UdpSocket::bind("127.0.0.1:0").unwrap();
        for relay in [&relay_a, &relay_b] {
            relay
                .set_read_timeout(Some(Duration::from_millis(400)))
                .unwrap();
        }
        let backend = ZrtcMediaBackend::new().unwrap();
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 77,
            host_call: 111,
            uid: 111,
            peer_uid: 222,
            sess_id: b"ring-ping-session".to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        *backend.ring_servers.lock().unwrap() = vec![
            (relay_a.local_addr().unwrap(), 0x1111),
            (relay_b.local_addr().unwrap(), 0x2222),
        ];
        backend.start_ring_ping_with_interval(Duration::from_millis(50));

        for (relay, expected_token) in [(&relay_a, 0x1111u32), (&relay_b, 0x2222u32)] {
            for _ in 0..2 {
                let mut packet = [0u8; 256];
                let (len, _) = relay.recv_from(&mut packet).unwrap();
                assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
                assert_eq!(packet[0], msg_types::CONTROL);
                assert_eq!(
                    u32::from_le_bytes(packet[14..18].try_into().unwrap()),
                    expected_token
                );
                assert_eq!(
                    u16::from_le_bytes(packet[18..20].try_into().unwrap()),
                    crate::protocol::zrtp::commands::PING
                );
            }
        }

        backend.ring_ping_running.store(false, Ordering::Release);
        thread::sleep(Duration::from_millis(120));
        for relay in [&relay_a, &relay_b] {
            relay
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut packet = [0u8; 256];
            assert!(relay.recv_from(&mut packet).is_err());
        }
    }

    #[test]
    fn selecting_a_relay_pings_it_and_ends_other_ring_sessions() {
        let selected = UdpSocket::bind("127.0.0.1:0").unwrap();
        let other = UdpSocket::bind("127.0.0.1:0").unwrap();
        selected
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        other
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let backend = ZrtcMediaBackend::new().unwrap();
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 77,
            host_call: 111,
            uid: 111,
            peer_uid: 222,
            sess_id: b"ring-select-session".to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        let selected_addr = selected.local_addr().unwrap();
        let other_addr = other.local_addr().unwrap();
        *backend.selected_server.lock().unwrap() = Some((selected_addr, 0xaaaa));
        *backend.ring_servers.lock().unwrap() = vec![(selected_addr, 0xaaaa), (other_addr, 0xbbbb)];

        backend.start_with_audio(false);
        let mut selected_packet = [0u8; 256];
        selected.recv_from(&mut selected_packet).unwrap();
        assert_eq!(
            u16::from_le_bytes(selected_packet[18..20].try_into().unwrap()),
            crate::protocol::zrtp::commands::PING
        );
        let mut other_packet = [0u8; 256];
        other.recv_from(&mut other_packet).unwrap();
        assert_eq!(
            u16::from_le_bytes(other_packet[18..20].try_into().unwrap()),
            crate::protocol::zrtp::commands::END_CALL
        );
        backend.stop();
    }

    #[test]
    fn group_camera_advertisement_is_not_disabled_by_srtp_mode() {
        assert!(group_camera_advertised(true, true));
        assert!(!group_camera_advertised(false, true));
        assert!(!group_camera_advertised(true, false));
    }

    #[test]
    fn clear_group_h264_probe_only_accepts_annex_b_or_group_fragments() {
        assert!(looks_like_group_h264_payload(&[0, 0, 0, 1, 0x65, 1]));
        assert!(looks_like_group_h264_payload(&[0, 0, 1, 0x41, 1]));
        assert!(looks_like_group_h264_payload(&[0x7c, 0x85, 0x11, 0x22]));
        assert!(looks_like_group_h264_payload(&[0x5c, 0x85, 0x11, 0x22]));
        assert!(!looks_like_group_h264_payload(&[0x7c, 0x80]));
        assert!(!looks_like_group_h264_payload(&[0x61, 0x85, 0x11]));
        assert!(looks_like_group_h264_payload(&[0x7c, 0x85, 1, 2]));
        assert!(looks_like_group_h264_payload(&[0x7c, 0x45, 1, 2]));
        assert!(!looks_like_group_h264_payload(&[0x9a, 0x7f, 0x21, 0x44]));
        assert!(!looks_like_group_h264_payload(&[0x7c, 0x9c, 1, 2]));
    }

    #[test]
    fn group_periodic_ping_sequence_shares_counter_with_video_controls() {
        let control_seq = AtomicU32::new(0);
        let ping_seq = next_group_control_sequence(&control_seq);
        let config_seq = next_group_control_sequence(&control_seq);
        let request_seq = next_group_control_sequence(&control_seq);
        assert_eq!((ping_seq, config_seq, request_seq), (0, 1, 2));

        let ping = build_zavi_ping_packet(111, 0x1234, ping_seq, b"group-session", true);
        let config = build_group_video_quality_config(
            111,
            0x1234,
            config_seq,
            &[GroupVideoLayer {
                id: 0,
                bitrate_kbps: 300,
                width: 640,
                height: 360,
            }],
        );
        let request = build_group_video_quality_request(
            111,
            0x1234,
            request_seq,
            &[GroupVideoQualityRequest {
                uid: 222,
                quality_id: 0,
            }],
        );
        for (packet, expected) in [(&ping, 0), (&config, 1), (&request, 2)] {
            assert_eq!(
                u32::from_le_bytes(packet[6..10].try_into().unwrap()),
                expected
            );
        }
    }

    #[test]
    fn selecting_a_group_relay_sends_zavi_ping_not_one_to_one_ping() {
        use crate::protocol::zrtp::commands;

        let selected = UdpSocket::bind("127.0.0.1:0").unwrap();
        let other = UdpSocket::bind("127.0.0.1:0").unwrap();
        selected
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        other
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();

        let backend = ZrtcMediaBackend::new().unwrap();
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 77,
            host_call: 111,
            uid: 111,
            peer_uid: 111,
            sess_id: b"group-select-session".to_vec(),
            is_caller: true,
            is_group: true,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        let selected_addr = selected.local_addr().unwrap();
        let other_addr = other.local_addr().unwrap();
        *backend.selected_server.lock().unwrap() = Some((selected_addr, 0xaaaa));
        *backend.ring_servers.lock().unwrap() = vec![(selected_addr, 0xaaaa), (other_addr, 0xbbbb)];

        backend.finish_ring_selection();

        let mut selected_packet = [0u8; 256];
        let (selected_len, _) = selected.recv_from(&mut selected_packet).unwrap();
        assert_eq!(
            u16::from_le_bytes(selected_packet[18..20].try_into().unwrap()),
            commands::INIT_ZAVI_PING,
            "group relay must receive ZaviPing immediately after selection"
        );
        assert_eq!(selected_packet[20], 1);
        assert_eq!(
            u32::from_le_bytes(selected_packet[6..10].try_into().unwrap()),
            0,
            "first group control packet consumes sequence zero"
        );
        assert_eq!(backend.group_control_seq.load(Ordering::Relaxed), 1);
        assert!(selected_len >= 38);

        let mut other_packet = [0u8; 256];
        other.recv_from(&mut other_packet).unwrap();
        assert_eq!(
            u16::from_le_bytes(other_packet[18..20].try_into().unwrap()),
            commands::END_CALL
        );
        assert!(backend.last_ping.lock().unwrap().elapsed() < Duration::from_secs(1));
        backend.stop();
    }

    #[test]
    fn group_screen_share_uses_a_second_socket_uid_token_and_rtp_stream() {
        use crate::protocol::zrtp::{
            build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
        };
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let init_socket = relay.try_clone().unwrap();
        let relay_thread = thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (len, client) = init_socket
                .recv_from(&mut buf)
                .expect("receive group InitZRTP");
            assert_eq!(buf[0], zrtp_types::CONTROL);
            let mut accepted = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                1,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            )
            .to_vec();
            accepted.extend_from_slice(&0u32.to_le_bytes());
            accepted.extend_from_slice(&77u32.to_le_bytes());
            accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
            accepted.extend_from_slice(&0u16.to_le_bytes());
            init_socket.send_to(&accepted, client).unwrap();
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        });

        let backend = ZrtcMediaBackend::new().unwrap();
        backend
            .prepare(
                MediaSessionParams {
                    call_id: 77,
                    host_call: 0x1234,
                    uid: 111,
                    peer_uid: 222,
                    sess_id: b"group-share-session".to_vec(),
                    is_caller: true,
                    is_group: true,
                    send_udp: true,
                    video_enabled: false,
                    video_config: serde_json::json!({
                        "maxShrScreenBr":1400,
                        "groupcall":{"codec":60,"temporalNum":1,"spatialLayers":[
                            {"factorNum":1,"factorDen":2,"bitrateKbps":700,"fps":24}
                        ]}
                    }),
                    p2p_candidates: Vec::new(),
                },
                &[relay_addr],
            )
            .unwrap();
        relay_thread.join().unwrap();
        let main_port = backend.socket.local_addr().unwrap().port();
        backend.start_with_audio(false);
        let mut selected_ping = [0u8; 1500];
        let (_ping_len, _) = relay.recv_from(&mut selected_ping).unwrap();
        assert_eq!(
            u16::from_le_bytes(selected_ping[18..20].try_into().unwrap()),
            commands::INIT_ZAVI_PING
        );
        backend.start_group_share(777).unwrap();

        let mut init = [0u8; 1500];
        let (_init_len, share_client) = relay.recv_from(&mut init).unwrap();
        assert_eq!(init[0], zrtp_types::CONTROL);
        assert_eq!(u32::from_le_bytes(init[10..14].try_into().unwrap()), 777);
        assert_ne!(
            share_client.port(),
            main_port,
            "share must own a second UDP socket"
        );
        assert_eq!(
            u16::from_le_bytes(init[18..20].try_into().unwrap()),
            commands::INIT_CALL_CALLEE
        );

        let mut accepted = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            1,
            777,
            0,
            commands::INIT_CALL_CALLEE,
            4,
        )
        .to_vec();
        accepted.extend_from_slice(&0u32.to_le_bytes());
        accepted.extend_from_slice(&77u32.to_le_bytes());
        accepted.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        accepted.extend_from_slice(&0u16.to_le_bytes());
        relay.send_to(&accepted, share_client).unwrap();

        let ready_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let ready = backend
                .share_peer
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|peer| peer.token == 0x1122_3344);
            if ready || Instant::now() >= ready_deadline {
                assert!(ready, "share InitZRTP response should establish a token");
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(backend
            .drain_group_events()
            .contains(&GroupMediaEvent::ShareReady));
        assert_eq!(
            backend
                .send_group_share_frame(
                    true,
                    &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0],
                    1280,
                    720
                )
                .unwrap(),
            1
        );

        let mut video = None;
        for _ in 0..8 {
            let mut packet = [0u8; 1500];
            let Ok((len, _)) = relay.recv_from(&mut packet) else {
                break;
            };
            if packet[0] == zrtp_types::VIDEO_RTP {
                video = Some(packet[..len].to_vec());
                break;
            }
        }
        let video = video.expect("screen-share RTP should reach the SFU");
        assert_eq!(
            u32::from_le_bytes(video[1..5].try_into().unwrap()),
            0x1122_3344
        );
        let rtp = &video[5..];
        assert_eq!(u32::from_be_bytes(rtp[8..12].try_into().unwrap()), 777);
        assert_eq!(rtp[23] >> 4, 12);

        backend.stop_group_share();
        let mut end = [0u8; 1500];
        let (end_len, _) = relay.recv_from(&mut end).unwrap();
        assert_eq!(end[0], zrtp_types::CONTROL);
        assert_eq!(u32::from_le_bytes(end[10..14].try_into().unwrap()), 777);
        assert_eq!(
            u16::from_le_bytes(end[18..20].try_into().unwrap()),
            commands::END_CALL
        );
        assert!(end_len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        backend.stop();
    }

    #[test]
    fn group_share_worker_recognizes_targeted_pli_and_fir() {
        let pli = [0x81, 206, 0, 2, 0, 0, 0, 1, 0, 0, 3, 9];
        assert!(rtcp_requests_keyframe(&pli, 0x0309));
        assert!(!rtcp_requests_keyframe(&pli, 0x0310));

        let fir = [0x84, 206, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 3, 9];
        assert!(rtcp_requests_keyframe(&fir, 0x0309));
        assert!(!rtcp_requests_keyframe(&fir, 0x0310));
    }

    #[test]
    fn legacy_group_downlink_accepts_alternate_relay_endpoint_only_for_group() {
        let selected: SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let same_host_other_port: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let other_host: SocketAddr = "192.0.2.11:4000".parse().unwrap();

        assert!(accepts_relay_source(Some(selected), selected, true));
        assert!(accepts_relay_source(
            Some(selected),
            same_host_other_port,
            true
        ));
        assert!(accepts_relay_source(Some(selected), other_host, true));
        assert!(!accepts_relay_source(
            Some(selected),
            same_host_other_port,
            false
        ));
        assert!(!accepts_relay_source(Some(selected), other_host, false));
    }

    #[test]
    fn one_to_one_media_watchdog_expires_after_twenty_seconds_without_peer_media() {
        assert!(!media_receive_timed_out(0, 50_000));
        assert!(!media_receive_timed_out(10_000, 29_999));
        assert!(media_receive_timed_out(10_000, 30_000));
        assert!(!media_receive_timed_out(30_000, 29_000));
    }

    #[test]
    fn member_speaking_uses_four_of_ten_audio_levels_and_expires_after_silence() {
        let now = Instant::now();
        let mut tracker = MemberSpeakingTracker::default();
        for index in 0..3 {
            assert_eq!(
                tracker.observe(45, now + Duration::from_millis(index * 20)),
                None
            );
        }
        assert_eq!(
            tracker.observe(45, now + Duration::from_millis(60)),
            Some(true)
        );
        assert_eq!(tracker.observe(80, now + Duration::from_millis(80)), None);
        assert!(!tracker.expire(now + Duration::from_millis(659)));
        assert!(tracker.expire(now + Duration::from_millis(660)));
        assert_eq!(tracker.observe(80, now + Duration::from_millis(680)), None);
    }

    #[test]
    fn plain_group_audio_parser_handles_header_extensions_and_padding() {
        let header =
            build_group_audio_rtp_header(42, 320, 0x1234, 7, false, 112, 1_700_000_000_000, 35);
        let mut packet = header;
        packet.extend_from_slice(&[0xf8, 0xff, 0xfe, 0x01]);
        packet.extend_from_slice(&[0, 2]); // RTP padding count = 2
        packet[0] |= 0x20;
        let parsed = read_plain_rtp_payload(&packet).expect("parse group RTP payload");
        assert_eq!(parsed.0, 42);
        assert_eq!(parsed.1, 0x1234);
        assert_eq!(parsed.2, [0xf8, 0xff, 0xfe, 0x01]);
        assert!(read_plain_rtp_payload(&packet[..20]).is_none());
    }

    #[test]
    fn caller_installs_candidates_from_answer_before_media_binder_starts() {
        let backend = ZrtcMediaBackend::new().unwrap();
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let params = MediaSessionParams {
            call_id: 7,
            host_call: 0,
            uid: 10,
            peer_uid: 20,
            sess_id: vec![1; 30],
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        };
        backend
            .prepare(params, &[relay.local_addr().unwrap()])
            .unwrap();
        backend
            .set_p2p_candidates(vec!["192.0.2.5:42000".parse().unwrap()])
            .unwrap();
        let stored = backend.params.lock().unwrap().clone().unwrap();
        assert_eq!(
            stored.p2p_candidates,
            vec!["192.0.2.5:42000".parse().unwrap()]
        );
    }

    #[test]
    fn active_audio_device_switch_updates_generation_without_restarting_media() {
        let backend = ZrtcMediaBackend::new().unwrap();
        let generation = backend.device_generation.load(Ordering::Acquire);
        backend.set_device("mic", "alsa_input.usb-headset");
        let after_mic = backend.device_generation.load(Ordering::Acquire);
        assert_eq!(after_mic, generation + 1);
        assert_eq!(
            backend.selected_devices().0.as_deref(),
            Some("alsa_input.usb-headset")
        );

        backend.set_device("mic", "alsa_input.usb-headset");
        assert_eq!(backend.device_generation.load(Ordering::Acquire), after_mic);

        backend.set_device("speaker", "alsa_output.usb-headset");
        assert_eq!(
            backend.device_generation.load(Ordering::Acquire),
            after_mic + 1
        );
        assert_eq!(
            backend.selected_devices().1.as_deref(),
            Some("alsa_output.usb-headset")
        );

        backend.set_device("mic", "");
        assert_eq!(
            backend.device_generation.load(Ordering::Acquire),
            after_mic + 2
        );
        assert_eq!(backend.selected_devices().0, None);
    }

    #[test]
    fn prepare_collects_all_accepted_relays_and_selects_first_successful_reply() {
        use crate::protocol::zrtp::{
            build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
        };
        use std::thread;

        fn relay(delay: Duration, public: &'static [u8]) -> (SocketAddr, thread::JoinHandle<()>) {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let addr = socket.local_addr().unwrap();
            let handle = thread::spawn(move || {
                let mut buf = [0u8; 1500];
                let (len, client) = socket.recv_from(&mut buf).expect("receive InitZRTP");
                assert_eq!(buf[0], zrtp_types::CONTROL);
                let _ = len;
                thread::sleep(delay);
                let header = build_control_header(
                    zrtp_types::SERVER_REPLY,
                    MAGIC_BYTE,
                    1,
                    111,
                    0,
                    commands::INIT_CALL_CALLER,
                    2,
                );
                let mut reply = header.to_vec();
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&77u32.to_le_bytes());
                reply.extend_from_slice(&0xabcdu32.to_le_bytes());
                reply.extend_from_slice(&(public.len() as u16).to_le_bytes());
                reply.extend_from_slice(public);
                socket.send_to(&reply, client).expect("send InitZRTP reply");
            });
            (addr, handle)
        }

        let media = ZrtcMediaBackend::new().unwrap();
        let (slow_addr, slow_thread) = relay(Duration::from_millis(120), b"198.51.100.10|4000");
        let (fast_addr, fast_thread) = relay(Duration::from_millis(10), b"198.51.100.20|5000");
        let servers = [slow_addr, fast_addr];
        let selected = media
            .prepare(
                MediaSessionParams {
                    call_id: 77,
                    host_call: 0,
                    uid: 111,
                    peer_uid: 222,
                    sess_id: vec![0x42; 30],
                    is_caller: true,
                    is_group: false,
                    send_udp: true,
                    video_enabled: false,
                    video_config: Value::Null,
                    p2p_candidates: Vec::new(),
                },
                &servers,
            )
            .unwrap();
        slow_thread.join().unwrap();
        fast_thread.join().unwrap();
        assert_eq!(selected, Some(servers[1]));
        assert_eq!(media.selected_server.lock().unwrap().unwrap().0, servers[1]);
        assert_eq!(media.ring_servers.lock().unwrap().len(), 2);
        assert_eq!(
            media.public_address().as_deref(),
            Some("198.51.100.20|5000")
        );
        media.stop();
    }

    #[test]
    fn stop_during_ringing_sends_end_call_before_media_workers_start() {
        use crate::protocol::zrtp::{
            build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
        };
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (len, client) = relay.recv_from(&mut buf).expect("receive InitZRTP");
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            let mut reply = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                1,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            )
            .to_vec();
            reply.extend_from_slice(&0u32.to_le_bytes());
            reply.extend_from_slice(&77u32.to_le_bytes());
            reply.extend_from_slice(&0xabcdu32.to_le_bytes());
            reply.extend_from_slice(&0u16.to_le_bytes());
            relay.send_to(&reply, client).unwrap();
            let (len, _) = relay
                .recv_from(&mut buf)
                .expect("receive EndCall while ringing");
            assert_eq!(buf[0], zrtp_types::CONTROL);
            let cmd = u16::from_le_bytes([buf[18], buf[19]]);
            assert_eq!(cmd, commands::END_CALL);
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        });

        let media = ZrtcMediaBackend::new().unwrap();
        media
            .prepare(
                MediaSessionParams {
                    call_id: 77,
                    host_call: 0,
                    uid: 111,
                    peer_uid: 222,
                    sess_id: vec![0x42; 30],
                    is_caller: true,
                    is_group: false,
                    send_udp: true,
                    video_enabled: false,
                    video_config: Value::Null,
                    p2p_candidates: Vec::new(),
                },
                &[relay_addr],
            )
            .unwrap();
        assert!(!media.running.load(Ordering::Relaxed));
        media.stop();
        handle.join().unwrap();
    }

    #[test]
    fn incoming_h264_frame_is_srtp_verified_and_foreign_sources_are_dropped() {
        let relay = UdpSocket::bind("127.0.0.1:0").expect("bind selected relay");
        let relay_addr = relay.local_addr().unwrap();
        let rogue = UdpSocket::bind("127.0.0.1:0").expect("bind unrelated source");

        let backend = ZrtcMediaBackend::new().expect("create media backend");
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 123,
            host_call: 0,
            uid: 0x1234,
            peer_uid: 0x5678,
            sess_id: vec![1; 30],
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
        let master = [0x42; 30];
        let keys = session_keys(&master).unwrap();
        *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
        let bound = backend.socket.local_addr().unwrap();
        let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        backend.start();

        let annex_b = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            0x88, 0x84, 0x21, 0x01,
        ];
        let chunks = packetize_frame(&annex_b, 9);
        assert_eq!(chunks.len(), 3);
        let mut packets = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let seq = 7 + index as u16;
            let header = build_audio_rtp_header(
                seq,
                9000,
                0x1234,
                55 + index as u16,
                index + 1 == chunks.len(),
                VIDEO_PT_KEY,
            );
            let protected = protect_rtp(&header, chunk, &keys, seq, 0x1234, 0);
            let mut packet = vec![msg_types::VIDEO_DOWN];
            packet.extend_from_slice(&protected);
            packets.push(packet);
        }

        // A correctly authenticated RTP datagram from an unselected source
        // must not be allowed to inject a video frame.
        rogue.send_to(&packets[0], local).unwrap();
        thread::sleep(Duration::from_millis(60));
        assert!(backend.drain_video_frames().is_empty());

        // Deliver the last fragment before the middle fragment, as happens
        // when relay and direct-path copies take different network routes.
        relay.send_to(&packets[0], local).unwrap();
        relay.send_to(&packets[2], local).unwrap();
        relay.send_to(&packets[1], local).unwrap();
        let mut received = Vec::new();
        for _ in 0..100 {
            received = backend.drain_video_frames();
            if !received.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received.len(), 1);
        assert!(received[0].key);
        assert_eq!(received[0].data, annex_b);
        backend.stop();
    }

    #[test]
    fn remote_group_share_is_plain_rtp_even_when_camera_peer_has_srtp_keys() {
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let backend = ZrtcMediaBackend::new().unwrap();
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 123,
            host_call: 111,
            uid: 111,
            peer_uid: 222,
            sess_id: vec![1; 30],
            is_caller: true,
            is_group: true,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
        *backend.keys.lock().unwrap() = Some(session_keys(&[0x42; 30]).unwrap());
        backend.set_group_share_remote_uid(777);
        let bound = backend.socket.local_addr().unwrap();
        let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        backend.start_with_audio(false);

        let annex_b = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0x01];
        let mut rtp = build_group_video_rtp_header(
            7,
            9000,
            777,
            55,
            true,
            100,
            false,
            12,
            1,
            unix_now_ms(),
            unix_now_ms(),
        );
        rtp.extend_from_slice(&annex_b);
        let mut packet = vec![msg_types::VIDEO_DOWN];
        packet.extend_from_slice(&rtp);
        relay.send_to(&packet, local).unwrap();

        let mut received = Vec::new();
        for _ in 0..100 {
            received = backend.drain_video_frames();
            if !received.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].src, Some(777));
        assert!(received[0].key);
        assert_eq!(received[0].data, annex_b);
        backend.stop();
    }

    #[test]
    fn p2p_bind_keepalive_and_media_duplicate_use_authenticated_call_path() {
        use crate::protocol::zrtp::p2p;
        use std::thread;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let backend = ZrtcMediaBackend::new().unwrap();
        let call_id = 0x1234_5678;
        let local_uid = 0x1111;
        let peer_uid = 0x2222;
        let master = [0x42; 30];
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id,
            host_call: 0,
            uid: local_uid,
            peer_uid,
            sess_id: vec![1; 30],
            is_caller: false,
            is_group: false,
            send_udp: true,
            video_enabled: true,
            video_config: Value::Null,
            p2p_candidates: vec![peer_addr],
        });
        *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
        *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
        *backend.rtcp.lock().unwrap() = Some(RtcpFeedback::new(local_uid));
        backend.start_with_audio(false);

        // The caller sends a BIND_PEER reply to the callee's bind attempt.
        let mut bind_peer = p2p::header(false, call_id, p2p::BIND_PEER).to_vec();
        bind_peer.extend_from_slice(&[0x34, 0x12, 0, 0, 0, 1]);
        bind_peer.extend_from_slice(&0u32.to_le_bytes());
        bind_peer.push(0xff);
        peer.send_to(&bind_peer, backend.socket.local_addr().unwrap())
            .unwrap();

        let mut buffer = [0u8; 1600];
        let mut bind_answer_seen = false;
        for _ in 0..40 {
            let Ok((length, _)) = peer.recv_from(&mut buffer) else {
                continue;
            };
            if let Some((header, body)) = p2p::parse(&buffer[..length]) {
                if header.kind == p2p::BIND_ANSWER {
                    assert!(header.is_callee);
                    assert_eq!(header.call_id, call_id);
                    assert!(body.len() >= 6 + 4);
                    bind_answer_seen = true;
                    break;
                }
            }
        }
        assert!(
            bind_answer_seen,
            "native engine should answer the peer's BIND_PEER"
        );

        // A valid peer keepalive completes the NAT path and gets a pong.
        let ping = p2p::wrap(false, call_id, p2p::KEEPALIVE, &[1]);
        peer.send_to(&ping, backend.socket.local_addr().unwrap())
            .unwrap();
        let mut pong_seen = false;
        for _ in 0..40 {
            let Ok((length, _)) = peer.recv_from(&mut buffer) else {
                continue;
            };
            if let Some((header, body)) = p2p::parse(&buffer[..length]) {
                if header.kind == p2p::KEEPALIVE && body.first() == Some(&2) {
                    assert!(header.is_callee);
                    pong_seen = true;
                    break;
                }
            }
        }
        assert!(pong_seen, "native engine should answer peer keepalive");
        assert_eq!(*backend.p2p_path.lock().unwrap(), Some(peer_addr));

        // Once the path is up the same protected H.264 packet is sent by both
        // relay and peer UDP, with each transport's own envelope.
        let frame = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
        assert_eq!(backend.send_video_frame(true, &frame).unwrap(), 1);
        let (relay_len, _) = relay.recv_from(&mut buffer).unwrap();
        assert_eq!(buffer[0], msg_types::VIDEO_RTP);
        let mut direct_seen = false;
        for _ in 0..40 {
            let Ok((length, _)) = peer.recv_from(&mut buffer) else {
                continue;
            };
            if let Some((header, body)) = p2p::parse(&buffer[..length]) {
                if header.kind == p2p::VIDEO {
                    assert!(header.is_callee);
                    assert_eq!(body.len(), relay_len - 5);
                    direct_seen = true;
                    break;
                }
            }
        }
        assert!(
            direct_seen,
            "native video should also be sent over the direct path"
        );
        backend.stop();
        thread::sleep(Duration::from_millis(30));
    }

    #[test]
    fn voice_only_session_never_sends_video_frames() {
        let backend = ZrtcMediaBackend::new().unwrap();
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 1,
            host_call: 0,
            uid: 111,
            peer_uid: 222,
            sess_id: b"voice-only-session".to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: false,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        assert_eq!(
            backend
                .send_video_frame(true, &[0, 0, 0, 1, 0x65, 0x88])
                .unwrap(),
            0
        );
    }

    #[test]
    fn outgoing_h264_frame_is_fragmented_srtp_protected_and_sent_to_relay() {
        let relay = UdpSocket::bind("127.0.0.1:0").expect("bind test relay");
        relay
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set relay timeout");
        let relay_addr = relay.local_addr().unwrap();

        let backend = ZrtcMediaBackend::new().expect("create media backend");
        *backend.params.lock().unwrap() = Some(MediaSessionParams {
            call_id: 123,
            host_call: 0,
            uid: 0x1234,
            peer_uid: 0x5678,
            sess_id: vec![1; 30],
            is_caller: true,
            is_group: false,
            send_udp: true,
            video_enabled: true,
            video_config: Value::Null,
            p2p_candidates: Vec::new(),
        });
        *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
        let master = [0x42; 30];
        let keys = session_keys(&master).unwrap();
        *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
        *backend.rtcp.lock().unwrap() = Some(RtcpFeedback::new(0x1234));
        backend.start();

        let annex_b: Vec<u8> = (0..2500).map(|n| (n % 251) as u8).collect();
        assert_eq!(backend.send_video_frame(false, &annex_b).unwrap(), 0);
        assert!(backend.take_local_keyframe_request());
        assert_eq!(backend.send_video_frame(true, &annex_b).unwrap(), 3);

        let mut recovered = Vec::new();
        let mut original_packets = Vec::new();
        for index in 0..3 {
            let mut packet = [0u8; 1600];
            let len = loop {
                let (len, from) = relay.recv_from(&mut packet).expect("receive H264 RTP");
                assert_eq!(from.port(), backend.socket.local_addr().unwrap().port());
                if packet[0] == msg_types::VIDEO_RTP {
                    break len;
                }
            };
            assert_eq!(
                u32::from_le_bytes(packet[1..5].try_into().unwrap()),
                0xaabb_ccdd
            );

            let rtp = &packet[5..len];
            original_packets.push(rtp.to_vec());
            let header_len = parse_rtp_header_len(rtp).unwrap();
            assert_eq!(header_len, 20);
            assert_eq!(rtp[1] & 0x7f, VIDEO_PT_KEY);
            assert_eq!((rtp[1] & 0x80) != 0, index == 2);
            let clear =
                unprotect_rtp(rtp, &keys, 0).expect("verify SRTP authentication and decrypt");
            let expected_flag = [0x80, 0x00, 0x40][index];
            assert_eq!(&clear.opus[..2], &[0x1c, expected_flag]);
            recovered.extend_from_slice(&clear.opus[2..]);
        }
        assert_eq!(recovered, annex_b);

        // Peer NACKs the middle RTP sequence. The engine must resend the
        // original authenticated packet from its short retransmission cache.
        let nack = build_nack_many(0x5678, 0x1234, &[1]);
        let feedback = wrap_media_packet(msg_types::VIDEO_FEC, 0xaabb_ccdd, &nack);
        relay
            .send_to(&feedback, backend.socket.local_addr().unwrap())
            .unwrap();
        let mut retransmitted = [0u8; 1600];
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut recovered_nack = false;
        while Instant::now() < deadline {
            let Ok((len, from)) = relay.recv_from(&mut retransmitted) else {
                continue;
            };
            assert_eq!(from.port(), backend.socket.local_addr().unwrap().port());
            if retransmitted[0] == msg_types::VIDEO_RTP
                && &retransmitted[5..len] == original_packets[1].as_slice()
            {
                recovered_nack = true;
                break;
            }
        }
        assert!(
            recovered_nack,
            "peer NACK should resend the original protected RTP packet"
        );

        backend.stop();
    }
}
