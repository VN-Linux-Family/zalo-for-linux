//! 1:1 video send-rate controller.
//!
//! TWCC queue delay and loss are primary; receiver reports are a fallback
//! loss signal. Missing transport feedback reduces the rate after a bounded
//! timeout. Rung changes are consumed by the Rust camera capture/encoder.
use crate::protocol::rtcp::{ReportBlock, TwccFeedback};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

const START_KBPS: u32 = 300;
const MIN_KBPS: u32 = 100;
const DOWN_GAP_MS: u64 = 2_000;
const UP_HOLD_S: u8 = 4;
const BACKOFF_MIN_MS: u64 = 10_000;
const BACKOFF_MAX_MS: u64 = 60_000;
const FEEDBACK_TIMEOUT_S: u8 = 2;
const QUEUE_HOLD_MS: i64 = 40;
const QUEUE_DOWN_MS: i64 = 150;
const DOWNLINK_HOLD: f64 = 0.02;
const DOWNLINK_DOWN: f64 = 0.10;
const BASE_WINDOW_MS: u64 = 15_000;
const MIN_TWCC_PACKETS: u64 = 10;
const SENT_KEEP: usize = 4_000;
const DEFAULT_LADDER: [(u32, u32, u32, u32, u32); 7] = [
    (1, 426, 240, 8, 30),
    (2, 426, 240, 10, 100),
    (3, 640, 360, 15, 300),
    (4, 640, 360, 20, 500),
    (5, 640, 360, 24, 700),
    (6, 848, 480, 24, 900),
    (7, 1280, 720, 24, 1100),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoProfile {
    pub rung: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub keyframe_interval_ms: u64,
}

#[derive(Clone, Debug)]
struct Rung {
    profile: VideoProfile,
    kbps: u32,
}

#[derive(Default)]
struct FeedbackWindow {
    received: u64,
    lost: u64,
    rr_loss: Vec<f64>,
    delays_ms: Vec<i64>,
}

pub struct SendRateController {
    local_ssrc: u32,
    ladder: Vec<Rung>,
    level: usize,
    ceiling: usize,
    fixed: bool,
    high_loss: f64,
    low_loss: f64,
    sent: HashMap<u16, u64>,
    window: FeedbackWindow,
    min_delays: VecDeque<(u64, i64)>,
    no_feedback: u8,
    good_seconds: u8,
    last_down_at: u64,
    last_up_at: u64,
    backoff_ms: u64,
    last_profile: VideoProfile,
    pub totals: RateTotals,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RateTotals {
    pub twcc_packets: u64,
    pub twcc_lost: u64,
    pub rr_blocks: u64,
    pub changes: u64,
    pub downs: u64,
    pub ups: u64,
    pub feedback_timeouts: u64,
}

fn u64_to_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or_default();
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or_default();
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

fn configured_ladder(
    config: &Value,
    max_width: u32,
    max_height: u32,
    max_fps: u32,
    max_kbps: u32,
    keyframe_interval_ms: u64,
) -> Vec<Rung> {
    config
        .get("bwProfiles")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .map_or_default(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let res = row.get("res").and_then(Value::as_u64)? as u32;
                    let fps = row.get("fps").and_then(Value::as_u64).unwrap_or(15) as u32;
                    let kbps = row.get("bitrate").and_then(Value::as_u64)? as u32;
                    let (width, height) = dimensions(res)?;
                    if kbps < MIN_KBPS
                        || kbps > max_kbps
                        || width > max_width
                        || height > max_height
                    {
                        return None;
                    }
                    Some(Rung {
                        profile: VideoProfile {
                            rung: id,
                            width,
                            height,
                            fps: fps.min(max_fps).max(1),
                            bitrate: kbps.saturating_mul(1000),
                            keyframe_interval_ms,
                        },
                        kbps,
                    })
                })
                .collect()
        })
}

fn default_ladder(
    max_width: u32,
    max_height: u32,
    max_fps: u32,
    max_kbps: u32,
    keyframe_interval_ms: u64,
) -> Vec<Rung> {
    DEFAULT_LADDER
        .iter()
        .filter(|(_, width, height, _, kbps)| {
            *kbps >= MIN_KBPS && *kbps <= max_kbps && *width <= max_width && *height <= max_height
        })
        .map(|(id, width, height, fps, kbps)| Rung {
            profile: VideoProfile {
                rung: *id,
                width: *width,
                height: *height,
                fps: (*fps).min(max_fps).max(1),
                bitrate: kbps.saturating_mul(1000),
                keyframe_interval_ms,
            },
            kbps: *kbps,
        })
        .collect()
}

