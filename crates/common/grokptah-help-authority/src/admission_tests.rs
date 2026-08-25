//! Route admission, replay, and outcome binding.

use crate::admission::*;
use crate::grant::GrantMintingKey;

const CORPUS: &str = "sha256:aaaa";
const INDEX: &str = "sha256:bbbb";
const MANIFEST: &str = "sha256:cccc";
const REQUEST: &str = "sha256:dddd";

fn key() -> GrantMintingKey {
    GrantMintingKey::new(vec![9u8; 32]).expect("key")
}

fn route() -> AnswerRoute {
    AnswerRoute {
        provider_id: "company-gateway".into(),
        tenant_id: "tenant-a".into(),
        project_id: Some("proj-1".into()),
        model_id: "review-model".into(),
    }
}

fn admit() -> AnswerAdmission {
    mint_admission(
        &key(),
        &route(),
        REQUEST,
        CORPUS,
        INDEX,
        MANIFEST,
        42,
        "policy-7",
        1_000,
        60_000,
    )
    .expect("mints")
}

fn expectation() -> AdmissionExpectation {
    AdmissionExpectation {
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: MANIFEST.into(),
        current_revision: 42,
        policy_revision: "policy-7".into(),
        request_digest: REQUEST.into(),
        now_ms: 1_500,
    }
}

#[test]
fn a_host_minted_admission_verifies() {
    assert_eq!(verify_admission(&key(), &admit(), &expectation()), Ok(()));
}

