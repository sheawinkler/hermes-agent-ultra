fn main() {
    let feature_enabled = std::env::var("CARGO_FEATURE_ROCKCHIP").is_ok();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if feature_enabled && target_arch == "aarch64" {
        link_tts_sdk();
        link_asr_sdk();
    }
}

fn link_tts_sdk() {
    let sdk = std::env::var("RK_TTS_SDK_DIR").ok();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let project_dir = std::path::Path::new(&manifest);

    let c_api_lib = project_dir.join("rkaudio/librktts_c_api.a");

    let mut lib_dirs: Vec<std::path::PathBuf> = Vec::new();
    lib_dirs.push(project_dir.join("rkaudio/lib"));
    if let Some(ref s) = sdk {
        lib_dirs.push(std::path::PathBuf::from(s).join("lib/Linux/aarch64"));
    }

    let mut found_so = false;
    for dir in &lib_dirs {
        if dir.join("librktts.so").exists() {
            println!("cargo:rustc-link-search={}", dir.display());
            found_so = true;
            break;
        }
    }
    if !found_so {
        panic!("librktts.so not found. Set RK_TTS_SDK_DIR or run: make rkaudio/lib");
    }

    println!("cargo:rustc-link-lib=rktts");
    println!("cargo:rustc-link-lib=rknnrt");

    if c_api_lib.exists() {
        println!(
            "cargo:rustc-link-search={}",
            project_dir.join("rkaudio").display()
        );
        println!("cargo:rustc-link-lib=static=rktts_c_api");
    } else if let Some(ref sdk) = sdk {
        cc::Build::new()
            .cpp(true)
            .std("c++11")
            .include(format!("{sdk}/include"))
            .file(project_dir.join("rkaudio/rk_tts_c_api.cpp"))
            .compile("rktts_c_api");
    }
}

fn link_asr_sdk() {
    let asr_sdk = std::env::var("RK_ASR_SDK_DIR").ok();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let project_dir = std::path::Path::new(&manifest);

    let mut lib_dirs: Vec<std::path::PathBuf> = Vec::new();
    lib_dirs.push(project_dir.join("rkaudio/lib"));
    if let Some(ref s) = asr_sdk {
        lib_dirs.push(std::path::PathBuf::from(s).join("lib/Linux/aarch64"));
        lib_dirs.push(
            std::path::PathBuf::from(s).join("RK3588_ROCKX2_SDK_V1.1.2_20260311/lib/Linux/aarch64"),
        );
    }

    let asr_libs = &[
        "rockasr",
        "rockx2",
        "rockx_modules",
        "rknnrt",
        "rknn3_api",
        "rkllmrt",
        "onnxruntime",
        "gomp",
    ];

    let mut found = false;
    for dir in &lib_dirs {
        let p = dir.join("librockasr.so");
        if p.exists() {
            println!("cargo:rustc-link-search={}", dir.display());
            found = true;
            break;
        } else {
            println!("cargo:warning=ASR lib not found: {}", p.display());
        }
    }
    if !found {
        println!(
            "cargo:warning=ASR SDK not found — ASR disabled. Set RK_ASR_SDK_DIR or copy libs to rkaudio/lib/"
        );
        return;
    }

    for lib in asr_libs {
        println!("cargo:rustc-link-lib={}", lib);
    }

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let project_dir = std::path::Path::new(&manifest);
    cc::Build::new()
        .file(project_dir.join("rkaudio/force_asr_libs.c"))
        .compile("force_asr_libs");
}
