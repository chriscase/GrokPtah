//! Snapshot issuance, token authentication, and action-time authorization.

use std::sync::Arc;

use super::support::{Fixture, context, make_managed_worktree, principal};
use crate::{
    AuthorizationContext, CandidateRoot, PolicyInputs, REPLAY_POLICY, RootKind, SnapshotStore,
    SourceViewError, TOKEN_VERSION, TestClock,
};

#[test]
fn a_snapshot_names_every_root_by_opaque_token_only() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();

    assert_eq!(snapshot.roots.len(), 1);
    let root = &snapshot.roots[0];
    assert!(root.token.starts_with(&format!("{TOKEN_VERSION}.")));
    assert_eq!(root.kind, RootKind::Workspace);
    assert_eq!(snapshot.replay_policy, REPLAY_POLICY);

    // The wire form carries identity, never location.
    let json = serde_json::to_string(&snapshot).expect("serialize");
    assert!(
        !json.contains(&fixture.root.display().to_string()),
        "a snapshot must not carry the absolute root path",
    );
    assert_eq!(root.path_digest.len(), 64);
    assert_eq!(root.identity_digest.len(), 64);
}

#[test]
fn issuing_a_snapshot_is_non_mutating_and_repeatable() {
    let fixture = Fixture::new();
    let first = fixture.snapshot();
    let second = fixture.snapshot();

    assert_ne!(
        first.snapshot_id, second.snapshot_id,
        "each issue is distinct"
    );
    assert!(second.revision > first.revision);
    assert_eq!(
        first.roots[0].path_digest, second.roots[0].path_digest,
        "the same directory keeps one identity across snapshots",
    );
    // Both remain usable: issuing does not invalidate an outstanding snapshot.
    assert!(fixture.open(&first.roots[0].token, "src/main.rs").is_ok());
    assert!(fixture.open(&second.roots[0].token, "src/main.rs").is_ok());
}

#[test]
fn an_empty_snapshot_is_an_answer_not_an_error() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot_with(&[]);
    assert!(snapshot.roots.is_empty());
    assert_eq!(snapshot.replay_policy, REPLAY_POLICY);
}

#[test]
fn an_unobservable_candidate_is_dropped_without_failing_the_snapshot() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot_with(&[
        CandidateRoot::workspace(&fixture.root),
        CandidateRoot::worktree(fixture.path("does/not/exist"), "run-gone"),
    ]);
    assert_eq!(
        snapshot.roots.len(),
        1,
        "one stale worktree must not blind the workspace"
    );
    assert_eq!(snapshot.roots[0].kind, RootKind::Workspace);
}

#[test]
fn each_root_gets_a_distinct_token_and_a_token_selects_exactly_one() {
    let fixture = Fixture::new();
    let worktree = make_managed_worktree(&fixture.root, "run-7");
    crate::tests::support::write_file(&worktree, "only-here.txt", b"worktree copy\n");

    let snapshot = fixture.snapshot_with(&[
        CandidateRoot::workspace(&fixture.root),
        CandidateRoot::worktree(&worktree, "run-7"),
    ]);
    assert_eq!(snapshot.roots.len(), 2);
    assert_ne!(snapshot.roots[0].token, snapshot.roots[1].token);

    // The worktree token reaches the worktree file; the workspace token does
    // not. There is no ordering rule that could silently substitute one.
    let worktree_token = &snapshot.roots[1].token;
    let workspace_token = &snapshot.roots[0].token;
    assert!(fixture.open(worktree_token, "only-here.txt").is_ok());
    assert!(matches!(
        fixture.open(workspace_token, "only-here.txt"),
        Err(SourceViewError::NotFound { .. }),
    ));
}

#[test]
fn a_tampered_token_fails_authentication() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let mut parts: Vec<&str> = token.split('.').collect();

    // Flip one hex digit of the tag.
    let mac = parts[3].to_string();
    let flipped = format!(
        "{}{}",
        if mac.starts_with('0') { '1' } else { '0' },
        &mac[1..]
    );
    parts[3] = &flipped;
    let forged = parts.join(".");

    assert_eq!(
        fixture.open(&forged, "src/main.rs").unwrap_err(),
        SourceViewError::TokenSignatureInvalid,
    );
}

#[test]
fn a_token_for_another_root_index_in_the_same_snapshot_is_refused() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let parts: Vec<&str> = token.split('.').collect();
    // Index 1 exists in no snapshot with one root; the tag also will not
    // verify, but the index check fails first and fails closed either way.
    let reindexed = format!("{}.{}.1.{}", parts[0], parts[1], parts[3]);
    assert_eq!(
        fixture.open(&reindexed, "src/main.rs").unwrap_err(),
        SourceViewError::UnknownRoot,
    );
}

