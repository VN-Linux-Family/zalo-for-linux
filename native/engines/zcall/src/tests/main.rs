use super::load_callv2_key;
use std::fs;

#[test]
fn loads_zalo_call_v2_key_from_bundle_source() {
    let path = std::env::temp_dir().join(format!("zcall-main-key-{}.js", std::process::id()));
    fs::write(
        &path,
        format!(
            "prefix callV2TransportAesKey:\"{}\" suffix",
            "AQEBAQEBAQEBAQEBAQEBAQ=="
        ),
    )
    .unwrap();
    assert_eq!(load_callv2_key(&path).unwrap(), [1u8; 16]);
    fs::remove_file(path).unwrap();
}
