//! Adversarial captured-frame evidence for Contained Browser (Phase-1 packet 9).
//!
//! Proves GuestFrame digests are recomputed from bounded synthetic payload bytes,
//! that tamper/oversize/caller-digest/source-upgrade fail closed, that raw bytes
//! never serialize, and that Stop/fence/restart plus browser-engine fail-closed
//! invariants are unchanged. Does not claim isolation PASS or enable admission.

use grokptah_isolated_surface::{
    admit_browser_engine_capture, HarnessErrorCode, SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
};
#[cfg(not(feature = "browser-engine"))]
use grokptah_isolated_surface::{
    admit_captured_frame_with_claimed_digest, assert_postcondition_change, canonical_sha256_digest,
    is_canonical_sha256_digest, require_frame_for_postcondition,
    run_contained_browser_stop_fence_regression, seal_contained_browser_dry_run_pack,
    serialize_evidence_pack, simulator_synthetic_frame_bytes, verify_evidence_pack,
    BoundedCapturedFrame, CapturedFrameMediaKind, CapturedFrameSource, ContainedBrowserBackend,
    EvidenceVerifierCode, GuestLifecycleDisposition, GuestLifecyclePhase, GuestLocalAction,
    HarnessSnapshot, HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness,
    ProofEvidenceClass, Sep18NoModelProofSequencer, CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
    MAX_CAPTURED_FRAME_BYTES, SNAPSHOT_FILE, SYNTHETIC_FRAME_HEIGHT, SYNTHETIC_FRAME_WIDTH,
};
#[cfg(not(feature = "browser-engine"))]
use tempfile::TempDir;

fn needle() -> &'static str {
    std::str::from_utf8(SYNTHETIC_FRAME_PAYLOAD_NEEDLE).expect("needle utf8")
}

