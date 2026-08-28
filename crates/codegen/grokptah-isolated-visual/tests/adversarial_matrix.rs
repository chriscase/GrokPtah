//! Adversarial matrix for the packaged Computer Use authority.
//!
//! Each test states an attack and asserts the system fails closed. None of
//! these launch a VM, request macOS permissions, sign anything, or dispatch OS
//! input; they exercise the admission, lease, ledger, and cleanup logic only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use chrono::{Duration, Utc};
use grokptah_isolated_visual::{
    admit_guest_image, admit_packaged_helper, hash_file, inspect_guest_image,
    inspect_helper_bundle, AppTrustAnchor, CleanupOutcome, CodeIdentityProbe,
    ComputerSurfaceLeaseState, GuestImageTrustAnchor, HelperTrustAnchor, IsolatedCleanupReason,
    IsolatedErrorCode, IsolatedPreflight, IsolatedVisualHost, IsolatedVisualStore,
    ObservedCodeIdentity, PackagedTrustRoot, SigningClass, HELPER_BUNDLE_ID, HELPER_EXECUTABLE,
    TRUST_ROOT_SCHEMA,
};
use tempfile::TempDir;

const ENTITLEMENTS: &[u8] = b"<?xml version=\"1.0\"?><plist><dict></dict></plist>";
const TEAM: &str = "TEAMID1234";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A probe that answers with whatever transcript the test supplies. It stands
/// in for `codesign`/`spctl`, which cannot run on a Linux CI host; the parsing
/// and comparison under test are identical either way.
#[derive(Debug)]
struct TranscriptProbe {
    identity: ObservedCodeIdentity,
    available: bool,
}

impl CodeIdentityProbe for TranscriptProbe {
    fn probe_id(&self) -> &'static str {
        "transcript_probe_v1"
    }
    fn available(&self) -> bool {
        self.available
    }
    fn inspect(
        &self,
        _bundle: &Path,
    ) -> grokptah_isolated_visual::IsolatedResult<ObservedCodeIdentity> {
        if !self.available {
            return Err(grokptah_isolated_visual::IsolatedError::unsupported(
                "probe unavailable",
            ));
        }
        Ok(self.identity.clone())
    }
}

fn identity_from(
    display: &str,
    requirement: &str,
    gatekeeper: &str,
    ok: bool,
) -> ObservedCodeIdentity {
    grokptah_isolated_visual::code_identity::parse_observed_identity(
        grokptah_isolated_visual::CapturedCodesignOutput {
            target: "/fixture/Helper.app".into(),
            display: display.into(),
            requirement: requirement.into(),
            gatekeeper: gatekeeper.into(),
            verify_ok: ok,
            gatekeeper_ok: ok,
        },
        ok,
        ok,
        ok,
    )
}

fn notarized(team: &str, bundle_id: &str) -> ObservedCodeIdentity {
    identity_from(
        &format!(
            "Identifier={bundle_id}\nTeamIdentifier={team}\n\
             Authority=Developer ID Application: Example Corp ({team})\n"
        ),
        &format!(
            "designated => identifier \"{bundle_id}\" and anchor apple generic and \
             certificate leaf[subject.OU] = {team}\n"
        ),
        "accepted\nsource=Notarized Developer ID\n",
        true,
    )
}

fn write_helper(root: &Path) -> PathBuf {
    let helper = root.join("GrokPtah Computer Use Helper.app");
    fs::create_dir_all(helper.join("Contents/MacOS")).unwrap();
    fs::write(
        helper.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleIdentifier</key><string>{HELPER_BUNDLE_ID}</string></dict></plist>"
        ),
    )
    .unwrap();
    fs::write(
        helper.join("Contents/MacOS").join(HELPER_EXECUTABLE),
        b"helper-executable-bytes",
    )
    .unwrap();
    fs::write(helper.join("Contents/entitlements.plist"), ENTITLEMENTS).unwrap();
    helper
}

