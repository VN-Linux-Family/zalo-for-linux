use crate::container::BufferCursor;
use std::collections::HashMap;

/// Size of Tag (4 bytes) + Length (4 bytes) in a TLV record header
pub const TLV_RECORD_HEADER_SIZE: usize = 8;

#[derive(Debug, Default, Clone)]
pub struct TlvBoxLite {
    pub values: HashMap<u32, Vec<Vec<u8>>>,
    pub little_endian_used: bool,
}

impl TlvBoxLite {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
            little_endian_used: true,
        }
    }

    pub fn parse_core(&mut self, buf: &[u8], little_endian: bool) -> Result<(), String> {
        self.values.clear();
        if buf.is_empty() {
            return Err("Empty buffer".to_string());
        }

        let mut cursor = BufferCursor::new(buf);
        while cursor.remaining() >= TLV_RECORD_HEADER_SIZE {
            let key = if little_endian {
                cursor.read_u32_le()
            } else {
                cursor.read_u32_be()
            }
            .ok_or_else(|| "Failed to read TLV key".to_string())?;

            let len = (if little_endian {
                cursor.read_u32_le()
            } else {
                cursor.read_u32_be()
            }
            .ok_or_else(|| "Failed to read TLV length".to_string())?) as usize;

            if cursor.remaining() < len {
                return Err("TLV length out of bounds".to_string());
            }

            let value = cursor
                .read_bytes(len)
                .ok_or_else(|| "TLV payload truncated".to_string())?;

            self.values.entry(key).or_default().push(value.to_vec());
        }

        if cursor.remaining() > TLV_RECORD_HEADER_SIZE {
            return Err("Trailing bytes".to_string());
        }
        Ok(())
    }

    pub fn score_for_binnet_shape(&self) -> i32 {
        let mut score = 0;
        const EXPECTED_TOP: &[u32] = &[0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b];
        const EXPECTED_ATTACH: &[u32] = &[
            0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35,
            0x36, 0x37, 0x40, 0x41, 0x42, 0x43, 0x44,
        ];

        for &k in EXPECTED_TOP {
            if self.values.contains_key(&k) {
                score += 6;
            }
        }
        for &k in EXPECTED_ATTACH {
            if self.values.contains_key(&k) {
                score += 4;
            }
        }
        for &k in self.values.keys() {
            if k <= 0x80 {
                score += 1;
            }
            if k > 0x10000 {
                score -= 3;
            }
        }
        score
    }

    pub fn parse(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut le_try = TlvBoxLite::new();
        let le_ok = le_try.parse_core(buf, true);
        let le_score = if le_ok.is_ok() {
            le_try.score_for_binnet_shape()
        } else {
            -1_000_000
        };

        let mut be_try = TlvBoxLite::new();
        let be_ok = be_try.parse_core(buf, false);
        let be_score = if be_ok.is_ok() {
            be_try.score_for_binnet_shape()
        } else {
            -1_000_000
        };

        if le_ok.is_err() && be_ok.is_err() {
            return Err(le_ok.err().or_else(|| be_ok.err()).unwrap());
        }

        if be_score > le_score {
            self.values = be_try.values;
            self.little_endian_used = false;
        } else {
            self.values = le_try.values;
            self.little_endian_used = true;
        }
        Ok(())
    }

    pub fn get_all_keys(&self) -> Vec<u32> {
        let mut keys: Vec<u32> = self.values.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    pub fn get_int_value(&self, key: u32) -> Option<i32> {
        let vals = self.values.get(&key)?;
        let v = vals.first()?;
        let mut cursor = BufferCursor::new(v);
        if self.little_endian_used {
            cursor.read_u32_le().map(|n| n as i32)
        } else {
            cursor.read_u32_be().map(|n| n as i32)
        }
    }

    pub fn get_int64_value(&self, key: u32) -> Option<i64> {
        let vals = self.values.get(&key)?;
        let v = vals.first()?;
        let mut cursor = BufferCursor::new(v);
        if cursor.remaining() >= 8 {
            let bytes = cursor.read_bytes(8)?;
            if self.little_endian_used {
                Some(i64::from_le_bytes(bytes.try_into().unwrap()))
            } else {
                Some(i64::from_be_bytes(bytes.try_into().unwrap()))
            }
        } else if cursor.remaining() >= 4 {
            if self.little_endian_used {
                cursor.read_u32_le().map(|n| n as i32 as i64)
            } else {
                cursor.read_u32_be().map(|n| n as i32 as i64)
            }
        } else {
            None
        }
    }

    pub fn get_bytes_value(&self, key: u32) -> Option<&[u8]> {
        let vals = self.values.get(&key)?;
        vals.first().map(|v| v.as_slice())
    }
}

