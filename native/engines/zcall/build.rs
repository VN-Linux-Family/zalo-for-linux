use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");

    let target_os = env::var("CARGO_CFG_TARGET_OS")
        .expect("Cargo must provide CARGO_CFG_TARGET_OS to the zcall build script");
    assert!(
        target_os == "linux",
        "zcall requires Linux (target OS: {target_os})"
    );
}