type Mutation = (&'static str, Box<dyn Fn(&mut AnswerAdmission)>);

#[test]
fn every_field_the_mac_covers_is_detected_when_edited() {
    // The defect this replaces: a caller-computed digest stays self-consistent
    // no matter what the caller writes into it. Each mutation below is a route
    // substitution that the old scheme would have accepted.
    let mutations: Vec<Mutation> = vec![
        (
            "provider",
            Box::new(|a: &mut AnswerAdmission| a.route.provider_id = "attacker-gateway".into()),
        ),
        (
            "tenant",
            Box::new(|a: &mut AnswerAdmission| a.route.tenant_id = "tenant-z".into()),
        ),
        (
            "project",
            Box::new(|a: &mut AnswerAdmission| a.route.project_id = Some("proj-9".into())),
        ),
        (
            "project cleared",
            Box::new(|a: &mut AnswerAdmission| a.route.project_id = None),
        ),
        (
            "model",
            Box::new(|a: &mut AnswerAdmission| a.route.model_id = "unreviewed-model".into()),
        ),
        (
            "grant revision",
            Box::new(|a: &mut AnswerAdmission| a.grant_revision = 41),
        ),
        (
            "policy revision",
            Box::new(|a: &mut AnswerAdmission| a.policy_revision = "policy-6".into()),
        ),
        (
            "corpus digest",
            Box::new(|a: &mut AnswerAdmission| a.corpus_digest = "sha256:other".into()),
        ),
        (
            "index digest",
            Box::new(|a: &mut AnswerAdmission| a.index_digest = "sha256:other".into()),
        ),
        (
            "manifest digest",
            Box::new(|a: &mut AnswerAdmission| a.manifest_digest = "sha256:other".into()),
        ),
        (
            "request digest",
            Box::new(|a: &mut AnswerAdmission| a.request_digest = "sha256:other".into()),
        ),
        (
            "admission id",
            Box::new(|a: &mut AnswerAdmission| a.admission_id = "sha256:chosen".into()),
        ),
        (
            "issued at",
            Box::new(|a: &mut AnswerAdmission| a.issued_at_ms = 0),
        ),
        (
            "expires at",
            Box::new(|a: &mut AnswerAdmission| a.expires_at_ms = u64::MAX),
        ),
        (
            "mac",
            Box::new(|a: &mut AnswerAdmission| a.mac = "hmac-sha256:0000".into()),
        ),
    ];

    for (label, mutate) in mutations {
        let mut edited = admit();
        mutate(&mut edited);
        assert_eq!(
            verify_admission(&key(), &edited, &expectation()),
            Err(AdmissionRejection::Forged),
            "editing the {label} must break the MAC"
        );
    }
}

#[test]
fn an_admission_from_a_foreign_key_is_forged() {
    let foreign = GrantMintingKey::new(vec![1u8; 32]).expect("key");
    let admission = mint_admission(
        &foreign,
        &route(),
        REQUEST,
        CORPUS,
        INDEX,
        MANIFEST,
        42,
        "policy-7",
        1_000,
        60_000,
    )
    .expect("mints");
    assert_eq!(
        verify_admission(&key(), &admission, &expectation()),
        Err(AdmissionRejection::Forged)
    );
}

#[test]
fn an_admission_cannot_be_replayed_on_a_different_request() {
    // The whole point of binding the request digest: an admission obtained for
    // one question must not carry a different one.
    let admission = admit();
    let mut other_request = expectation();
    other_request.request_digest = "sha256:a-different-question".into();
    assert_eq!(
        verify_admission(&key(), &admission, &other_request),
        Err(AdmissionRejection::RequestMismatch)
    );
}

#[test]
fn a_superseded_revision_is_refused() {
    let admission = admit();
    let mut moved_on = expectation();
    moved_on.current_revision = 43;
    assert_eq!(
        verify_admission(&key(), &admission, &moved_on),
        Err(AdmissionRejection::StaleRevision)
    );

    let mut repolicied = expectation();
    repolicied.policy_revision = "policy-8".into();
    assert_eq!(
        verify_admission(&key(), &admission, &repolicied),
        Err(AdmissionRejection::StaleRevision)
    );
}

#[test]
fn a_rebuilt_corpus_index_or_manifest_is_refused() {
    for mutate in [
        (|e: &mut AdmissionExpectation| e.corpus_digest = "sha256:new".into())
            as fn(&mut AdmissionExpectation),
        |e: &mut AdmissionExpectation| e.index_digest = "sha256:new".into(),
        |e: &mut AdmissionExpectation| e.manifest_digest = "sha256:new".into(),
    ] {
        let mut served = expectation();
        mutate(&mut served);
        assert_eq!(
            verify_admission(&key(), &admit(), &served),
            Err(AdmissionRejection::IndexMismatch)
        );
    }
}

#[test]
fn the_validity_window_is_closed_at_both_ends() {
    let admission = admit();
    let mut early = expectation();
    early.now_ms = 999;
    assert_eq!(
        verify_admission(&key(), &admission, &early),
        Err(AdmissionRejection::Expired)
    );

    let mut late = expectation();
    late.now_ms = 61_000;
    assert_eq!(
        verify_admission(&key(), &admission, &late),
        Err(AdmissionRejection::Expired)
    );

    // The expiry instant itself is outside the window.
    let mut boundary = expectation();
    boundary.now_ms = admission.expires_at_ms;
    assert_eq!(
        verify_admission(&key(), &admission, &boundary),
        Err(AdmissionRejection::Expired)
    );
}

#[test]
fn an_unbounded_lifetime_or_identifier_is_refused_at_mint_time() {
    assert_eq!(
        mint_admission(
            &key(),
            &route(),
            REQUEST,
            CORPUS,
            INDEX,
            MANIFEST,
            42,
            "policy-7",
            1_000,
            MAX_ADMISSION_LIFETIME_MS + 1,
        ),
        Err(AdmissionRejection::Bounds)
    );
    assert_eq!(
        mint_admission(
            &key(),
            &route(),
            REQUEST,
            CORPUS,
            INDEX,
            MANIFEST,
            42,
            "policy-7",
            1_000,
            0,
        ),
        Err(AdmissionRejection::Bounds)
    );

    let mut nameless = route();
    nameless.provider_id = String::new();
    assert_eq!(
        mint_admission(
            &key(),
            &nameless,
            REQUEST,
            CORPUS,
            INDEX,
            MANIFEST,
            42,
            "policy-7",
            1_000,
            60_000,
        ),
        Err(AdmissionRejection::Bounds)
    );

    let mut oversized = route();
    oversized.model_id = "m".repeat(crate::MAX_ID_BYTES + 1);
    assert_eq!(
        mint_admission(
            &key(),
            &oversized,
            REQUEST,
            CORPUS,
            INDEX,
            MANIFEST,
            42,
            "policy-7",
            1_000,
            60_000,
        ),
        Err(AdmissionRejection::Bounds)
    );
}

#[test]
fn a_project_named_like_the_sentinel_is_not_the_same_as_no_project() {
    let mut sentinel = route();
    sentinel.project_id = Some("<none>".into());
    let with_sentinel = mint_admission(
        &key(),
        &sentinel,
        REQUEST,
        CORPUS,
        INDEX,
        MANIFEST,
        42,
        "policy-7",
        1_000,
        60_000,
    )
    .expect("mints");

    let mut absent = route();
    absent.project_id = None;
    let without = mint_admission(
        &key(),
        &absent,
        REQUEST,
        CORPUS,
        INDEX,
        MANIFEST,
        42,
        "policy-7",
        1_000,
        60_000,
    )
    .expect("mints");

    assert_ne!(with_sentinel.mac, without.mac);
    assert_ne!(with_sentinel.admission_id, without.admission_id);
}

// ------------------------------------------------------- outcome binding

fn citation(claim_index: u32, chunk: &str, start: u32, end: u32) -> BoundCitation {
    BoundCitation {
        claim_index,
        chunk_id: chunk.into(),
        chunk_digest: "sha256:chunk".into(),
        source_id: "durable.lifecycle".into(),
        start_utf8: start,
        end_utf8: end,
    }
}

#[test]
fn the_outcome_digest_follows_every_part_of_the_answer() {
    let admission = admit();
    let base = bind_outcome(&admission, "answer", "unsure", &[citation(0, "c1", 0, 10)]);

    assert_ne!(
        base,
        bind_outcome(
            &admission,
            "different",
            "unsure",
            &[citation(0, "c1", 0, 10)]
        )
    );
    assert_ne!(
        base,
        bind_outcome(&admission, "answer", "certain", &[citation(0, "c1", 0, 10)])
    );
    // A citation re-pointed at different bytes of the same chunk.
    assert_ne!(
        base,
        bind_outcome(&admission, "answer", "unsure", &[citation(0, "c1", 5, 15)])
    );
    // The same citation attached to a different claim.
    assert_ne!(
        base,
        bind_outcome(&admission, "answer", "unsure", &[citation(1, "c1", 0, 10)])
    );
}

#[test]
fn the_outcome_digest_is_bound_to_its_admission() {
    let first = admit();
    let second = mint_admission(
        &key(),
        &route(),
        "sha256:another-request",
        CORPUS,
        INDEX,
        MANIFEST,
        42,
        "policy-7",
        1_000,
        60_000,
    )
    .expect("mints");
    assert_ne!(
        bind_outcome(&first, "answer", "unsure", &[]),
        bind_outcome(&second, "answer", "unsure", &[])
    );
}

#[test]
fn reordering_citations_changes_the_outcome_digest() {
    let admission = admit();
    let forward = [citation(0, "c1", 0, 10), citation(1, "c2", 0, 10)];
    let reversed = [citation(1, "c2", 0, 10), citation(0, "c1", 0, 10)];
    assert_ne!(
        bind_outcome(&admission, "answer", "unsure", &forward),
        bind_outcome(&admission, "answer", "unsure", &reversed)
    );
}

#[test]
fn overlapping_citations_are_detected_in_utf8_space() {
    assert!(citations_overlap(&[
        citation(0, "c1", 0, 20),
        citation(1, "c1", 10, 30),
    ]));
    // Adjacent but disjoint ranges are two pieces of evidence, not one.
    assert!(!citations_overlap(&[
        citation(0, "c1", 0, 20),
        citation(1, "c1", 20, 30),
    ]));
    // Same offsets in different chunks never overlap.
    assert!(!citations_overlap(&[
        citation(0, "c1", 0, 20),
        citation(1, "c2", 0, 20),
    ]));
}