pub fn is_likely_utf8_printable(v: &[u8]) -> bool {
    if v.is_empty() {
        return true;
    }
    let mut good = 0usize;
    let mut bad = 0usize;
    let mut i = 0usize;
    while i < v.len() {
        let b = v[i];
        if b == 0 {
            return false;
        }
        if b == 9 || b == 10 || b == 13 {
            good += 1;
            i += 1;
            continue;
        }
        if b < 0x20 {
            bad += 1;
            i += 1;
            continue;
        }
        if b <= 0x7E {
            good += 1;
            i += 1;
            continue;
        }

        if (0xC2..=0xDF).contains(&b) {
            if i + 2 > v.len() {
                return false;
            }
            if (v[i + 1] & 0xC0) != 0x80 {
                bad += 1;
                i += 1;
                continue;
            }
            good += 2;
            i += 2;
            continue;
        }
        if (0xE0..=0xEF).contains(&b) {
            if i + 3 > v.len() {
                return false;
            }
            if (v[i + 1] & 0xC0) != 0x80 || (v[i + 2] & 0xC0) != 0x80 {
                bad += 1;
                i += 1;
                continue;
            }
            good += 3;
            i += 3;
            continue;
        }
        if (0xF0..=0xF4).contains(&b) {
            if i + 4 > v.len() {
                return false;
            }
            if (v[i + 1] & 0xC0) != 0x80 || (v[i + 2] & 0xC0) != 0x80 || (v[i + 3] & 0xC0) != 0x80 {
                bad += 1;
                i += 1;
                continue;
            }
            good += 4;
            i += 4;
            continue;
        }

        bad += 1;
        i += 1;
    }
    bad == 0 && good * 20 >= v.len() * 19
}

pub fn try_decode_utf16_le_printable(v: &[u8]) -> Option<String> {
    if v.len() < 4 || v.len() % 2 != 0 {
        return None;
    }
    let units = v.len() / 2;
    let mut zero_hi = 0usize;
    let mut out = String::with_capacity(units);

    for chunk in v.chunks_exact(2) {
        let lo = chunk[0];
        let hi = chunk[1];
        if hi == 0 {
            zero_hi += 1;
            if (0x20..=0x7e).contains(&lo) || lo == 9 || lo == 10 || lo == 13 {
                out.push(lo as char);
            }
        }
    }
    if out.is_empty() || zero_hi * 100 < units * 60 {
        None
    } else {
        Some(out)
    }
}

pub fn try_decode_text_smart(v: &[u8]) -> Option<String> {
    if is_likely_utf8_printable(v) {
        Some(String::from_utf8_lossy(v).into_owned())
    } else {
        try_decode_utf16_le_printable(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tlv_parse_le() {
        let mut buf = Vec::new();
        // key 0x05, len 4, val 1234
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&1234i32.to_le_bytes());

        let mut tlv = TlvBoxLite::new();
        assert!(tlv.parse(&buf).is_ok());
        assert_eq!(tlv.get_int_value(5), Some(1234));
    }

    #[test]
    fn test_is_likely_utf8() {
        assert!(is_likely_utf8_printable(b"Hello world!"));
        assert!(!is_likely_utf8_printable(&[0, 1, 2, 3]));
    }
}
