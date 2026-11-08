use crate::crypto::{
    aes256cbc_decrypt_nopad, aes256cbc_decrypt_nopad_chunked_reset_iv, derive_aes256_key_zalo_v2,
};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: usize,
}

pub struct DecryptResult {
    pub code: i32,
    pub err: String,
}
/// Magic header for Zalo ZDB4.0 backup containers (6 bytes)
pub const ZDB4_MAGIC_HEADER: &[u8; 6] = b"ZDB4.0";

/// Offset where Table of Contents (TOC) begins: 6 bytes magic + 8 bytes padding
pub const ZDB4_TOC_START_OFFSET: usize = 14;

/// SQLite database file magic header (16 bytes)
pub const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// XZ container stream header magic (\xFD7zXZ\x00)
pub const XZ_HEADER_MAGIC: &[u8; 6] = b"\xFD7zXZ\x00";

/// XZ stream footer trailer marker (2 bytes ASCII "YZ")
pub const XZ_FOOTER_MAGIC: &[u8; 2] = b"YZ";

/// Safe sequential byte slice cursor for bounds-checked binary parsing
#[derive(Debug, Clone)]
pub struct BufferCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BufferCursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.remaining() < len {
            return None;
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Some(slice)
    }

    pub fn read_u32_be(&mut self) -> Option<u32> {
        let bytes = self.read_bytes(4)?;
        Some(u32::from_be_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_u32_le(&mut self) -> Option<u32> {
        let bytes = self.read_bytes(4)?;
        Some(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_string_utf8_lossy(&mut self, len: usize) -> Option<String> {
        let bytes = self.read_bytes(len)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    pub fn remaining_slice(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

fn is_sqlite_db_file(p: &Path) -> bool {
    if let Ok(mut f) = File::open(p) {
        let mut hdr = [0u8; SQLITE_HEADER_MAGIC.len()];
        if f.read_exact(&mut hdr).is_ok() {
            return &hdr == SQLITE_HEADER_MAGIC;
        }
    }
    false
}

fn has_zdb4_magic_at0(pt: &[u8]) -> bool {
    pt.starts_with(ZDB4_MAGIC_HEADER)
}

fn strip_pkcs7_padding(buf: &[u8]) -> &[u8] {
    let Some(&last) = buf.last() else {
        return buf;
    };
    let pad = last as usize;
    if (1..=16).contains(&pad)
        && buf.len() >= pad
        && buf[buf.len() - pad..].iter().all(|&b| b == last)
    {
        &buf[..buf.len() - pad]
    } else {
        buf
    }
}

fn decompress_zdb4_payload(compressed: &[u8]) -> Result<Vec<u8>, String> {
    if compressed.starts_with(XZ_HEADER_MAGIC) {
        // Detect XZ stream boundary using the standard 2-byte footer "YZ"
        // to trim trailing random padding bytes left by Zalo's compression.
        let xz_end = compressed
            .windows(XZ_FOOTER_MAGIC.len())
            .rposition(|window| window == XZ_FOOTER_MAGIC)
            .map(|pos| pos + XZ_FOOTER_MAGIC.len())
            .unwrap_or(compressed.len());
        let mut out = Vec::new();
        if lzma_rs::xz_decompress(&mut &compressed[..xz_end], &mut out).is_ok() {
            return Ok(out);
        }
    }
    let unpadded = strip_pkcs7_padding(compressed);
    let mut out = Vec::new();
    if lzma_rs::lzma_decompress(&mut &unpadded[..], &mut out).is_ok() {
        return Ok(out);
    }
    let mut out_xz = Vec::new();
    if lzma_rs::xz_decompress(&mut &unpadded[..], &mut out_xz).is_ok() {
        return Ok(out_xz);
    }
    Err("Failed to decompress LZMA/XZ payload".to_string())
}

pub fn process_zdb4_v2<F>(
    input_path: &str,
    output_path: &str,
    private_key_str: &str,
    mut progress_cb: F,
) -> DecryptResult
where
    F: FnMut(),
{
    let ct_full = match fs::read(input_path) {
        Ok(b) => b,
        Err(e) => {
            return DecryptResult {
                code: -1,
                err: format!("Cannot open input: {}", e),
            }
        }
    };

    let ct_size_full = ct_full.len();
    if ct_size_full < 16 {
        return DecryptResult {
            code: -1,
            err: "Input too small".to_string(),
        };
    }

    struct CtSpan {
        off: usize,
        len: usize,
    }

    let mut spans = Vec::new();
    if ct_size_full % 16 == 0 {
        spans.push(CtSpan {
            off: 0,
            len: ct_size_full,
        });
    }
    if ct_size_full > 4 && (ct_size_full - 4) % 16 == 0 {
        spans.push(CtSpan {
            off: 4,
            len: ct_size_full - 4,
        });
        spans.push(CtSpan {
            off: 0,
            len: ct_size_full - 4,
        });
    }

    if spans.is_empty() {
        return DecryptResult {
            code: -1,
            err: "Ciphertext length not AES block aligned".to_string(),
        };
    }

    let aes_key = derive_aes256_key_zalo_v2(private_key_str);
    let iv_zero = [0u8; 16];

    let mut pt = Vec::new();
    let mut ok_magic = false;

    for sp in spans {
        let ct_slice = &ct_full[sp.off..sp.off + sp.len];

        // Try chunked reset IV first
        let tmp = if let Some(dec) =
            aes256cbc_decrypt_nopad_chunked_reset_iv(ct_slice, &aes_key)
        {
            if has_zdb4_magic_at0(&dec) {
                Some(dec)
            } else {
                aes256cbc_decrypt_nopad(ct_slice, &aes_key, &iv_zero)
            }
        } else {
            aes256cbc_decrypt_nopad(ct_slice, &aes_key, &iv_zero)
        };

        if let Some(dec) = tmp {
            if has_zdb4_magic_at0(&dec) {
                pt = dec;
                ok_magic = true;
                break;
            }
        }
    }

    if !ok_magic {
        return DecryptResult {
            code: -1,
            err: "Invalid ZDB4.0 Magic Header".to_string(),
        };
    }

    if pt.len() < ZDB4_TOC_START_OFFSET + 4 {
        return DecryptResult {
            code: -2,
            err: "Truncated container header".to_string(),
        };
    }

    let mut cursor = BufferCursor::new(&pt);
    // Advance past magic signature and reserved padding (14 bytes)
    cursor.read_bytes(ZDB4_TOC_START_OFFSET);

    let file_count = match cursor.read_u32_be() {
        Some(cnt) => cnt as usize,
        None => {
            return DecryptResult {
                code: -2,
                err: "Truncated file count".to_string(),
            };
        }
    };

    let mut files = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        let nlen = match cursor.read_u32_be() {
            Some(len) => len as usize,
            None => {
                return DecryptResult {
                    code: -2,
                    err: "Truncated file table".to_string(),
                };
            }
        };

        let name = match cursor.read_string_utf8_lossy(nlen) {
            Some(s) => s,
            None => {
                return DecryptResult {
                    code: -2,
                    err: "Truncated file table names".to_string(),
                };
            }
        };

        let size = match cursor.read_u32_be() {
            Some(sz) => sz as usize,
            None => {
                return DecryptResult {
                    code: -2,
                    err: "Truncated file table size".to_string(),
                };
            }
        };

        files.push(FileEntry { name, size });
    }

    // Remainder after file table is an XZ stream (LZMA2).
    let lzma_payload = cursor.remaining_slice();
    let decompressed = match decompress_zdb4_payload(lzma_payload) {
        Ok(data) => data,
        Err(e) => {
            return DecryptResult {
                code: -3,
                err: e,
            };
        }
    };

    let out_dir = Path::new(output_path);
    if let Err(e) = fs_create_dirs(out_dir) {
        return DecryptResult {
            code: -10,
            err: format!("Failed to create output directories: {}", e),
        };
    }

    let mut decomp_offset = 0usize;
    let mut sanitized_files = Vec::with_capacity(files.len());
    for file in &files {
        if decomp_offset + file.size > decompressed.len() {
            return DecryptResult {
                code: -13,
                err: "Not all files were extracted (truncated stream)".to_string(),
            };
        }

        let file_data = &decompressed[decomp_offset..decomp_offset + file.size];
        decomp_offset += file.size;

        // Path Traversal Sanitization: Prevent directory escape (e.g. "../../etc/passwd")
        let safe_name = match Path::new(&file.name)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        {
            Some(n) => n,
            None => {
                return DecryptResult {
                    code: -12,
                    err: format!("Unsafe or invalid file name in container TOC: {}", file.name),
                };
            }
        };

        let target_path = out_dir.join(safe_name);
        let mut out_file = match File::create(&target_path) {
            Ok(f) => f,
            Err(e) => {
                return DecryptResult {
                    code: -11,
                    err: format!("Failed to open output file: {}", e),
                };
            }
        };

        if let Err(e) = out_file.write_all(file_data) {
            return DecryptResult {
                code: -11,
                err: format!("Failed to write output file: {}", e),
            };
        }

        sanitized_files.push((safe_name.to_string(), target_path));

        // Notify JS progress callback after each file finishes
        progress_cb();
    }

    // Validate at least one extracted .db is a valid SQLite db
    let mut any_db = false;
    let mut any_valid_sqlite = false;

    for (name, path) in &sanitized_files {
        if name.ends_with(".db") {
            any_db = true;
            if is_sqlite_db_file(path) {
                any_valid_sqlite = true;
                break;
            }
        }
    }

    if any_db && !any_valid_sqlite {
        return DecryptResult {
            code: -14,
            err: "Extracted .db files are not valid SQLite databases".to_string(),
        };
    }

    if !any_db {
        return DecryptResult {
            code: -15,
            err: "No .db files were extracted".to_string(),
        };
    }

    DecryptResult {
        code: 0,
        err: String::new(),
    }
}

fn fs_create_dirs(p: &Path) -> std::io::Result<()> {
    if !p.exists() {
        fs::create_dir_all(p)
    } else {
        Ok(())
    }
}
