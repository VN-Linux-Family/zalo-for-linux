//! SRTP AES_CM_128_HMAC_SHA1_80 (RFC 3711) implementation.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;

pub struct SrtpKeys {
    pub enc: [u8; 16],
    pub auth: [u8; 20],
    pub salt: [u8; 14],
}

pub struct UnprotectedRtp {
    pub seq: u16,
    pub ssrc: u32,
    pub opus: Vec<u8>,
}

fn aes_ecb_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(key.into());
    let mut out = *block;
    cipher.encrypt_block((&mut out).into());
    out
}

fn aes_cm_expand(key: &[u8; 16], iv16: &[u8; 16], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut counter: u16 = 0;
    while out.len() < n {
        let mut blk = *iv16;
        blk[14] = (counter >> 8) as u8;
        blk[15] = (counter & 0xff) as u8;
        let enc = aes_ecb_block(key, &blk);
        let take = std::cmp::min(16, n - out.len());
        out.extend_from_slice(&enc[..take]);
        counter = counter.wrapping_add(1);
    }
    out
}

fn kdf(master_key: &[u8; 16], master_salt: &[u8], label: u8, out_len: usize) -> Vec<u8> {
    let mut x = [0u8; 16];
    let len = std::cmp::min(14, master_salt.len());
    x[..len].copy_from_slice(&master_salt[..len]);
    x[7] ^= label;
    aes_cm_expand(master_key, &x, out_len)
}

pub fn session_keys(master30: &[u8]) -> Result<SrtpKeys, String> {
    if master30.len() < 30 {
        return Err("master key+salt must be at least 30 bytes".to_string());
    }
    let mut mk = [0u8; 16];
    mk.copy_from_slice(&master30[..16]);
    let ms = &master30[16..30];

    let enc_bytes = kdf(&mk, ms, 0x00, 16);
    let auth_bytes = kdf(&mk, ms, 0x01, 20);
    let salt_bytes = kdf(&mk, ms, 0x02, 14);

    let mut enc = [0u8; 16];
    enc.copy_from_slice(&enc_bytes);
    let mut auth = [0u8; 20];
    auth.copy_from_slice(&auth_bytes);
    let mut salt = [0u8; 14];
    salt.copy_from_slice(&salt_bytes);

    Ok(SrtpKeys { enc, auth, salt })
}

pub fn master_from_sess_id(sess_id: &[u8]) -> Result<[u8; 30], String> {
    if sess_id.len() < 30 {
        return Err("sessId shorter than 30".to_string());
    }
    let mut m = [0u8; 30];
    m.copy_from_slice(&sess_id[..30]);
    Ok(m)
}

pub fn packet_iv(session_salt: &[u8; 14], ssrc: u32, seq: u16, roc: u32) -> [u8; 16] {
    let idx = ((roc as u64) << 16) | (seq as u64 & 0xffff);
    let shifted = idx << 16;
    let mut iv = [0u8; 16];
    iv[4..8].copy_from_slice(&ssrc.to_be_bytes());
    iv[8..12].copy_from_slice(&((shifted >> 32) as u32).to_be_bytes());
    iv[12..16].copy_from_slice(&(shifted as u32).to_be_bytes());
    for i in 0..14 {
        iv[i] ^= session_salt[i];
    }
    iv
}

pub fn parse_rtp_header_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 12 {
        return None;
    }
    let x = (buf[0] >> 4) & 1;
    let cc = (buf[0] & 0x0f) as usize;
    let mut off = 12 + 4 * cc;
    if x != 0 {
        if buf.len() < off + 4 {
            return None;
        }
        let words = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
        off += 4 + 4 * words;
    }
    if off > buf.len() {
        return None;
    }
    Some(off)
}

pub fn unprotect_rtp(rtp_buf: &[u8], keys: &SrtpKeys, roc: u32) -> Option<UnprotectedRtp> {
    unprotect_rtp_with_profile(rtp_buf, keys, roc, 0, 10)
}

