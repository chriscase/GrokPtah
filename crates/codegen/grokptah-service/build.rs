fn main() {
    // The shared black-box fixture reads this with `option_env!`, so the value
    // is sealed into the test binary when the workflow compiles it rather than
    // being settable by whoever starts the already-built test. Forwarding it
    // through the build script is what makes Cargo fingerprint the value, so a
    // changed declaration rebuilds instead of reusing a binary that names some
    // other revision.
    println!("cargo::rerun-if-env-changed=GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION");
    if let Ok(revision) = std::env::var("GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION") {
        println!("cargo::rustc-env=GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION={revision}");
    }
}