fn artifacts(dir: &Path) -> (PathBuf, PackagedTrustRoot) {
    let root = dir.join("artifacts");
    fs::create_dir_all(&root).unwrap();
    let helper = write_helper(&root);
    fs::write(root.join("guest.img"), b"guest-bytes").unwrap();
    let trust = PackagedTrustRoot {
        schema: TRUST_ROOT_SCHEMA.into(),
        issuer: "adversarial-matrix".into(),
        app: AppTrustAnchor {
            bundle_id: grokptah_isolated_visual::APP_BUNDLE_ID.into(),
            team_id: TEAM.into(),
            designated_requirement: format!(
                "identifier \"{}\" and anchor apple generic",
                grokptah_isolated_visual::APP_BUNDLE_ID
            ),
        },
        helper: HelperTrustAnchor {
            bundle_id: HELPER_BUNDLE_ID.into(),
            team_id: TEAM.into(),
            designated_requirement: format!(
                "identifier \"{HELPER_BUNDLE_ID}\" and anchor apple generic and \
                 certificate leaf[subject.OU] = {TEAM}"
            ),
            entitlements_sha256: hash_file(&helper.join("Contents/entitlements.plist")).unwrap(),
        },
        guest_image: GuestImageTrustAnchor {
            digest_sha256: hash_file(&root.join("guest.img")).unwrap(),
            format: "raw".into(),
            provenance: "adversarial-matrix-image".into(),
            authorization_sha256: grokptah_isolated_visual::ids::sha256_hex(b"authorization"),
        },
    };
    (root, trust)
}

// ---------------------------------------------------------------------------
// Identity attacks
// ---------------------------------------------------------------------------

#[test]
fn forged_artifact_root_cannot_supply_its_own_expectations() {
    let dir = TempDir::new().unwrap();
    let (root, trust) = artifacts(dir.path());

    // The attacker swaps the image and ships a manifest that agrees with it.
    fs::write(root.join("guest.img"), b"attacker-image").unwrap();
    fs::write(
        root.join("guest.img.manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "manifestId": "forged",
            "digest": grokptah_isolated_visual::ids::sha256_hex(b"attacker-image"),
            "provenance": "forged",
            "format": "raw",
            "authorizationDigest": grokptah_isolated_visual::ids::sha256_hex(b"forged"),
        }))
        .unwrap(),
    )
    .unwrap();

    let observed = inspect_guest_image(&root.join("guest.img")).unwrap();
    let error = admit_guest_image(&observed, &trust).unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Unauthorized);
}

#[test]
fn synthesized_designated_requirement_and_team_identity_are_refused() {
    let dir = TempDir::new().unwrap();
    let (root, trust) = artifacts(dir.path());
    let helper = root.join("GrokPtah Computer Use Helper.app");

    // Signed by a team the operator never declared.
    let probe = TranscriptProbe {
        identity: notarized("ATTACKER99", HELPER_BUNDLE_ID),
        available: true,
    };
    let observed = inspect_helper_bundle(&helper, &probe).unwrap();
    assert!(admit_packaged_helper(&observed, &trust).is_err());

    // Correct team, but a requirement that merely name-drops the identifier.
    let mut loose = notarized(TEAM, HELPER_BUNDLE_ID);
    loose.designated_requirement = Some(format!("identifier \"{HELPER_BUNDLE_ID}\""));
    let probe = TranscriptProbe {
        identity: loose,
        available: true,
    };
    let observed = inspect_helper_bundle(&helper, &probe).unwrap();
    let error = admit_packaged_helper(&observed, &trust).unwrap_err();
    assert!(error.message.contains("designated requirement"), "{error}");

    // No requirement at all is never filled in from the observed Team ID.
    let mut missing = notarized(TEAM, HELPER_BUNDLE_ID);
    missing.designated_requirement = None;
    let probe = TranscriptProbe {
        identity: missing,
        available: true,
    };
    let observed = inspect_helper_bundle(&helper, &probe).unwrap();
    assert!(admit_packaged_helper(&observed, &trust).is_err());
}

