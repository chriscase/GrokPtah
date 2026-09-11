//! Content-addressed SyntheticGuest frame digests (Phase-1 #286 follow-on).
//!
//! Proves GuestFrame digests are canonical SHA-256 of private bounded synthetic
//! bytes, that button-state changes the digest while same-state / epoch-only
//! observations do not, that public JSON never leaks payload or the legacy
//! `synthetic-frame` label, and that Synthetic evidence stays non-qualifying.
//! Does not claim isolation PASS, VF PASS, or enable admission.

use grokptah_isolated_surface::{
    canonical_sha256_digest, is_canonical_sha256_digest, seal_synthetic_harness_pack,
    serialize_evidence_pack, simulator_synthetic_frame_bytes, verify_evidence_pack,
    GuestLocalAction, HostSentinelSnapshot, InjectOutcome, IsolatedSurfaceBackend,
    IsolatedSurfaceHarness, ProofEvidenceClass, Sep18NoModelProofSequencer, SyntheticGuest,
    SYNTHETIC_FRAME_PAYLOAD_NEEDLE, SYNTHETIC_HARNESS_NONCLAIM,
};

fn needle() -> &'static str {
    std::str::from_utf8(SYNTHETIC_FRAME_PAYLOAD_NEEDLE).expect("needle utf8")
}

fn assert_no_raw_or_legacy_leak(serialized: &str) {
    assert!(
        !serialized.contains(needle()),
        "raw synthetic payload leaked into serialized form"
    );
    assert!(!serialized.contains("GROKPTAH-SYNTHETIC-BROWSER-FRAME-v1"));
    assert!(!serialized.contains("synthetic-frame"));
    assert!(!serialized.contains("raw-synthetic-frame-pixels"));
    assert!(!serialized.contains("capturedBytes"));
}

#[test]
fn synthetic_guest_digest_equals_recompute_from_private_bytes() {
    let mut guest = SyntheticGuest::new();
    let frame = guest.boot().expect("boot");
    let expected = canonical_sha256_digest(&simulator_synthetic_frame_bytes(false));
    assert_eq!(frame.digest, expected);
    assert!(is_canonical_sha256_digest(&frame.digest));
    assert_eq!(frame.digest.len(), "sha256:".len() + 64);
    assert!(frame.captured_frame.is_none());
    assert_eq!(guest.evidence_class(), ProofEvidenceClass::Synthetic);
    assert!(!frame.digest.contains("synthetic-frame"));
    assert!(!frame.digest.contains("btn="));
}

#[test]
fn button_state_changes_digest_repeated_same_state_does_not() {
    let mut guest = SyntheticGuest::new();
    let before = guest.boot().expect("boot");
    let again = guest.observe_frame().expect("observe");
    assert_eq!(before.digest, again.digest);
    assert_eq!(before.epoch, again.epoch);

    let click = match guest
        .inject_guest_local(GuestLocalAction::ClickGuestButton)
        .expect("click")
    {
        InjectOutcome::Changed(delta) => delta,
        other => panic!("expected Changed, got {other:?}"),
    };
    let after = guest.observe_frame().expect("after click");
    assert_ne!(after.digest, before.digest);
    assert!(click.guest_local_change);
    assert!(after.epoch > before.epoch);
    assert_eq!(
        after.digest,
        canonical_sha256_digest(&simulator_synthetic_frame_bytes(true))
    );
    let reobserve = guest.observe_frame().expect("reobserve");
    assert_eq!(reobserve.digest, after.digest);
    assert_eq!(reobserve.epoch, after.epoch);
    assert!(after.captured_frame.is_none());
}

#[test]
fn epoch_or_label_alone_cannot_fabricate_digest() {
    let mut guest = SyntheticGuest::new();
    let first = guest.boot().expect("boot");
    let typed = match guest
        .inject_guest_local(GuestLocalAction::TypeGuestText)
        .expect("type")
    {
        InjectOutcome::Changed(delta) => delta,
        other => panic!("expected Changed, got {other:?}"),
    };
    let later = guest.observe_frame().expect("later");
    assert!(later.epoch > first.epoch);
    assert_eq!(later.digest, first.digest);
    assert!(!typed.guest_local_change);
    assert_eq!(typed.before_digest, typed.after_digest);
    assert_eq!(
        later.digest,
        canonical_sha256_digest(&simulator_synthetic_frame_bytes(false))
    );
    assert!(!later.digest.contains("synthetic-frame"));
    assert!(!later.digest.contains("btn="));
}

#[test]
fn public_frames_and_packs_omit_payload_and_legacy_label() {
    let mut guest = SyntheticGuest::new();
    let frame = guest.boot().expect("boot");
    let frame_json = serde_json::to_string(&frame).expect("frame json");
    assert_no_raw_or_legacy_leak(&frame_json);
    assert!(!frame_json.contains("capturedFrame"));
    assert!(is_canonical_sha256_digest(&frame.digest));

    let delta = match guest
        .inject_guest_local(GuestLocalAction::ClickGuestButton)
        .expect("click")
    {
        InjectOutcome::Changed(delta) => delta,
        other => panic!("expected Changed, got {other:?}"),
    };
    let delta_json = serde_json::to_string(&delta).expect("delta json");
    assert_no_raw_or_legacy_leak(&delta_json);

    let sequencer = Sep18NoModelProofSequencer::new(HostSentinelSnapshot::synthetic_baseline());
    let sealed = sequencer.run_happy_path().expect("happy path");
    assert_eq!(sealed.evidence_class, ProofEvidenceClass::Synthetic);
    assert_eq!(sealed.nonclaim, SYNTHETIC_HARNESS_NONCLAIM);
    let pack = seal_synthetic_harness_pack(sealed, true);
    let pack_json = serialize_evidence_pack(&pack).expect("pack json");
    assert_no_raw_or_legacy_leak(&pack_json);
    let decision = verify_evidence_pack(&pack);
    assert!(decision.accepted, "{decision:?}");
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[test]
fn synthetic_nonclaim_and_admission_stay_false() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert_eq!(harness.evidence_class(), ProofEvidenceClass::Synthetic);
    harness.boot().expect("boot");
    let frame = harness.observe_frame().expect("frame");
    assert!(frame.captured_frame.is_none());
    assert!(is_canonical_sha256_digest(&frame.digest));
    assert_eq!(
        frame.digest,
        canonical_sha256_digest(&simulator_synthetic_frame_bytes(false))
    );
    harness
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect("click");
    harness.stop().expect("stop");
    assert!(SYNTHETIC_HARNESS_NONCLAIM.contains("ineligible"));
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}
