//! Release gate: qualify a candidate from its post-soak evidence directory.
//!
//! Usage:
//!
//! ```text
//! grokptah-qualify-release <evidence-dir> <policy.json> [artifacts.json]
//! ```
//!
//! Exits `0` and prints the qualification (and, when artifacts are supplied,
//! the bound release record) as JSON on stdout. Exits `1` and prints every
//! finding on stderr when the evidence does not qualify the candidate.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use grokptah_release_evidence::{
    QualificationPolicy, Rejection, ReleaseArtifact, qualify_from_directory,
};

fn main() -> ExitCode {
    match run() {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (evidence_dir, policy_path, artifacts_path) = match arguments.as_slice() {
        [evidence_dir, policy_path] => (evidence_dir, policy_path, None),
        [evidence_dir, policy_path, artifacts_path] => {
            (evidence_dir, policy_path, Some(artifacts_path))
        }
        _ => {
            return Err(
                "usage: grokptah-qualify-release <evidence-dir> <policy.json> [artifacts.json]"
                    .into(),
            );
        }
    };

    let policy: QualificationPolicy = read_json(policy_path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the unix epoch: {error}"))?
        .as_secs();

    let candidate =
        qualify_from_directory(Path::new(evidence_dir), &policy, now).map_err(describe)?;

    let rendered = match artifacts_path {
        None => serde_json::json!({ "qualification": candidate }),
        Some(path) => {
            let artifacts: Vec<ReleaseArtifact> = read_json(path)?;
            let release = candidate.bind_release(artifacts).map_err(describe)?;
            serde_json::json!({ "qualification": candidate, "release": release })
        }
    };
    serde_json::to_string_pretty(&rendered)
        .map_err(|error| format!("could not render output: {error}"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("cannot parse {path}: {error}"))
}

fn describe(rejection: Rejection) -> String {
    let mut message = String::from("NOT QUALIFIED\n");
    message.push_str(&rejection.to_string());
    message
}