#[test]
fn negated_signing_text_cannot_invert_into_admission() {
    let dir = TempDir::new().unwrap();
    let (root, trust) = artifacts(dir.path());
    let helper = root.join("GrokPtah Computer Use Helper.app");

    for hostile in [
        // Negation inside the Authority value.
        format!(
            "Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier={TEAM}\n\
             Authority=not Developer ID Application: Example Corp\n"
        ),
        // The right words, but never as an anchored key line.
        format!(
            "note: Authority=Developer ID Application appears only in prose\n\
             Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier={TEAM}\n"
        ),
        // An explicit "not signed" diagnostic.
        "/x: code object is not signed at all\n".to_string(),
    ] {
        let identity = identity_from(
            &hostile,
            &format!(
                "designated => identifier \"{HELPER_BUNDLE_ID}\" and anchor apple generic and \
                 certificate leaf[subject.OU] = {TEAM}\n"
            ),
            "accepted\nsource=Notarized Developer ID\n",
            true,
        );
        assert_ne!(
            identity.signing_class,
            SigningClass::NotarizedDeveloperId,
            "hostile transcript was promoted: {hostile}"
        );
        let probe = TranscriptProbe {
            identity,
            available: true,
        };
        let observed = inspect_helper_bundle(&helper, &probe).unwrap();
        assert!(admit_packaged_helper(&observed, &trust).is_err());
    }
}

#[test]
fn a_bundle_local_attestation_file_is_recorded_and_never_read() {
    let dir = TempDir::new().unwrap();
    let (root, trust) = artifacts(dir.path());
    let helper = root.join("GrokPtah Computer Use Helper.app");
    fs::write(
        helper.join("codesign-display.txt"),
        format!(
            "Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier={TEAM}\n\
             Authority=Developer ID Application: Example ({TEAM})\nsource=Notarized Developer ID\n"
        ),
    )
    .unwrap();

    // The OS says the bundle is unsigned; the planted file says otherwise.
    let probe = TranscriptProbe {
        identity: identity_from("/x: code object is not signed at all\n", "", "", false),
        available: true,
    };
    let observed = inspect_helper_bundle(&helper, &probe).unwrap();
    assert!(observed
        .ignored_self_attestations
        .contains(&"codesign-display.txt".to_string()));
    assert!(admit_packaged_helper(&observed, &trust).is_err());
}

#[test]
fn symlinked_entitlements_fail_closed() {
    let dir = TempDir::new().unwrap();
    let (root, _trust) = artifacts(dir.path());
    let helper = root.join("GrokPtah Computer Use Helper.app");
    let entitlements = helper.join("Contents/entitlements.plist");
    fs::remove_file(&entitlements).unwrap();
    #[cfg(unix)]
    {
        let elsewhere = dir.path().join("elsewhere.plist");
        fs::write(&elsewhere, ENTITLEMENTS).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &entitlements).unwrap();
        let probe = TranscriptProbe {
            identity: notarized(TEAM, HELPER_BUNDLE_ID),
            available: true,
        };
        let error = inspect_helper_bundle(&helper, &probe).unwrap_err();
        assert_eq!(error.code, IsolatedErrorCode::Unauthorized);
    }
}

#[test]
fn a_trust_root_inside_the_artifact_root_is_refused() {
    let dir = TempDir::new().unwrap();
    let (root, trust) = artifacts(dir.path());
    let inside = root.join("trust-root.json");
    fs::write(&inside, serde_json::to_vec(&trust).unwrap()).unwrap();
    assert!(PackagedTrustRoot::load(&inside, Some(&root)).is_err());

    let outside = dir.path().join("trust-root.json");
    fs::write(&outside, serde_json::to_vec(&trust).unwrap()).unwrap();
    PackagedTrustRoot::load(&outside, Some(&root)).unwrap();
}

// ---------------------------------------------------------------------------
// Lease, ledger, and cleanup attacks
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod support {
    include!("support/harness.rs");
}
use support::{granted_lease, harness, running_guest, Harness};

