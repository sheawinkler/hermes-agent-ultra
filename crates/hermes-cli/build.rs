fn main() {
    if !talk_feature_enabled() {
        return;
    }

    // speexdsp (aec-rs) and sherpa-onnx both embed kiss_fft; allow duplicate symbols at link time.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    match (os.as_str(), env.as_str()) {
        ("windows", _) => println!("cargo:rustc-link-arg=/FORCE:MULTIPLE"),
        ("macos", _) => println!("cargo:rustc-link-arg=-Wl,-multiply_defined,suppress"),
        (_, "gnu") => println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition"),
        _ => {}
    }
}

fn talk_feature_enabled() -> bool {
    std::env::var("CARGO_FEATURE_TALK").is_ok()
        || std::env::var("CARGO_FEATURE_TALK_ROCKCHIP").is_ok()
}