/// Authenticate and decrypt SRTP with an optional RFC 3711 MKI field.
///
/// Wire order is RTP header | encrypted payload | MKI | authentication tag.
/// RFC 3711 excludes MKI from the authenticated portion, so the candidate
/// MKI bytes are skipped but never treated as ciphertext. `tag_len` is
/// negotiated by the SRTP profile; group receive probes only common lengths
/// and accepts a profile only after its HMAC verifies.
pub fn unprotect_rtp_with_profile(
    rtp_buf: &[u8],
    keys: &SrtpKeys,
    roc: u32,
    mki_len: usize,
    tag_len: usize,
) -> Option<UnprotectedRtp> {
    if mki_len > 16 || !(4..=20).contains(&tag_len) {
        return None;
    }
    let hdr_len = parse_rtp_header_len(rtp_buf)?;
    if rtp_buf.len() < hdr_len + mki_len + tag_len {
        return None;
    }
    let seq = u16::from_be_bytes([rtp_buf[2], rtp_buf[3]]);
    let ssrc = u32::from_be_bytes([rtp_buf[8], rtp_buf[9], rtp_buf[10], rtp_buf[11]]);

    let tag_start = rtp_buf.len() - tag_len;
    let mki_start = tag_start - mki_len;
    let hdr = &rtp_buf[..hdr_len];
    let body = &rtp_buf[hdr_len..mki_start];
    let tag = &rtp_buf[tag_start..];

    let roc_bytes = roc.to_be_bytes();
    let mac = hmac_sha1(&keys.auth, &[hdr, body, &roc_bytes]);
    if mac[..tag_len] != *tag {
        return None;
    }

    let iv = packet_iv(&keys.salt, ssrc, seq, roc);
    let ks = aes_cm_expand(&keys.enc, &iv, body.len());
    let mut plain = vec![0u8; body.len()];
    for i in 0..body.len() {
        plain[i] = body[i] ^ ks[i];
    }

    Some(UnprotectedRtp {
        seq,
        ssrc,
        opus: plain,
    })
}

pub fn protect_rtp(
    rtp_header_and_ext: &[u8],
    opus: &[u8],
    keys: &SrtpKeys,
    seq: u16,
    ssrc: u32,
    roc: u32,
) -> Vec<u8> {
    let iv = packet_iv(&keys.salt, ssrc, seq, roc);
    let ks = aes_cm_expand(&keys.enc, &iv, opus.len());
    let mut body = vec![0u8; opus.len()];
    for i in 0..opus.len() {
        body[i] = opus[i] ^ ks[i];
    }

    let roc_bytes = roc.to_be_bytes();
    let tag = hmac_sha1_80(&keys.auth, &[rtp_header_and_ext, &body, &roc_bytes]);

    let mut out = Vec::with_capacity(rtp_header_and_ext.len() + body.len() + 10);
    out.extend_from_slice(rtp_header_and_ext);
    out.extend_from_slice(&body);
    out.extend_from_slice(&tag);
    out
}

pub fn hmac_sha1_80(key: &[u8], chunks: &[&[u8]]) -> [u8; 10] {
    let full = hmac_sha1(key, chunks);
    let mut out = [0u8; 10];
    out.copy_from_slice(&full[..10]);
    out
}

pub fn hmac_sha1(key: &[u8], chunks: &[&[u8]]) -> [u8; 20] {
    let mut k_pad = [0u8; 64];
    if key.len() > 64 {
        let hash = sha1(&[key]);
        k_pad[..20].copy_from_slice(&hash);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k_pad[i];
        opad[i] ^= k_pad[i];
    }

    let mut inner_chunks = Vec::with_capacity(chunks.len() + 1);
    inner_chunks.push(&ipad[..]);
    inner_chunks.extend_from_slice(chunks);
    let inner_hash = sha1(&inner_chunks);

    sha1(&[&opad[..], &inner_hash[..]])
}

pub fn sha1(chunks: &[&[u8]]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let mut total_len: u64 = 0;
    for chunk in chunks {
        total_len += chunk.len() as u64;
    }

    let mut buf = [0u8; 64];
    let mut buf_len = 0;

    for chunk in chunks {
        let mut cur = *chunk;
        while !cur.is_empty() {
            let take = std::cmp::min(64 - buf_len, cur.len());
            buf[buf_len..buf_len + take].copy_from_slice(&cur[..take]);
            buf_len += take;
            cur = &cur[take..];
            if buf_len == 64 {
                process_sha1_block(&buf, &mut h0, &mut h1, &mut h2, &mut h3, &mut h4);
                buf_len = 0;
            }
        }
    }

    // Padding
    buf[buf_len] = 0x80;
    buf_len += 1;
    if buf_len > 56 {
        for b in &mut buf[buf_len..64] {
            *b = 0;
        }
        process_sha1_block(&buf, &mut h0, &mut h1, &mut h2, &mut h3, &mut h4);
        buf_len = 0;
    }
    for b in &mut buf[buf_len..56] {
        *b = 0;
    }
    let bit_len = total_len * 8;
    buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
    process_sha1_block(&buf, &mut h0, &mut h1, &mut h2, &mut h3, &mut h4);

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn process_sha1_block(
    block: &[u8; 64],
    h0: &mut u32,
    h1: &mut u32,
    h2: &mut u32,
    h3: &mut u32,
    h4: &mut u32,
) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let mut a = *h0;
    let mut b = *h1;
    let mut c = *h2;
    let mut d = *h3;
    let mut e = *h4;

    for (i, &word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }

    *h0 = h0.wrapping_add(a);
    *h1 = h1.wrapping_add(b);
    *h2 = h2.wrapping_add(c);
    *h3 = h3.wrapping_add(d);
    *h4 = h4.wrapping_add(e);
}

