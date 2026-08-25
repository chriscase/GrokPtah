fn main() {
    println!("cargo:rerun-if-changed=src/computer_use/macos_native_shim.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    cc::Build::new()
        .file("src/computer_use/macos_native_shim.m")
        .flag("-mmacosx-version-min=11.0")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .warnings(true)
        .compile("grokptah_macos_observation");

    for framework in [
        "AppKit",
        "ApplicationServices",
        "CoreGraphics",
        "Foundation",
        "Security",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
