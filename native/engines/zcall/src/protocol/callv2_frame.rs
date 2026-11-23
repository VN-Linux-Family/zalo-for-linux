//! Frame parser and chunker for call-v2 socket transport.

use crate::crypto::callv2::{decrypt, encrypt};
use std::collections::HashMap;

pub struct FrameParser {
    key: [u8; 16],
    buf: String,
    need_token: bool,
    chunks: HashMap<u64, Vec<Option<String>>>,
}

#[derive(Debug)]
pub enum FrameEvent {
    Token(String),
    Json(serde_json::Value),
    Raw(String),
    Error(String),
}

impl FrameParser {
    #[must_use]
    pub fn new(key: [u8; 16], expect_token: bool) -> Self {
        Self {
            key,
            buf: String::new(),
            need_token: expect_token,
            chunks: HashMap::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<FrameEvent> {
        let mut events = Vec::new();
        // Socket carries ascii/latin1 hex strings
        self.buf.push_str(&String::from_utf8_lossy(chunk));

        if self.need_token {
            if let Some(nl) = self.buf.find('\n') {
                let first = self.buf[..nl].trim().to_string();
                if !first.is_empty() && !first.chars().all(|c| c.is_ascii_hexdigit() || c == '$') {
                    events.push(FrameEvent::Token(first));
                    self.buf = self.buf[nl + 1..].to_string();
                }
                self.need_token = false;
            }
        }

        while let Some(pos) = self.buf.find('$') {
            let mut raw = self.buf[..pos].trim().to_string();
            self.buf = self.buf[pos + 1..].to_string();

            if raw.is_empty() {
                continue;
            }

            // Multipart check: "<hex>#<msgId>#<total>#<index>#"
            if raw.ends_with('#') {
                let parts: Vec<&str> = raw.split('#').collect();
                if parts.len() >= 5 {
                    let hex_part = parts[0];
                    if let (Ok(id), Ok(total), Ok(index)) = (
                        parts[1].parse::<u64>(),
                        parts[2].parse::<usize>(),
                        parts[3].parse::<usize>(),
                    ) {
                        let entry = self.chunks.entry(id).or_insert_with(|| vec![None; total]);
                        if entry.len() < total {
                            entry.resize(total, None);
                        }
                        if index < entry.len() {
                            entry[index] = Some(hex_part.to_string());
                        }

                        if entry.iter().all(std::option::Option::is_some) {
                            raw = entry
                                .iter()
                                .filter_map(std::clone::Clone::clone)
                                .collect::<String>();
                            self.chunks.remove(&id);
                        } else {
                            continue;
                        }
                    }
                }
            }

            match decrypt(&raw, &self.key) {
                Ok(bytes) => {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        events.push(FrameEvent::Json(val));
                    } else if let Ok(s) = String::from_utf8(bytes) {
                        events.push(FrameEvent::Raw(s));
                    }
                }
                Err(e) => {
                    events.push(FrameEvent::Error(format!(
                        "{}: {}",
                        e,
                        &raw[..std::cmp::min(raw.len(), 60)]
                    )));
                }
            }
        }

        events
    }
}

#[must_use]
pub fn format_outgoing_frames(value: &serde_json::Value, key: &[u8; 16]) -> Vec<u8> {
    let json_bytes = serde_json::to_vec(value).unwrap_or_default();
    let hex = encrypt(&json_bytes, key);
    let mut out = Vec::new();

    if hex.len() <= 4000 {
        out.extend_from_slice(hex.as_bytes());
        out.push(b'$');
    } else {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let total = hex.len().div_ceil(4000);
        for i in 0..total {
            let start = i * 4000;
            let end = std::cmp::min(hex.len(), (i + 1) * 4000);
            let part = &hex[start..end];
            let chunk_str = format!("{part}#{id}#{total}#{i}#$");
            out.extend_from_slice(chunk_str.as_bytes());
        }
    }

    out
}