#[cfg(test)]
mod rollover_tests {
    use super::*;
    use crate::protocol::rtp::build_audio_rtp_header;

    #[test]
    fn hmac_sha1_matches_rfc_2202_test_case_1() {
        let key = [0x0b; 20];
        let digest = hmac_sha1(&key, &[b"Hi There"]);
        assert_eq!(
            digest,
            [
                0xb6, 0x17, 0x31, 0x86, 0x55, 0x05, 0x72, 0x64, 0xe2, 0x8b, 0xc0, 0xb6, 0xfb, 0x37,
                0x8c, 0x8e, 0xf1, 0x46, 0xbe, 0x00,
            ]
        );
    }

    #[test]
    fn hmac_sha1_matches_rfc_2202_multiblock_test_case_2() {
        let key = [0xaa; 20];
        let data = [0xdd; 50];
        let digest = hmac_sha1(&key, &[&data]);
        assert_eq!(
            digest,
            [
                0x12, 0x5d, 0x73, 0x42, 0xb9, 0xac, 0x11, 0xcd, 0x91, 0xa3, 0x9a, 0xf4, 0x8a, 0xa1,
                0x7b, 0x4f, 0x63, 0xf1, 0x75, 0xd3,
            ]
        );
    }

    #[test]
    fn derives_session_keys_with_rfc3711_lengths_and_rejects_short_master() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        assert_eq!(keys.enc.len(), 16);
        assert_eq!(keys.auth.len(), 20);
        assert_eq!(keys.salt.len(), 14);
        assert!(session_keys(&[0x42; 29]).is_err());
    }

    #[test]
    fn derives_master_from_the_first_thirty_session_id_bytes() {
        let session = b"012345678901234567890123456789extra";
        let master = master_from_sess_id(session).unwrap();
        assert_eq!(&master, b"012345678901234567890123456789");
        assert!(master_from_sess_id(b"short").is_err());
    }

    #[test]
    fn derives_a_deterministic_packet_iv_that_changes_with_sequence() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let first = packet_iv(&keys.salt, 0x1234_5678, 100, 0);
        let repeated = packet_iv(&keys.salt, 0x1234_5678, 100, 0);
        let next = packet_iv(&keys.salt, 0x1234_5678, 101, 0);
        assert_eq!(first.len(), 16);
        assert_eq!(first, repeated);
        assert_ne!(first, next);
    }

    #[test]
    fn protects_and_unprotects_an_rtp_payload() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let ssrc = 0xdead_beefu32;
        let seq = 1234u16;
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = 112;
        header[2..4].copy_from_slice(&seq.to_be_bytes());
        header[4..8].copy_from_slice(&960_000u32.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        let original = b"Testing Opus Audio Frame Payload 1234567890!@#$%^&*()";

        let protected = protect_rtp(&header, original, &keys, seq, ssrc, 0);
        assert_eq!(protected.len(), header.len() + original.len() + 10);
        let clear = unprotect_rtp(&protected, &keys, 0).unwrap();
        assert_eq!(clear.seq, seq);
        assert_eq!(clear.ssrc, ssrc);
        assert_eq!(clear.opus, original);
    }

    #[test]
    fn matches_legacy_node_srtp_wire_vector() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let header = hex_bytes("80620001000ea600deadbeef");
        let payload = b"legacy-rust-interoperability-vector-2026";
        let expected = hex_bytes(
            "80620001000ea600deadbeef5ca2531becdc8fa53bea26bab555b5ec67760afb4a1ed1e9dea5f60b8eb9ee00a93fc55ccfc99b72e3951717b28bee25c620",
        );
        let protected = protect_rtp(&header, payload, &keys, 1, 0xdead_beef, 0);
        assert_eq!(
            protected, expected,
            "Rust SRTP must match legacy Node byte-for-byte"
        );
        let clear = unprotect_rtp(&expected, &keys, 0).expect("legacy packet authenticates");
        assert_eq!(clear.seq, 1);
        assert_eq!(clear.ssrc, 0xdead_beef);
        assert_eq!(clear.opus, payload);
    }

    fn hex_bytes(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair.len(), 2);
                let digit = |b: u8| match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    _ => panic!("invalid hex digit"),
                };
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect()
    }

    #[test]
    fn matches_legacy_node_srtp_large_video_packet_tag() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let header = hex_bytes("80620001000ea600deadbeef");
        let payload = (0..1100)
            .map(|i| ((i * 37 + 11) & 0xff) as u8)
            .collect::<Vec<_>>();
        let protected = protect_rtp(&header, &payload, &keys, 1, 0xdead_beef, 0);
        assert_eq!(
            &protected[protected.len() - 10..],
            &hex_bytes("ee932972b32a73924f19")
        );
        let clear = unprotect_rtp(&protected, &keys, 0).expect("large legacy packet authenticates");
        assert_eq!(clear.opus, payload);
    }

    #[test]
    fn unprotects_rfc3711_mki_before_the_authentication_tag() {
        let keys = session_keys(&[0x6a; 30]).unwrap();
        let seq = 321u16;
        let ssrc = 0x1357_2468u32;
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = 98;
        header[2..4].copy_from_slice(&seq.to_be_bytes());
        header[4..8].copy_from_slice(&9000u32.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        let clear_payload = b"group-video-with-mki";
        let protected = protect_rtp(&header, clear_payload, &keys, seq, ssrc, 0);

        for (mki, tag_len) in [(&[0x01][..], 10usize), (&[0x01, 0x02, 0x03, 0x04][..], 10)] {
            let tag_start = protected.len() - 10;
            let mut packet = protected[..tag_start].to_vec();
            packet.extend_from_slice(mki);
            packet.extend_from_slice(&protected[tag_start..]);
            let result = unprotect_rtp_with_profile(&packet, &keys, 0, mki.len(), tag_len)
                .expect("MKI packet should authenticate");
            assert_eq!(result.seq, seq);
            assert_eq!(result.ssrc, ssrc);
            assert_eq!(result.opus, clear_payload);
            assert!(unprotect_rtp(&packet, &keys, 0).is_none());
        }
    }

    #[test]
    fn rejects_mki_packet_with_modified_ciphertext_or_tag() {
        let keys = session_keys(&[0x6b; 30]).unwrap();
        let seq = 322u16;
        let ssrc = 0x1357_2469u32;
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = 98;
        header[2..4].copy_from_slice(&seq.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        let protected = protect_rtp(&header, b"authenticated", &keys, seq, ssrc, 0);
        let tag_start = protected.len() - 10;
        let mut packet = protected[..tag_start].to_vec();
        packet.extend_from_slice(&[0xaa, 0xbb]);
        packet.extend_from_slice(&protected[tag_start..]);
        let mut damaged = packet.clone();
        damaged[12] ^= 0x80;
        assert!(unprotect_rtp_with_profile(&damaged, &keys, 0, 2, 10).is_none());
        let last = packet.len() - 1;
        packet[last] ^= 1;
        assert!(unprotect_rtp_with_profile(&packet, &keys, 0, 2, 10).is_none());
    }

    #[test]
    fn rejects_tampered_ciphertext_and_authentication_tag() {
        let keys = session_keys(&[0x42; 30]).unwrap();
        let ssrc = 0x1122_3344u32;
        let seq = 55u16;
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = 112;
        header[2..4].copy_from_slice(&seq.to_be_bytes());
        header[4..8].copy_from_slice(&1000u32.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        let protected = protect_rtp(&header, b"Authentic audio data", &keys, seq, ssrc, 0);

        let mut payload_tampered = protected.clone();
        payload_tampered[15] ^= 0xff;
        assert!(unprotect_rtp(&payload_tampered, &keys, 0).is_none());

        let mut tag_tampered = protected;
        let last = tag_tampered.len() - 1;
        tag_tampered[last] ^= 1;
        assert!(unprotect_rtp(&tag_tampered, &keys, 0).is_none());
    }

    #[test]
    fn authenticates_rtp_packets_after_sequence_rollover() {
        let keys = session_keys(&[0x51; 30]).unwrap();
        let seq = 3u16;
        let header = build_audio_rtp_header(seq, 960, 0x1122_3344, 77, false, 112);
        let protected = protect_rtp(
            &header,
            b"opus-frame-after-rollover",
            &keys,
            seq,
            0x1122_3344,
            1,
        );

        assert!(unprotect_rtp(&protected, &keys, 0).is_none());
        let clear = unprotect_rtp(&protected, &keys, 1).expect("correct rollover counter");
        assert_eq!(clear.seq, seq);
        assert_eq!(clear.ssrc, 0x1122_3344);
        assert_eq!(clear.opus, b"opus-frame-after-rollover");
    }
}
