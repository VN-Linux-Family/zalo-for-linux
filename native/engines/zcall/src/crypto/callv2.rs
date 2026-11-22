//! Call-v2 transport crypto (AES-128-CBC zero-IV, PKCS#7 padding).

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};

type Aes128CbcEnc = cbc::Encryptor<Aes128>;
type Aes128CbcDec = cbc::Decryptor<Aes128>;

const ZERO_IV: [u8; 16] = [0u8; 16];

/// Decode the 128-bit call-v2 transport key loaded by the Zalo helper.
pub fn decode_transport_key(encoded: &str) -> Result<[u8; 16], String> {
    let key_bytes = decode_base64(encoded)?;
    if key_bytes.len() != 16 {
        return Err(format!("Unexpected key length: {}", key_bytes.len()));
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(&key_bytes);
    Ok(key)
}

pub fn encrypt(data: &[u8], key: &[u8; 16]) -> String {
    let cipher = Aes128CbcEnc::new(key.into(), &ZERO_IV.into());
    let mut buf = vec![0u8; data.len() + 16];
    buf[..data.len()].copy_from_slice(data);
    let ct = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
        .expect("PKCS7 encrypt failed");
    hex::encode(ct)
}

pub fn decrypt(hex_str: &str, key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let mut ciphertext = hex::decode(hex_str).map_err(|e| format!("Invalid hex: {}", e))?;
    let cipher = Aes128CbcDec::new(key.into(), &ZERO_IV.into());
    let pt = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext)
        .map_err(|e| format!("PKCS7 decrypt failed: {:?}", e))?;
    Ok(pt.to_vec())
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let mut table = [255u8; 256];
    for (i, &b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[b as usize] = i as u8;
    }

    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);

    let mut buf = 0u32;
    let mut bits = 0;
    for &b in &clean {
        if b == b'=' {
            break;
        }
        let val = table[b as usize];
        if val == 255 {
            return Err(format!("Invalid base64 character: {}", b as char));
        }
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callv2_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 16];
        let original = b"{\"type\":\"ping\",\"seq\":12345}";
        let enc = encrypt(original, &key);
        let dec = decrypt(&enc, &key).expect("decrypt should succeed");
        assert_eq!(dec, original);
    }
}