#[test]
fn a_second_process_cannot_open_the_same_store() {
    let harness = harness();
    let root = harness.host.store_root().to_path_buf();

    // Same process, second handle.
    let error = IsolatedVisualStore::open(&root, Utc::now())
        .err()
        .expect("a second handle must be refused");
    assert_eq!(error.code, IsolatedErrorCode::Conflict);

    // A genuinely separate OS process, so this proves a file lock rather than
    // an in-process mutex. `env!` resolves at compile time, so this branch
    // cannot silently skip.
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_grokptah-isolated-visual-qualify"));
    let output = Command::new(&exe)
        .arg("--try-open-store")
        .arg(&root)
        .output()
        .expect("second process runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a second process acquired the store lock: {stdout}"
    );
    assert!(
        stderr.contains("already open") || stdout.contains("already open"),
        "expected a lock conflict, got: {stdout}{stderr}"
    );

    // Once the holder is gone, another process may take it: the lock is a
    // lock, not a permanent poison.
    drop(harness);
    let output = Command::new(&exe)
        .arg("--try-open-store")
        .arg(&root)
        .output()
        .expect("second process runs");
    assert!(
        output.status.success(),
        "the store stayed locked after its holder exited: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_second_agent_cannot_take_a_leased_guest() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let _lease = granted_lease(&mut harness, &guest_id);
    // The guest is already leased; a second lease request is a conflict, so two
    // agents can never both hold the surface.
    let error = harness.host.enqueue_lease(&guest_id).unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Conflict);
}

#[test]
fn a_torn_lease_record_is_quarantined_not_trusted() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let root = dir.path().join("store");

    {
        let mut harness = Harness::at(&root, &clock);
        let guest_id = running_guest(&mut harness, "a");
        let _ = granted_lease(&mut harness, &guest_id);
    }

    let leases_dir = root.join("leases");
    let lease_path = fs::read_dir(&leases_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("a lease record exists");

    // Deserializable but semantically invalid: revision zero is not a lease.
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).unwrap()).unwrap();
    value["revision"] = serde_json::json!(0);
    fs::write(&lease_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let harness = Harness::at(&root, &clock);
    assert!(
        !harness.host.recovery().quarantined.is_empty(),
        "an invalid-but-deserializable record must be quarantined"
    );
    assert!(harness.host.leases().unwrap().is_empty());
    assert!(root.join("quarantine").read_dir().unwrap().next().is_some());
}

#[test]
fn a_truncated_lease_record_is_quarantined() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let root = dir.path().join("store");
    {
        let mut harness = Harness::at(&root, &clock);
        let guest_id = running_guest(&mut harness, "a");
        let _ = granted_lease(&mut harness, &guest_id);
    }
    let lease_path = fs::read_dir(root.join("leases"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    fs::write(&lease_path, b"{\"schemaVersion\": 1, \"lease").unwrap();

    let harness = Harness::at(&root, &clock);
    assert!(!harness.host.recovery().quarantined.is_empty());
    assert!(harness.host.leases().unwrap().is_empty());
}

#[test]
fn a_stale_lease_is_reaped_on_open() {
    // Two distinct paths must both retire a lease, and this test separates
    // them so neither can silently stand in for the other.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let root = dir.path().join("store");

    // Path A: expiry on a *live* host. The grant window lapses while the host
    // is still running, so this is expiry alone, not restart recovery.
    let mut harness = Harness::at(&root, &clock);
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    let conflict_domain = harness.host.guest(&guest_id).unwrap().conflict_domain_id;
    clock.jump(Duration::minutes(30));
    // grant_next reaps expired grants before judging capacity.
    let _ = harness.host.grant_next(&conflict_domain);
    let lease = harness
        .host
        .leases()
        .unwrap()
        .into_iter()
        .find(|lease| lease.lease_id == lease_id)
        .expect("lease still recorded");
    assert!(
        lease.state.is_terminal(),
        "an expired grant must be reaped on a live host, got {:?}",
        lease.state
    );
    drop(harness);

    // Path B: restart. A lease that was live when the process died is not
    // resumable, whether or not it had expired.
    let dir2 = TempDir::new().unwrap();
    let clock2 = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let root2 = dir2.path().join("store");
    {
        let mut harness = Harness::at(&root2, &clock2);
        let guest_id = running_guest(&mut harness, "b");
        let _ = granted_lease(&mut harness, &guest_id);
    }
    clock2.jump(Duration::seconds(1));
    let harness = Harness::at(&root2, &clock2);
    for lease in harness.host.leases().unwrap() {
        assert!(
            lease.state.is_terminal(),
            "lease {} survived a restart as {:?}",
            lease.lease_id,
            lease.state
        );
    }
}

#[test]
fn cleanup_that_leaves_a_resource_behind_is_uncertain() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    harness.clock.jump(Duration::seconds(1));
    harness
        .host
        .terminate(&guest_id, IsolatedCleanupReason::Success)
        .unwrap();

    // Replace the overlay file with a non-empty directory: `remove_file` then
    // genuinely fails, which is the case a discarded error used to hide.
    let overlay = harness
        .host
        .store_root()
        .join("overlays")
        .join(format!("{guest_id}.overlay"));
    fs::remove_file(&overlay).unwrap();
    fs::create_dir(&overlay).unwrap();
    fs::write(overlay.join("occupant"), b"x").unwrap();

    let (guest, receipt) = harness.host.cleanup(&guest_id).unwrap();
    assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
    assert!(receipt.unresolved.iter().any(|r| r.contains("overlay")));
    assert!(!guest.cleaned);
    assert_eq!(
        receipt.require_exact().unwrap_err().code,
        IsolatedErrorCode::UncertainOutcome
    );
}