#[test]
fn malformed_tokens_are_refused_before_any_filesystem_access() {
    let fixture = Fixture::new();
    let good = fixture.token();
    let parts: Vec<&str> = good.split('.').collect();
    let cases = vec![
        String::new(),
        "not-a-token".to_string(),
        format!("sv0.{}.0.{}", parts[1], parts[3]),
        format!("{}.{}.0", parts[0], parts[1]),
        format!("{}.{}.0.{}.extra", parts[0], parts[1], parts[3]),
        format!("{}.tooshort.0.{}", parts[0], parts[3]),
        format!("{}.{}.00.{}", parts[0], parts[1], parts[3]),
        format!("{}.{}.+1.{}", parts[0], parts[1], parts[3]),
        format!("{}.{}.0.zzzz", parts[0], parts[1]),
        format!("{}.{}.0.{}", parts[0], parts[1], &parts[3][..30]),
    ];
    for case in cases {
        assert_eq!(
            fixture.open(&case, "src/main.rs").unwrap_err(),
            SourceViewError::TokenMalformed,
            "token `{case}` must be refused as malformed",
        );
    }
}

#[test]
fn a_token_expires_exactly_at_its_deadline() {
    let clock = Arc::new(TestClock::new(super::support::START_MS));
    let fixture = Fixture::with_clock(clock.clone());
    let store = SnapshotStore::new(super::support::TEST_KEY, clock.clone()).with_ttl_ms(1_000);
    let snapshot = store.issue(&fixture.context, &[CandidateRoot::workspace(&fixture.root)]);
    let token = snapshot.roots[0].token.clone();
    let request = crate::SourceRequest::new(&token, "src/main.rs");

    clock.advance_ms(999);
    assert!(
        crate::open_document(
            &store,
            &fixture.context,
            &request,
            crate::PathPolicy::host()
        )
        .is_ok(),
        "a token is valid up to but not including its deadline",
    );

    clock.advance_ms(1);
    let error = crate::open_document(
        &store,
        &fixture.context,
        &request,
        crate::PathPolicy::host(),
    )
    .unwrap_err();
    // At the deadline the sweep removes the entry, so the refusal is
    // `snapshot_unknown`; either way it is an authorization refusal and the
    // read never happens.
    assert!(
        matches!(
            error,
            SourceViewError::TokenExpired | SourceViewError::SnapshotUnknown
        ),
        "expected an expiry refusal, got {error:?}",
    );
    assert!(error.is_authorization());
}

#[test]
fn revocation_refuses_an_otherwise_valid_token() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let token = snapshot.roots[0].token.clone();
    assert!(fixture.open(&token, "src/main.rs").is_ok());

    assert!(fixture.store.revoke(&snapshot.snapshot_id));
    assert_eq!(
        fixture.open(&token, "src/main.rs").unwrap_err(),
        SourceViewError::TokenRevoked,
    );
    assert!(!fixture.store.revoke("no-such-snapshot"));
}

#[test]
fn revoking_a_principal_kills_every_snapshot_it_holds() {
    let fixture = Fixture::new();
    let first = fixture.snapshot();
    let second = fixture.snapshot();

    let revoked = fixture
        .store
        .revoke_for_principal(&fixture.context.principal.fingerprint());
    assert_eq!(revoked, 2);
    for snapshot in [first, second] {
        assert_eq!(
            fixture
                .open(&snapshot.roots[0].token, "src/main.rs")
                .unwrap_err(),
            SourceViewError::TokenRevoked,
        );
    }
}

#[test]
fn a_token_cannot_be_replayed_by_another_principal() {
    let fixture = Fixture::new();
    let token = fixture.token();

    for other in [
        AuthorizationContext::new(principal("session-2"), super::support::policy("primary")),
        AuthorizationContext::new(
            crate::Principal::new("user-2", "tenant-a", "project-x", "session-1"),
            super::support::policy("primary"),
        ),
        AuthorizationContext::new(
            crate::Principal::new("user-1", "tenant-b", "project-x", "session-1"),
            super::support::policy("primary"),
        ),
        AuthorizationContext::new(
            crate::Principal::new("user-1", "tenant-a", "project-y", "session-1"),
            super::support::policy("primary"),
        ),
    ] {
        assert_eq!(
            fixture.open_as(&other, &token, "src/main.rs").unwrap_err(),
            SourceViewError::PrincipalMismatch,
            "every principal field must be part of the binding",
        );
    }
}

#[test]
fn a_token_is_refused_after_policy_drifts() {
    let fixture = Fixture::new();
    let token = fixture.token();
    assert!(fixture.open(&token, "src/main.rs").is_ok());

    let drifted = context("session-1", "workspace-was-changed");
    assert_eq!(
        fixture
            .open_as(&drifted, &token, "src/main.rs")
            .unwrap_err(),
        SourceViewError::PolicyDrift,
    );
}

#[test]
fn policy_drift_notices_a_reordering_not_only_a_value_change() {
    let mut forward = PolicyInputs::new();
    forward.push("a", "1");
    forward.push("b", "2");
    let mut reversed = PolicyInputs::new();
    reversed.push("b", "2");
    reversed.push("a", "1");
    assert_ne!(
        forward.fingerprint(),
        reversed.fingerprint(),
        "policy order is significant, so a reordered policy is drift",
    );
}