impl SendRateController {
    pub fn new(config: &Value, local_ssrc: u32) -> Self {
        let max_width = positive(config, "maxWidth", 1280);
        let max_height = positive(config, "maxHeight", 720);
        let max_fps = positive(config, "codecMaxFps", 24);
        let max_kbps = positive(config, "maxBitrate", 1500);
        let keyframe_interval_ms = u64::from(positive(config, "keyFrameIntervalMs", 10_000));

        let mut ladder = configured_ladder(
            config,
            max_width,
            max_height,
            max_fps,
            max_kbps,
            keyframe_interval_ms,
        );
        if ladder.is_empty() {
            ladder = default_ladder(
                max_width,
                max_height,
                max_fps,
                max_kbps,
                keyframe_interval_ms,
            );
        }
        if ladder.is_empty() {
            ladder.push(Rung {
                profile: VideoProfile {
                    rung: 2,
                    width: 426,
                    height: 240,
                    fps: 10,
                    bitrate: 100_000,
                    keyframe_interval_ms,
                },
                kbps: 100,
            });
        }
        ladder.sort_by_key(|rung| rung.kbps);

        let requested_start = std::env::var("ZCALL_VIDEO_RUNG")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let startup = positive(config, "startupBitrate", START_KBPS);
        let mut level = 0usize;
        for (index, rung) in ladder.iter().enumerate() {
            if requested_start == Some(rung.profile.rung) || rung.kbps <= startup {
                level = index;
            }
        }

        let last_profile = ladder[level].profile.clone();
        Self {
            local_ssrc,
            ceiling: ladder.len().saturating_sub(1),
            ladder,
            level,
            fixed: std::env::var("ZCALL_VIDEO_ADAPT").is_ok_and(|value| value == "0"),
            high_loss: f64::from(positive(config, "bweSendSideHighLoss", 10)) / 100.0,
            low_loss: f64::from(positive(config, "bweSendSideLowLoss", 2)) / 100.0,
            sent: HashMap::new(),
            window: FeedbackWindow::default(),
            min_delays: VecDeque::new(),
            no_feedback: 0,
            good_seconds: 0,
            last_down_at: 0,
            last_up_at: 0,
            backoff_ms: BACKOFF_MIN_MS,
            last_profile,
            totals: RateTotals::default(),
        }
    }

    #[must_use]
    pub fn profile(&self) -> VideoProfile {
        self.ladder[self.level].profile.clone()
    }

    #[must_use]
    pub const fn rung_count(&self) -> usize {
        self.ladder.len()
    }

    pub fn on_sent(&mut self, twcc_sequence: u16, now_ms: u64) {
        self.sent.insert(twcc_sequence, now_ms);
        while self.sent.len() > SENT_KEEP {
            if let Some(oldest) = self.sent.keys().next().copied() {
                self.sent.remove(&oldest);
            } else {
                break;
            }
        }
    }

    pub fn on_feedback(
        &mut self,
        feedback: &[TwccFeedback],
        report_blocks: &[ReportBlock],
        now_ms: u64,
    ) {
        for report in report_blocks {
            if report.ssrc == self.local_ssrc {
                self.window
                    .rr_loss
                    .push(f64::from(report.fraction_lost) / 256.0);
                self.totals.rr_blocks = self.totals.rr_blocks.saturating_add(1);
            }
        }

        for packet in feedback {
            if packet.sender_ssrc == self.local_ssrc {
                continue;
            }
            self.totals.twcc_packets = self
                .totals
                .twcc_packets
                .saturating_add(u64::from(packet.count));
            let mut arrival_ms = i64::from(packet.reference_time) * 64;
            let mut delta_index = 0usize;
            for (index, status) in packet.status.iter().copied().enumerate() {
                if status == 0 {
                    self.window.lost = self.window.lost.saturating_add(1);
                    self.totals.twcc_lost = self.totals.twcc_lost.saturating_add(1);
                    continue;
                }
                self.window.received = self.window.received.saturating_add(1);
                let Some(delta) = packet.delta_q.get(delta_index).copied() else {
                    continue;
                };
                delta_index += 1;
                arrival_ms += i64::from(delta) / 4;
                let sequence = packet.base.wrapping_add(index as u16);
                if let Some(sent_at) = self.sent.get(&sequence) {
                    self.window
                        .delays_ms
                        .push(arrival_ms - sent_at.cast_signed());
                }
            }
        }
        if self.sent.len() > SENT_KEEP {
            self.sent
                .retain(|_, timestamp| now_ms.saturating_sub(*timestamp) <= 30_000);
        }
    }