#[test]
fn a_fabricated_cleanup_receipt_does_not_validate() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    harness.clock.jump(Duration::seconds(1));
    harness
        .host
        .terminate(&guest_id, IsolatedCleanupReason::Success)
        .unwrap();
    let (_, receipt) = harness.host.cleanup(&guest_id).unwrap();
    receipt.validate().unwrap();

    // Flip the observed states while keeping the Exact claim and its digests.
    let mut forged = receipt.clone();
    for probe in &mut forged.probes {
        probe.state = grokptah_isolated_visual::ResourceState::Present;
    }
    assert!(forged.validate().is_err());

    // Claim Exact with no evidence at all.
    let mut empty = receipt;
    empty.probes.clear();
    empty.probe_digests.clear();
    assert!(empty.validate().is_err());
}

#[test]
fn a_duplicate_dispatch_id_with_a_changed_payload_is_refused() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    harness
        .host
        .ingest_frame(&guest_id, &lease_id, 8, 8, b"frame")
        .unwrap();
    let first = support::dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");
    harness
        .host
        .inject_dispatch(&guest_id, &lease_id, first.clone(), false)
        .unwrap();

    let mut changed = first;
    changed.kind = grokptah_isolated_visual::protocol::IsolatedInputKind::Key {
        code: "z".into(),
        pressed: true,
    };
    let error = harness
        .host
        .inject_dispatch(&guest_id, &lease_id, changed, false)
        .unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Conflict);
    assert_eq!(harness.host.simulator().input_len(&guest_id), 1);
}

#[test]
fn two_restarts_after_injection_never_replay() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let root = dir.path().join("store");
    let (guest_id, lease_id) = {
        let mut harness = Harness::at(&root, &clock);
        let guest_id = running_guest(&mut harness, "a");
        let lease_id = granted_lease(&mut harness, &guest_id);
        harness
            .host
            .ingest_frame(&guest_id, &lease_id, 8, 8, b"frame")
            .unwrap();
        let event = support::dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");
        // Injected is durable; the acknowledgement never lands.
        harness
            .host
            .inject_dispatch(&guest_id, &lease_id, event, true)
            .unwrap();
        (guest_id, lease_id)
    };

    for restart in 1..=2 {
        clock.jump(Duration::seconds(1));
        let harness = Harness::at(&root, &clock);
        let lease = harness
            .host
            .leases()
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == lease_id)
            .expect("lease survives restart");
        assert_eq!(
            lease.state,
            ComputerSurfaceLeaseState::Uncertain,
            "restart {restart}"
        );
        assert!(!harness.host.guest(&guest_id).unwrap().is_live());
    }
}

// ---------------------------------------------------------------------------
// Cross-implementation digest agreement
// ---------------------------------------------------------------------------

