fn main() {
    println!("cargo:rustc-link-lib=opus");
    println!("cargo:rustc-link-lib=pulse-simple");
    println!("cargo:rustc-link-lib=pulse");
    println!("cargo:rerun-if-changed=src/c_pulse_vol.c");
    println!("cargo:rerun-if-changed=src/c_apm.cpp");

    cc::Build::new()
        .file("src/c_pulse_vol.c")
        .warnings(false)
        .compile("zcall_pulse_vol");

    let has_webrtc = std::process::Command::new("pkg-config")
        .args(["--exists", "webrtc-audio-processing-1"])
        .status()
        .is_ok_and(|s| s.success());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .file("src/c_apm.cpp");

    if has_webrtc {
        println!("cargo:rustc-cfg=has_webrtc_apm");

        let cflags_out = std::process::Command::new("pkg-config")
            .args(["--cflags", "webrtc-audio-processing-1"])
            .output()
            .ok();
        let mut added_include = false;
        if let Some(out) = cflags_out.filter(|o| o.status.success()) {
            for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if let Some(inc) = flag.strip_prefix("-I") {
                    build.include(inc);
                    if let Some(parent) = std::path::Path::new(inc).parent() {
                        build.include(parent);
                    }
                    added_include = true;
                }
            }
        }
        if !added_include {
            build.include("/usr/include/webrtc-audio-processing-1");
        }

        build
            .define("HAS_WEBRTC_APM", None)
            .define("WEBRTC_POSIX", None)
            .define("WEBRTC_LIBRARY_IMPL", None)
            .define("NOMINMAX", None);
        // Compile zcall_apm before emitting webrtc/absl link flags so GNU ld
        // sees zcall_apm's undefined symbols prior to static archives.
        build.compile("zcall_apm");

        let libdir = std::process::Command::new("pkg-config")
            .args(["--variable=libdir", "webrtc-audio-processing-1"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(String::new, |o| {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            });
        let is_static = !libdir.is_empty()
            && std::path::Path::new(&libdir)
                .join("libwebrtc-audio-processing-1.a")
                .exists()
            && !std::path::Path::new(&libdir)
                .join("libwebrtc-audio-processing-1.so")
                .exists();

        let libs_args = if is_static {
            vec!["--libs", "--static", "webrtc-audio-processing-1"]
        } else {
            vec!["--libs", "webrtc-audio-processing-1"]
        };
        let libs_out = std::process::Command::new("pkg-config")
            .args(&libs_args)
            .output()
            .ok();
        let mut added_lib = false;
        if let Some(out) = libs_out.filter(|o| o.status.success()) {
            for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if let Some(dir) = flag.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={dir}");
                } else if let Some(lib) = flag.strip_prefix("-l") {
                    if is_static && lib == "webrtc-audio-processing-1" {
                        println!("cargo:rustc-link-lib=static={lib}");
                    } else {
                        println!("cargo:rustc-link-lib={lib}");
                    }
                    added_lib = true;
                }
            }
        }
        if !added_lib {
            println!("cargo:rustc-link-lib=webrtc-audio-processing-1");
        }
    } else {
        println!("cargo:warning=webrtc-audio-processing-1 not found via pkg-config; compiling stubs (fallback to software DSP)");
        build.compile("zcall_apm");
    }
}
