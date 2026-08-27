//! Every target's `grokptah_agent_bridge::Name` import must resolve.
//!
//! Two macOS-gated targets — the desktop crate and
//! `examples/macos_computer_use_background_text.rs` — imported types from the
//! crate root that `lib.rs` never re-exported. Both are invisible to a
//! non-macOS `--all-targets` build, so each one cost a full macOS CI round to
//! find. This gate reads the sources instead of compiling them, so it holds on
//! every host.
//!
//! It checks reachability only. Which names *should* be public is a separate
//! decision that `lib.rs` still owns.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root_imports(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = source;
    while let Some(start) = rest.find("use grokptah_agent_bridge::") {
        let tail = &rest[start + "use grokptah_agent_bridge::".len()..];
        if let Some(open) = tail.strip_prefix('{') {
            if let Some(end) = open.find('}') {
                for entry in open[..end].split(',') {
                    let entry = entry.trim().split(" as ").next().unwrap_or("").trim();
                    if !entry.is_empty() && !entry.contains("::") && !entry.starts_with("//") {
                        names.insert(entry.to_string());
                    }
                }
            }
        } else if let Some(end) = tail.find(';') {
            let entry = tail[..end].trim();
            if !entry.contains("::") && entry.starts_with(char::is_uppercase) {
                names.insert(entry.to_string());
            }
        }
        rest = tail;
    }
    names
}

fn collect(directory: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

#[test]
fn every_crate_root_import_in_every_target_resolves() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(manifest.join("src/lib.rs")).expect("read lib.rs");

    let mut sources = Vec::new();
    for directory in ["examples", "tests", "benches"] {
        collect(&manifest.join(directory), &mut sources);
    }
    // The desktop crate is a separate nested workspace that depends on this
    // one by path; its root imports are the ones that broke the macOS job.
    collect(
        &manifest.join("../../../desktop/src-tauri/src"),
        &mut sources,
    );
    assert!(
        sources.len() > 10,
        "expected to find the bridge's targets, found {}",
        sources.len()
    );

    let mut unresolved = Vec::new();
    for source in &sources {
        let text = std::fs::read_to_string(source).expect("read source");
        for name in root_imports(&text) {
            if !lib.contains(&name) {
                unresolved.push(format!("{}: {name}", source.display()));
            }
        }
    }
    assert!(
        unresolved.is_empty(),
        "these targets import names the crate root does not re-export, which \
         only fails on the host that compiles them:\n  {}",
        unresolved.join("\n  ")
    );
}