#[test]
fn fingerprints_resist_field_boundary_confusion() {
    let split_one = crate::Principal::new("ab", "c", "p", "s");
    let split_two = crate::Principal::new("a", "bc", "p", "s");
    assert_ne!(
        split_one.fingerprint(),
        split_two.fingerprint(),
        "length-prefixed fields cannot be shifted across the boundary",
    );
}

#[test]
fn an_expired_snapshot_is_swept_and_the_registry_stays_bounded() {
    let clock = Arc::new(TestClock::new(super::support::START_MS));
    let fixture = Fixture::with_clock(clock.clone());
    let store = SnapshotStore::new(super::support::TEST_KEY, clock.clone()).with_ttl_ms(500);

    for _ in 0..5 {
        store.issue(&fixture.context, &[CandidateRoot::workspace(&fixture.root)]);
    }
    assert_eq!(store.len(), 5);

    clock.advance_ms(500);
    assert_eq!(store.sweep(), 5);
    assert!(store.is_empty());
}

#[test]
fn the_registry_evicts_the_oldest_revision_past_its_cap() {
    let fixture = Fixture::with_capacity(2);
    let first = fixture.snapshot();
    let second = fixture.snapshot();
    let third = fixture.snapshot();

    assert_eq!(fixture.store.len(), 2);
    assert_eq!(
        fixture
            .open(&first.roots[0].token, "src/main.rs")
            .unwrap_err(),
        SourceViewError::SnapshotUnknown,
        "an evicted token fails closed rather than being honoured",
    );
    assert!(fixture.open(&second.roots[0].token, "src/main.rs").is_ok());
    assert!(fixture.open(&third.roots[0].token, "src/main.rs").is_ok());
}

#[test]
fn a_token_minted_under_a_different_key_never_verifies() {
    let fixture = Fixture::new();
    let other_store = SnapshotStore::new([9u8; 32], fixture.clock.clone());
    let foreign = other_store.issue(&fixture.context, &[CandidateRoot::workspace(&fixture.root)]);

    // The foreign snapshot id is unknown to this store, so it fails closed at
    // the first lookup rather than reaching signature comparison.
    assert_eq!(
        fixture
            .open(&foreign.roots[0].token, "src/main.rs")
            .unwrap_err(),
        SourceViewError::SnapshotUnknown,
    );
}

#[test]
fn replay_within_the_validity_window_is_permitted_by_design() {
    let fixture = Fixture::with_ttl(60_000);
    let token = fixture.token();
    for _ in 0..3 {
        assert!(
            fixture.open(&token, "src/main.rs").is_ok(),
            "reads are idempotent, so paging a file is not a replay attack",
        );
        fixture.clock.advance_ms(1_000);
    }
}

#[test]
fn every_authorization_refusal_is_classified_as_one() {
    for error in [
        SourceViewError::NoApprovedRoot,
        SourceViewError::SnapshotUnknown,
        SourceViewError::TokenMalformed,
        SourceViewError::TokenSignatureInvalid,
        SourceViewError::TokenExpired,
        SourceViewError::TokenRevoked,
        SourceViewError::PrincipalMismatch,
        SourceViewError::PolicyDrift,
        SourceViewError::UnknownRoot,
    ] {
        assert!(
            error.is_authorization(),
            "{} must be authorization",
            error.code()
        );
    }
    for error in [
        SourceViewError::ParentEscape,
        SourceViewError::RangeInvalid,
        SourceViewError::DocumentChanged,
    ] {
        assert!(
            !error.is_authorization(),
            "{} must not be authorization",
            error.code()
        );
    }
}

#[test]
fn the_default_registry_cap_stays_inside_a_conservative_descriptor_budget() {
    // Each snapshot holds one open directory handle per root. The cap is
    // therefore a file-descriptor budget, and macOS still ships a 256
    // soft limit; a few roots per snapshot must stay well inside it.
    let fixture = Fixture::new();
    for _ in 0..40 {
        fixture.snapshot();
    }
    assert!(
        fixture.store.len() <= 16,
        "the default cap must bound held directory handles, got {}",
        fixture.store.len(),
    );
}

#[test]
fn sweeping_releases_the_directory_handles_a_snapshot_held() {
    let clock = Arc::new(TestClock::new(super::support::START_MS));
    let fixture = Fixture::with_clock(clock.clone());
    let store = SnapshotStore::new(super::support::TEST_KEY, clock.clone()).with_ttl_ms(1_000);
    for _ in 0..8 {
        store.issue(&fixture.context, &[CandidateRoot::workspace(&fixture.root)]);
    }
    assert_eq!(store.len(), 8);
    clock.advance_ms(1_001);
    assert_eq!(store.sweep(), 8);
    assert!(
        store.is_empty(),
        "an idle tick must release every expired handle"
    );
}
