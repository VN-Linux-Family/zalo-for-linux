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

    println!("cargo:rustc-link-lib=opus");
    println!("cargo:rustc-link-lib=pulse-simple");
    println!("cargo:rustc-link-lib=pulse");
    println!("cargo:rerun-if-changed=src/audio/c_pulse_vol.c");
    println!("cargo:rerun-if-changed=src/audio/c_apm.cpp");
    println!("cargo:rustc-check-cfg=cfg(has_webrtc_apm)");

    cc::Build::new()
        .file("src/audio/c_pulse_vol.c")
        .warnings(false)
        .compile("zcall_pulse_vol");
    println!("cargo:rustc-link-lib=static=zcall_pulse_vol");

    let has_webrtc = std::process::Command::new("pkg-config")
        .args(["--exists", "webrtc-audio-processing-1"])
        .status()
        .is_ok_and(|status| status.success());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .file("src/audio/c_apm.cpp");

    if has_webrtc {
        println!("cargo:rustc-cfg=has_webrtc_apm");
        println!("cargo:rustc-link-lib=webrtc-audio-processing-1");
        build
            .include("/usr/include/webrtc-audio-processing-1")
            .define("HAS_WEBRTC_APM", None)
            .define("WEBRTC_POSIX", None)
            .define("WEBRTC_LIBRARY_IMPL", None)
            .define("NOMINMAX", None);
    } else {
        println!(
            "cargo:warning=webrtc-audio-processing-1 not found via pkg-config; compiling APM stubs"
        );
    }
    build.compile("zcall_apm");
    println!("cargo:rustc-link-lib=static=zcall_apm");
}
