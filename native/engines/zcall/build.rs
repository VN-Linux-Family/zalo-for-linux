fn main() {
    println!("cargo:rerun-if-changed=src/screen/pipewire.c");

    let pipewire_cflags = std::process::Command::new("pkg-config")
        .args(["--cflags", "libpipewire-0.3"])
        .output()
        .expect("pkg-config is required to locate PipeWire headers");
    assert!(
        pipewire_cflags.status.success(),
        "libpipewire-0.3 development files are required (pkg-config libpipewire-0.3)"
    );
    let mut pipewire_build = cc::Build::new();
    pipewire_build.file("src/screen/pipewire.c").warnings(false);
    for flag in String::from_utf8_lossy(&pipewire_cflags.stdout).split_whitespace() {
        if let Some(include) = flag.strip_prefix("-I") {
            pipewire_build.include(include);
        } else {
            pipewire_build.flag(flag);
        }
    }
    pipewire_build.compile("zcall_pipewire");
    println!("cargo:rustc-link-lib=dl");
}