#[cfg(not(feature = "browser-engine"))]
fn assert_no_raw_leak(serialized: &str) {
    let marker = needle();
    assert!(
        !serialized.contains(marker),
        "raw synthetic payload leaked into serialized form"
    );
    assert!(!serialized.contains("GROKPTAH-SYNTHETIC-BROWSER-FRAME-v1"));
    assert!(!serialized.contains("/tmp/"));
    assert!(!serialized.contains("capturedBytes"));
    assert!(!serialized.contains("raw-synthetic-frame-pixels"));
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn digest_recomputed_from_simulator_bytes_not_labels() {
    let capture = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("admit");
    let expected = canonical_sha256_digest(capture.captured_bytes());
    assert_eq!(capture.digest(), expected);
    assert!(is_canonical_sha256_digest(&capture.digest()));
    assert!(!capture.digest().contains("contained-browser-frame"));
    assert!(!capture.digest().contains("link="));
    capture
        .public_evidence()
        .verify_against_bytes(&simulator_synthetic_frame_bytes(false))
        .expect("bytes match");
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_state_change_changes_content_digest() {
    let mut backend = ContainedBrowserBackend::new();
    let before = backend.boot().expect("boot");
    let before_expected = canonical_sha256_digest(&simulator_synthetic_frame_bytes(false));
    assert_eq!(before.digest, before_expected);
    require_frame_for_postcondition(&before).expect("before honest");

    let delta = match backend
        .inject_guest_local(GuestLocalAction::ClickGuestButton)
        .expect("click")
    {
        grokptah_isolated_surface::InjectOutcome::Changed(delta) => delta,
        other => panic!("expected Changed, got {other:?}"),
    };
    let after = backend.observe_frame().expect("after");
    let after_expected = canonical_sha256_digest(&simulator_synthetic_frame_bytes(true));
    assert_eq!(after.digest, after_expected);
    assert_ne!(before.digest, after.digest);
    assert!(delta.guest_local_change);
    assert!(after.epoch > before.epoch);
    assert_postcondition_change(&before, &after).expect("postcondition");
    assert_eq!(
        after.captured_frame.as_ref().map(|ev| ev.source),
        Some(CapturedFrameSource::SyntheticSimulator)
    );
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn empty_oversize_malformed_caller_digest_and_tamper_fail_closed() {
    let empty = BoundedCapturedFrame::admit_from_source(
        1,
        Vec::new(),
        CapturedFrameSource::SyntheticSimulator,
        CapturedFrameMediaKind::SyntheticPayload,
        SYNTHETIC_FRAME_WIDTH,
        SYNTHETIC_FRAME_HEIGHT,
    )
    .expect_err("empty");
    assert_eq!(empty.code, HarnessErrorCode::InvalidState);

    let oversize = vec![0x22; MAX_CAPTURED_FRAME_BYTES + 1];
    let oversized = BoundedCapturedFrame::admit_from_source(
        1,
        oversize,
        CapturedFrameSource::SyntheticSimulator,
        CapturedFrameMediaKind::SyntheticPayload,
        SYNTHETIC_FRAME_WIDTH,
        SYNTHETIC_FRAME_HEIGHT,
    )
    .expect_err("oversize");
    assert_eq!(oversized.code, HarnessErrorCode::InvalidState);
    assert!(!oversized.message.contains(needle()));

    let caller = admit_captured_frame_with_claimed_digest(
        simulator_synthetic_frame_bytes(false),
        "sha256:contained-browser-frame:1:link=false",
        1,
        CapturedFrameSource::SyntheticSimulator,
        CapturedFrameMediaKind::SyntheticPayload,
        SYNTHETIC_FRAME_WIDTH,
        SYNTHETIC_FRAME_HEIGHT,
    )
    .expect_err("caller digest");
    assert_eq!(caller.code, HarnessErrorCode::InvalidState);

    let capture = BoundedCapturedFrame::admit_simulator_capture(1, true).expect("admit");
    let mut tampered = capture.captured_bytes().to_vec();
    let idx = tampered.len() - 1;
    tampered[idx] ^= 0x01;
    let tamper = capture
        .public_evidence()
        .verify_against_bytes(&tampered)
        .expect_err("tamper");
    assert_eq!(tamper.code, HarnessErrorCode::InvalidState);

    let mut malformed = capture.to_guest_frame(true);
    malformed.digest = "sha256:contained-browser-frame:1:link=true".into();
    if let Some(ev) = malformed.captured_frame.as_mut() {
        ev.digest = malformed.digest.clone();
    }
    assert!(require_frame_for_postcondition(&malformed).is_err());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn stale_misbound_and_source_upgrade_fail_closed() {
    let capture = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("admit");
    let mut stale = capture.to_guest_frame(false);
    stale.epoch = 8;
    let stale_err = require_frame_for_postcondition(&stale).expect_err("stale");
    assert_eq!(stale_err.code, HarnessErrorCode::InvalidState);

    let mut misbound = capture.to_guest_frame(false);
    if let Some(ev) = misbound.captured_frame.as_mut() {
        ev.digest = canonical_sha256_digest(&simulator_synthetic_frame_bytes(true));
    }
    let misbound_err = require_frame_for_postcondition(&misbound).expect_err("misbound");
    assert_eq!(misbound_err.code, HarnessErrorCode::InvalidState);

    let engine = BoundedCapturedFrame::admit_from_source(
        1,
        simulator_synthetic_frame_bytes(false),
        CapturedFrameSource::BrowserEngine,
        CapturedFrameMediaKind::EngineRgba8,
        SYNTHETIC_FRAME_WIDTH,
        SYNTHETIC_FRAME_HEIGHT,
    )
    .expect_err("source upgrade");
    assert_eq!(engine.code, HarnessErrorCode::BackendUnavailable);

    let mut upgraded = capture.to_guest_frame(false);
    if let Some(ev) = upgraded.captured_frame.as_mut() {
        ev.source = CapturedFrameSource::BrowserEngine;
        ev.media_kind = CapturedFrameMediaKind::EngineRgba8;
    }
    let upgraded_err = require_frame_for_postcondition(&upgraded).expect_err("upgrade");
    assert_eq!(upgraded_err.code, HarnessErrorCode::BackendUnavailable);
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn public_sealed_snapshot_and_errors_do_not_serialize_raw_bytes() {
    let dir = TempDir::new().expect("tempdir");
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("permitted")
    .with_snapshot_root(dir.path());
    harness.boot().expect("boot");
    let frame = harness.observe_frame().expect("frame");
    let frame_json = serde_json::to_string(&frame).expect("frame json");
    assert_no_raw_leak(&frame_json);
    assert!(is_canonical_sha256_digest(&frame.digest));

    harness
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect("click");
    harness.stop().expect("stop");

    let snapshot_json =
        std::fs::read_to_string(dir.path().join(SNAPSHOT_FILE)).expect("snapshot file");
    assert_no_raw_leak(&snapshot_json);
    let snapshot: HarnessSnapshot = serde_json::from_str(&snapshot_json).expect("snapshot");
    assert_eq!(
        snapshot.lifecycle.evidence_class,
        ProofEvidenceClass::ContainedBrowser
    );

    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer.run_contained_browser_dry_run().expect("dry-run");
    assert_eq!(evidence.nonclaim, CONTAINED_BROWSER_DRY_RUN_NONCLAIM);
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.nonclaim.contains("real browser capture"));
    let frames = evidence.captured_frames.as_ref().expect("sealed frames");
    assert_eq!(
        frames.before.source,
        CapturedFrameSource::SyntheticSimulator
    );
    assert_eq!(
        frames.before.media_kind,
        CapturedFrameMediaKind::SyntheticPayload
    );
    assert_ne!(frames.before.digest, frames.after.digest);

    let pack = seal_contained_browser_dry_run_pack(evidence);
    let pack_json = serialize_evidence_pack(&pack).expect("pack json");
    assert_no_raw_leak(&pack_json);
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn synthetic_nonclaim_and_computer_mode_stay_false() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer.run_contained_browser_dry_run().expect("dry-run");
    assert_eq!(
        evidence.evidence_class,
        ProofEvidenceClass::ContainedBrowser
    );
    assert!(!evidence.isolation_pass_claimed);
    assert!(!evidence.vf_pass_claimed);
    assert_eq!(evidence.nonclaim, CONTAINED_BROWSER_DRY_RUN_NONCLAIM);
    assert!(evidence.nonclaim.contains("not isolation PASS"));
    let pack_json = serialize_evidence_pack(&seal_contained_browser_dry_run_pack(evidence.clone()))
        .expect("json");
    assert!(!pack_json.contains("isolationPassClaimed\": true"));
    assert!(!pack_json.contains("real browser"));
    assert!(!pack_json.contains("browserEngine"));
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn stop_fence_and_restart_invariants_unchanged() {
    run_contained_browser_stop_fence_regression(HostSentinelSnapshot::synthetic_baseline())
        .expect("fence");

    let dir = TempDir::new().expect("tempdir");
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("permitted")
    .with_snapshot_root(dir.path());
    harness.boot().expect("boot");
    harness.schedule_uncertain_on_next_inject();
    harness
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect_err("uncertain");
    let stop = harness.stop().expect("stop");
    assert_eq!(stop.disposition, Some(GuestLifecycleDisposition::Uncertain));
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    harness.channels().assert_all_destroyed().expect("channels");

    let mut restarted = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("permitted")
    .with_snapshot_root(dir.path());
    let recovered = restarted.recover_after_restart().expect("recover");
    assert_eq!(
        recovered.disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );
    let replay = restarted
        .retry_inject_after_uncertain(GuestLocalAction::ClickGuestButton)
        .expect_err("no replay");
    assert_eq!(replay.code, HarnessErrorCode::AutoRetryForbidden);

    let mut fenced = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("permitted");
    fenced.boot().expect("boot");
    fenced.stop().expect("stop");
    let inject = fenced
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect_err("fenced");
    assert_eq!(inject.code, HarnessErrorCode::InjectFenced);
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn verifier_rejects_source_upgraded_or_missing_captured_frames() {
    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let evidence = sequencer.run_contained_browser_dry_run().expect("dry-run");
    let mut pack = seal_contained_browser_dry_run_pack(evidence);
    {
        let cb = pack.contained_browser.as_mut().expect("cb");
        let frames = cb.captured_frames.as_mut().expect("frames");
        frames.after.source = CapturedFrameSource::BrowserEngine;
        frames.after.media_kind = CapturedFrameMediaKind::EngineRgba8;
    }
    let decision = verify_evidence_pack(&pack);
    assert!(!decision.accepted);
    assert_eq!(
        decision.code,
        EvidenceVerifierCode::CapturedFrameEvidenceInvalid
    );

    let evidence = sequencer.run_contained_browser_dry_run().expect("dry-run");
    let mut pack = seal_contained_browser_dry_run_pack(evidence);
    pack.contained_browser.as_mut().expect("cb").captured_frames = None;
    let missing = verify_evidence_pack(&pack);
    assert!(!missing.accepted);
    assert_eq!(
        missing.code,
        EvidenceVerifierCode::CapturedFrameEvidenceInvalid
    );
}

#[test]
fn browser_engine_capture_entry_never_fabricates_bytes_or_pass() {
    let err = admit_browser_engine_capture(b"pretend-engine-raster".to_vec(), 64, 36)
        .expect_err("unwired");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(err.message.contains("not wired"));
    assert!(!err.message.contains(needle()));
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}