    /// `downlink_loss` is the fraction of remote RTP packets not received in the
    /// previous interval. Returns a new profile only when a rung changes.
    pub fn tick(
        &mut self,
        now_ms: u64,
        sending_video: bool,
        downlink_loss: Option<f64>,
    ) -> Option<VideoProfile> {
        let window = std::mem::take(&mut self.window);
        let total = window.received.saturating_add(window.lost);
        let twcc_valid = total >= MIN_TWCC_PACKETS;
        let loss = if twcc_valid {
            Some(u64_to_f64(window.lost) / u64_to_f64(total))
        } else {
            window.rr_loss.iter().copied().reduce(f64::max)
        };

        let queue_ms =
            if window.delays_ms.is_empty() {
                None
            } else {
                let mut sorted = window.delays_ms;
                sorted.sort_unstable();
                let minimum = sorted[0];
                self.min_delays.push_back((now_ms, minimum));
                while self.min_delays.front().is_some_and(|(timestamp, _)| {
                    now_ms.saturating_sub(*timestamp) > BASE_WINDOW_MS
                }) {
                    self.min_delays.pop_front();
                }
                let baseline = self
                    .min_delays
                    .iter()
                    .map(|(_, delay)| *delay)
                    .min()
                    .unwrap_or(minimum);
                Some((sorted[sorted.len() / 2] - baseline).max(0))
            };

        self.no_feedback = if twcc_valid {
            0
        } else {
            self.no_feedback.saturating_add(1)
        };
        if !self.fixed && sending_video {
            let mut down_reason = None;
            let hold;
            if self.no_feedback >= FEEDBACK_TIMEOUT_S && self.totals.twcc_packets > 0 {
                down_reason = Some("transport-cc feedback timeout");
                self.totals.feedback_timeouts = self.totals.feedback_timeouts.saturating_add(1);
            } else if loss.is_some_and(|value| value > self.high_loss) {
                down_reason = Some("high packet loss");
            } else if queue_ms.is_some_and(|value| value > QUEUE_DOWN_MS) {
                down_reason = Some("high queueing delay");
            } else if downlink_loss.is_some_and(|value| value > DOWNLINK_DOWN) {
                down_reason = Some("high reverse-path loss");
            } else {
                hold = !twcc_valid
                    || loss.is_none_or(|value| value >= self.low_loss)
                    || queue_ms.is_none_or(|value| value > QUEUE_HOLD_MS)
                    || downlink_loss.is_some_and(|value| value > DOWNLINK_HOLD);
                if hold {
                    self.good_seconds = 0;
                } else {
                    self.good_seconds = self.good_seconds.saturating_add(1);
                    if self.good_seconds >= UP_HOLD_S
                        && self.level < self.ceiling
                        && now_ms.saturating_sub(self.last_down_at) >= self.backoff_ms
                    {
                        self.good_seconds = 0;
                        self.last_up_at = now_ms;
                        self.level += 1;
                        self.totals.ups = self.totals.ups.saturating_add(1);
                    }
                }
            }

            if down_reason.is_some() {
                self.good_seconds = 0;
                if self.level > 0 && now_ms.saturating_sub(self.last_down_at) >= DOWN_GAP_MS {
                    self.backoff_ms = if now_ms.saturating_sub(self.last_up_at) < 15_000 {
                        (self.backoff_ms * 2).min(BACKOFF_MAX_MS)
                    } else {
                        BACKOFF_MIN_MS
                    };
                    self.last_down_at = now_ms;
                    self.totals.downs = self.totals.downs.saturating_add(1);
                    self.level -= 1;
                }
            }
        }

        let current = self.profile();
        if current == self.last_profile {
            None
        } else {
            self.last_profile = current.clone();
            self.totals.changes = self.totals.changes.saturating_add(1);
            Some(current)
        }
    }

    #[must_use]
    pub fn totals(&self) -> RateTotals {
        self.totals.clone()
    }
}

const fn dimensions(resolution: u32) -> Option<(u32, u32)> {
    match resolution {
        240 => Some((426, 240)),
        360 => Some((640, 360)),
        480 => Some((848, 480)),
        720 => Some((1280, 720)),
        _ => None,
    }
}

fn positive(config: &Value, key: &str, fallback: u32) -> u32 {
    config
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

#[cfg(test)]
#[path = "../tests/media/rate.rs"]
mod tests;