/// The Rust and Node inspectors must compute the *same* digests, or a packaged
/// artifact could be admitted by one and rejected by the other. This pins both
/// the single-file digest and the sorted bundle-manifest digest against Node's
/// `crypto.createHash("sha256")`.
#[test]
fn rust_and_js_digests_agree() {
    let Some(node) = which_node() else {
        eprintln!("skipping: node is not on PATH");
        return;
    };
    let dir = TempDir::new().unwrap();
    let helper = write_helper(dir.path());
    let file = helper.join("Contents/entitlements.plist");

    let rust_file = hash_file(&file).unwrap();
    let rust_bundle = grokptah_isolated_visual::hash_bundle_manifest(&helper).unwrap();

    let script = r#"
const { createHash } = require("node:crypto");
const { lstatSync, readdirSync, readFileSync } = require("node:fs");
const { join } = require("node:path");
const [, , filePath, bundleRoot] = process.argv;
function sha256File(p) {
  const st = lstatSync(p);
  if (st.isSymbolicLink() || !st.isFile()) throw new Error("refusing " + p);
  return createHash("sha256").update(readFileSync(p)).digest("hex");
}
function hashBundleManifest(root) {
  const files = [];
  (function walk(cur, rel) {
    const st = lstatSync(cur);
    if (st.isSymbolicLink()) throw new Error("symlink " + rel);
    if (st.isDirectory()) {
      for (const name of readdirSync(cur).sort()) walk(join(cur, name), rel ? rel + "/" + name : name);
      return;
    }
    files.push([rel, sha256File(cur)]);
  })(root, "");
  files.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  const h = createHash("sha256");
  for (const [rel, digest] of files) {
    h.update(rel); h.update("\0"); h.update(digest); h.update("\0");
  }
  return h.digest("hex");
}
process.stdout.write(JSON.stringify({
  file: sha256File(filePath),
  bundle: hashBundleManifest(bundleRoot),
}));
"#;
    let script_path = dir.path().join("digest.cjs");
    fs::write(&script_path, script).unwrap();
    let output = Command::new(node)
        .arg(&script_path)
        .arg(&file)
        .arg(&helper)
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let js: serde_json::Value = serde_json::from_slice(&output.stdout).expect("node json");

    assert_eq!(
        js["file"].as_str().unwrap(),
        rust_file,
        "single-file SHA-256 disagrees between Rust and JS"
    );
    assert_eq!(
        js["bundle"].as_str().unwrap(),
        rust_bundle,
        "bundle-manifest digest disagrees between Rust and JS"
    );
    // And the Rust digest really is a plain SHA-256 of the bytes, not a
    // double hash: the two must be the same value.
    assert_eq!(
        rust_file,
        grokptah_isolated_visual::ids::sha256_hex(&fs::read(&file).unwrap())
    );
}

fn which_node() -> Option<PathBuf> {
    for dir in std::env::var("PATH").ok()?.split(':') {
        let candidate = Path::new(dir).join("node");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Reachability of production admission
// ---------------------------------------------------------------------------

#[test]
fn production_preflight_is_reachable_and_denies_on_this_host() {
    // The production path must be callable, and on any host without signed
    // artifacts, a trust root, and Apple silicon it must deny — not panic and
    // not silently read as eligible.
    let preflight = IsolatedPreflight::inspect_production();
    assert!(!preflight.allowed_to_launch);
    assert!(!preflight.virtualization_framework_launched_claim());
    assert!(!preflight.deny_reasons.is_empty());
    assert!(preflight.fail_closed_launch().is_err());
    assert!(preflight.with_observed_launch(true).is_err());
}

#[test]
fn a_host_opened_without_artifacts_never_claims_virtualization_framework() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(grokptah_isolated_visual::TestClock::new(Utc::now()));
    let host = IsolatedVisualHost::open_with_preflight(
        dir.path().join("store"),
        clock,
        grokptah_isolated_visual::HermeticResolver::new(
            grokptah_isolated_visual::ContentAddressedStore::new(),
        ),
        IsolatedPreflight::denied("no artifacts in this environment"),
    )
    .unwrap();
    assert!(!host.preflight().allowed_to_launch);
    assert!(!host.preflight().virtualization_framework_launched_claim());
}
