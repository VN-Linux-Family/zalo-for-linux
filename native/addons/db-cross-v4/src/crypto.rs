use aes::Aes256;
use cbc::cipher::generic_array::GenericArray;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;

type Aes256CbcDec = Decryptor<Aes256>;

pub fn derive_aes256_key_zalo_v2(private_key_str: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (i, &b) in private_key_str.as_bytes().iter().take(32).enumerate() {
        key[i] = b.to_ascii_uppercase();
    }
    key
}

pub fn aes256cbc_decrypt_nopad(ct: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Option<Vec<u8>> {
    if ct.is_empty() || ct.len() % 16 != 0 {
        return None;
    }
    let mut pt = ct.to_vec();
    let mut decryptor = Aes256CbcDec::new(GenericArray::from_slice(key), GenericArray::from_slice(iv));
    for block in pt.chunks_exact_mut(16) {
        decryptor.decrypt_block_mut(GenericArray::from_mut_slice(block));
    }
    Some(pt)
}

pub fn aes256cbc_decrypt_nopad_chunked_reset_iv(ct: &[u8], key: &[u8; 32]) -> Option<Vec<u8>> {
    const CHUNK_SIZE: usize = 0x10000; // 65536
    if ct.is_empty() || ct.len() % 16 != 0 {
        return None;
    }
    let mut pt_out = Vec::with_capacity(ct.len());
    let iv0 = [0u8; 16];
    for chunk in ct.chunks(CHUNK_SIZE) {
        let part_pt = aes256cbc_decrypt_nopad(chunk, key, &iv0)?;
        pt_out.extend_from_slice(&part_pt);
    }
    Some(pt_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_key() {
        let key = derive_aes256_key_zalo_v2("abcdef123456");
        assert_eq!(&key[0..12], b"ABCDEF123456");
        assert_eq!(&key[12..32], &[0u8; 20]);
    }
}
