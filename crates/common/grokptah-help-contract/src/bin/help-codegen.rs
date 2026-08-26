//! Emit or verify the generated Semantic Help artifacts.
//!
//! ```text
//! cargo run -p grokptah-help-contract --bin help-codegen -- --write
//! cargo run -p grokptah-help-contract --bin help-codegen -- --verify
//! ```
//!
//! `--verify` is the gate: it re-emits every artifact into memory and compares
//! byte for byte with what is committed. Regeneration is required to be
//! byte-identical, so a drifted artifact — whether hand-edited or produced by a
//! different model — fails loudly instead of being silently overwritten by the
//! next person who runs `--write`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use grokptah_help_contract::corpus::Visibility;
use grokptah_help_contract::{
    CORPUS_ARTIFACT_PATH, PARITY_ARTIFACT_PATH, PUBLIC_CORPUS_ARTIFACT_PATH, SCHEMA_ARTIFACT_PATH,
    TYPESCRIPT_ARTIFACT_PATH, build_corpus, codegen, render_corpus_artifact,
};

/// Walk up from the crate directory to the repository root.
fn repo_root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .ancestors()
        .nth(3)
        .expect("crate sits three levels below the repository root")
        .to_path_buf()
}

struct Artifact {
    path: &'static str,
    contents: String,
}

fn artifacts() -> Vec<Artifact> {
    let model = codegen::model();
    let corpus = build_corpus();
    vec![
        Artifact {
            path: CORPUS_ARTIFACT_PATH,
            contents: render_corpus_artifact(&corpus),
        },
        // The published bundle is filtered here, by the same code that built
        // the corpus, rather than by a packaging script that could be run with
        // the wrong argument or skipped.
        Artifact {
            path: PUBLIC_CORPUS_ARTIFACT_PATH,
            contents: render_corpus_artifact(&corpus.bundle_at(Visibility::Public)),
        },
        Artifact {
            path: TYPESCRIPT_ARTIFACT_PATH,
            contents: codegen::emit_typescript(&model),
        },
        Artifact {
            path: SCHEMA_ARTIFACT_PATH,
            contents: codegen::render_json_schema(&codegen::emit_json_schema(&model)),
        },
        Artifact {
            path: PARITY_ARTIFACT_PATH,
            contents: codegen::render_json_schema(&codegen::digest_parity_cases()),
        },
    ]
}

fn main() -> ExitCode {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "--verify".to_string());
    let root = repo_root();
    let artifacts = artifacts();

    match mode.as_str() {
        "--write" => {
            for artifact in &artifacts {
                let target = root.join(artifact.path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).expect("artifact directory is creatable");
                }
                std::fs::write(&target, &artifact.contents).expect("artifact is writable");
                println!("wrote {}", artifact.path);
            }
            ExitCode::SUCCESS
        }
        "--verify" => {
            let mut drifted = false;
            for artifact in &artifacts {
                let target = root.join(artifact.path);
                match std::fs::read_to_string(&target) {
                    Ok(found) if found == artifact.contents => {
                        println!("ok    {}", artifact.path);
                    }
                    Ok(found) => {
                        drifted = true;
                        println!(
                            "DRIFT {} (committed {} bytes, regenerated {} bytes)",
                            artifact.path,
                            found.len(),
                            artifact.contents.len()
                        );
                    }
                    Err(error) => {
                        drifted = true;
                        println!("MISSING {} ({error})", artifact.path);
                    }
                }
            }
            if drifted {
                eprintln!(
                    "\ngenerated artifacts are not byte-identical to a fresh emission.\n\
                     run: cargo run -p grokptah-help-contract --bin help-codegen -- --write"
                );
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown mode `{other}`; expected --write or --verify");
            ExitCode::FAILURE
        }
    }
}
