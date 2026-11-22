fn main() {
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
