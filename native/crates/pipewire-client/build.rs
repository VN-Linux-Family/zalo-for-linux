use std::{env, fs, path::PathBuf};

const CORE_PROTOCOL_VERSION: u32 = 3;
const REGISTRY_PROTOCOL_VERSION: u32 = 3;
const CLIENT_NODE_PROTOCOL_VERSION: u32 = 6;

fn main() {
    println!("cargo:rerun-if-env-changed=PIPEWIRE_NATIVE_PROTOCOL_VERSION");
    println!("cargo:rerun-if-env-changed=PIPEWIRE_NATIVE_REGISTRY_VERSION");
    println!("cargo:rerun-if-env-changed=PIPEWIRE_NATIVE_CLIENT_NODE_VERSION");
    println!("cargo:rustc-check-cfg=cfg(pipewire_native_unix)");

    let target = env::var("TARGET").expect("Cargo must set TARGET");
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    assert!(
        target_family.split(',').any(|family| family == "unix"),
        "pipewire-client requires a Unix target"
    );
    println!("cargo:rustc-cfg=pipewire_native_unix");

    let core_version = env_u32("PIPEWIRE_NATIVE_PROTOCOL_VERSION", CORE_PROTOCOL_VERSION);
    let registry_version = env_u32(
        "PIPEWIRE_NATIVE_REGISTRY_VERSION",
        REGISTRY_PROTOCOL_VERSION,
    );
    let client_node_version = env_u32(
        "PIPEWIRE_NATIVE_CLIENT_NODE_VERSION",
        CLIENT_NODE_PROTOCOL_VERSION,
    );

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let generated = format!(
        "pub const CORE_PROTOCOL_VERSION: u32 = {core_version};\n\
         pub const REGISTRY_PROTOCOL_VERSION: u32 = {registry_version};\n\
         pub const CLIENT_NODE_PROTOCOL_VERSION: u32 = {client_node_version};\n\
         pub const MAX_MESSAGE_SIZE: usize = 16_777_215;\n\
         pub const BUILD_TARGET: &str = {target:?};\n"
    );
    fs::write(output.join("build_info.rs"), generated)
        .expect("write PipeWire protocol build metadata");
}

fn env_u32(name: &str, default: u32) -> u32 {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => default,
        Err(error) => panic!("read {name}: {error}"),
    }
}
