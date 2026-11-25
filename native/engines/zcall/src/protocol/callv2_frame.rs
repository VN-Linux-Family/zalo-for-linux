//! Frame parser and chunker for call-v2 socket transport.

use crate::crypto::callv2::{decrypt, encrypt};
use std::collections::HashMap;

const MAX_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAME_HEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHUNKS_PER_FRAME: usize = MAX_FRAME_HEX_BYTES.div_ceil(4000);

struct ChunkAssembly {
    total: usize,
    parts: Vec<Option<String>>,
    received_bytes: usize,
}

pub struct FrameParser {
    key: [u8; 16],
    buf: String,
    need_token: bool,
    chunks: HashMap<u64, ChunkAssembly>,
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

            // Multipart wire format: <hex>#<message-id>#<total>#<index>#$.
            if raw.contains('#') {
                match self.accept_chunk(&raw) {
                    Ok(Some(assembled)) => raw = assembled,
                    Ok(None) => continue,
                    Err(error) => {
                        events.push(FrameEvent::Error(error));
                        continue;
                    }
                }
            }
            if raw.len() > MAX_FRAME_HEX_BYTES
                || !raw.len().is_multiple_of(2)
                || !raw.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                events.push(FrameEvent::Error(
                    "invalid or oversized call-v2 frame".into(),
                ));
                continue;
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

        if self.buf.len() > MAX_BUFFER_BYTES {
            self.buf.clear();
            self.chunks.clear();
            events.push(FrameEvent::Error(
                "call-v2 receive buffer exceeded limit".into(),
            ));
        }
        events
    }

    fn accept_chunk(&mut self, raw: &str) -> Result<Option<String>, String> {
        let fields: Vec<&str> = raw.split('#').collect();
        if fields.len() != 5 || !fields[4].is_empty() || fields[0].is_empty() {
            return Err("malformed call-v2 multipart frame".into());
        }
        let id = fields[1]
            .parse::<u64>()
            .map_err(|_| "invalid call-v2 multipart message id")?;
        let total = fields[2]
            .parse::<usize>()
            .map_err(|_| "invalid call-v2 multipart count")?;
        let index = fields[3]
            .parse::<usize>()
            .map_err(|_| "invalid call-v2 multipart index")?;
        if total == 0
            || total > MAX_CHUNKS_PER_FRAME
            || index >= total
            || fields[0].len() > 4000
            || !fields[0].len().is_multiple_of(2)
            || !fields[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid or oversized call-v2 multipart chunk".into());
        }

        if !self.chunks.contains_key(&id) && self.chunks.len() >= 128 {
            return Err("too many incomplete call-v2 multipart frames".into());
        }
        let assembly = self.chunks.entry(id).or_insert_with(|| ChunkAssembly {
            total,
            parts: vec![None; total],
            received_bytes: 0,
        });
        if assembly.total != total {
            self.chunks.remove(&id);
            return Err("inconsistent call-v2 multipart chunk count".into());
        }
        if let Some(existing) = assembly.parts[index].as_ref() {
            if existing != fields[0] {
                self.chunks.remove(&id);
                return Err("conflicting duplicate call-v2 multipart chunk".into());
            }
        } else {
            assembly.received_bytes += fields[0].len();
            if assembly.received_bytes > MAX_FRAME_HEX_BYTES {
                self.chunks.remove(&id);
                return Err("oversized assembled call-v2 frame".into());
            }
            assembly.parts[index] = Some(fields[0].to_owned());
        }

        if assembly.parts.iter().all(Option::is_some) {
            let assembly = self.chunks.remove(&id).expect("completed assembly exists");
            Ok(Some(assembly.parts.into_iter().flatten().collect()))
        } else {
            Ok(None)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_json_frame_roundtrips_across_socket_fragmentation() {
        let key = [0x42; 16];
        let value = serde_json::json!({"type":"request","data":{"text":"hello"}});
        let wire = format_outgoing_frames(&value, &key);
        let mut parser = FrameParser::new(key, false);
        let mut events = Vec::new();
        for bytes in wire.chunks(7) {
            events.extend(parser.push(bytes));
        }
        assert!(matches!(events.as_slice(), [FrameEvent::Json(decoded)] if decoded == &value));
    }

    #[test]
    fn multipart_frame_reassembles_out_of_order_without_duplicate_inflation() {
        let key = [0x24; 16];
        let value = serde_json::json!({"payload":"x".repeat(5000)});
        let wire = format_outgoing_frames(&value, &key);
        let parts: Vec<&[u8]> = wire
            .split(|byte| *byte == b'$')
            .filter(|part| !part.is_empty())
            .collect();
        assert!(parts.len() > 1);
        let mut parser = FrameParser::new(key, false);
        let mut events = Vec::new();
        for part in parts.iter().rev() {
            let mut framed = part.to_vec();
            framed.push(b'$');
            events.extend(parser.push(&framed));
        }
        assert!(matches!(events.as_slice(), [FrameEvent::Json(decoded)] if decoded == &value));
        assert!(parser.chunks.is_empty());
    }

    #[test]
    fn malformed_multipart_metadata_is_rejected_without_allocating() {
        let mut parser = FrameParser::new([0x42; 16], false);
        let events = parser.push(b"aa#1#0#0#$");
        assert!(matches!(events.as_slice(), [FrameEvent::Error(_)]));
        assert!(parser.chunks.is_empty());
    }

    #[test]
    fn conflicting_duplicate_chunk_discards_assembly() {
        let mut parser = FrameParser::new([0x42; 16], false);
        assert!(parser.push(b"aa#1#2#0#$").is_empty());
        let events = parser.push(b"bb#1#2#0#$");
        assert!(matches!(events.as_slice(), [FrameEvent::Error(_)]));
        assert!(parser.chunks.is_empty());
    }
}
