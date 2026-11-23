use super::*;

#[test]
fn test_callv2_encrypt_decrypt_roundtrip() {
    let key = [0x42u8; 16];
    let original = b"{\"type\":\"ping\",\"seq\":12345}";
    let enc = encrypt(original, &key);
    let dec = decrypt(&enc, &key).expect("decrypt should succeed");
    assert_eq!(dec, original);
}
