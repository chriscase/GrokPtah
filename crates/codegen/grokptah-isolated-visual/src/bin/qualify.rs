//! Emit a packaged-authority qualification record for the current host.
//!
//! This binary inspects and reports. It never signs, never opens the Keychain,
//! never prompts for TCC, and never boots a guest. The verdict it prints is
//! deliberately capped: without an OS-verified signed helper, an operator trust
//! root, and observed hardware, the honest answer is `unavailable` or
//! `fail_closed`, never `pass`.

use std::env;
use std::fs;
use std::path::PathBuf;

use grokptah_isolated_visual::{IsolatedPreflight, IsolatedVisualStore, PackagedTrustRoot};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    // Cross-process store-lock probe, used by the adversarial suite to prove
    // the exclusive lock is not merely an in-process mutex.
    if let Some(index) = args.iter().position(|arg| arg == "--try-open-store") {
        let Some(root) = args.get(index + 1) else {
            eprintln!("--try-open-store requires a path");
            std::process::exit(2);
        };
        match IsolatedVisualStore::open(root, chrono::Utc::now()) {
            Ok(_) => {
                println!("acquired store lock at {root}");
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("{}", error.message);
                std::process::exit(1);
            }
        }
    }

    let artifact_root =
        std::env::var_os(grokptah_isolated_visual::ARTIFACT_ROOT_ENV).map(PathBuf::from);
    let trust_root = PackagedTrustRoot::from_env(artifact_root.as_deref());
    let preflight = IsolatedPreflight::inspect_production();

    // Verdict vocabulary, in increasing strength:
    //   unavailable  - the inputs to decide were not present on this host
    //   fail_closed  - inputs were present and admission denied
    //   partial      - artifacts admitted, but no hardware launch was observed
    //   pass         - reserved; requires observed hardware evidence
    let verdict = if !preflight.code_identity_probe_available || !preflight.trust_root_present {
        "unavailable"
    } else if !preflight.launch_intent_admitted {
        "fail_closed"
    } else if preflight.virtualization_framework_launched_claim() {
        // Unreachable from this binary: it does not launch a guest.
        "pass"
    } else {
        "partial"
    };

    let evidence = serde_json::json!({
        "schemaVersion": 1,
        "kind": "isolatedVisualQualification",
        "baseSha": "67e29bd34dc64049432c715c93c2cef2185c63ea",
        "verdict": verdict,
        "trustRootPresent": preflight.trust_root_present,
        "trustRootIssuer": trust_root.as_ref().ok().map(|root| root.issuer.clone()),
        "trustRootError": trust_root.as_ref().err().map(|error| error.message.clone()),
        "artifactRoot": artifact_root.as_ref().map(|path| path.display().to_string()),
        "codeIdentityProbeAvailable": preflight.code_identity_probe_available,
        "evidenceClass": preflight.evidence_class,
        "virtualizationFrameworkLaunched": preflight.virtualization_framework_launched_claim(),
        "simulatorEvidenceIneligibleForVmQualification": true,
        "sourceCompilationIneligibleForVmQualification": true,
        "tccGrantsObserved": false,
        "notarizationObserved": preflight
            .helper_identity
            .as_ref()
            .map(|identity| identity.signing_class),
        "hardwareActionObserved": false,
        "ignoredSelfAttestations": preflight.ignored_self_attestations,
        "denyReasons": preflight.deny_reasons,
        "preflight": preflight,
    });

    let encoded = serde_json::to_string_pretty(&evidence).expect("encode");
    println!("{encoded}");
    if let Some(out) = args
        .iter()
        .position(|arg| arg == "--out")
        .and_then(|index| args.get(index + 1))
    {
        let path = PathBuf::from(out);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, encoded).expect("write evidence");
    }
}
