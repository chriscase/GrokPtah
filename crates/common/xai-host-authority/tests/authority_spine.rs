//! Adversarial tests for the canonical authority spine.
//!
//! Each test names the attack it closes rather than the function it calls.

use std::path::PathBuf;

use xai_host_authority::*;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    authority: HostAuthority,
    auth: AuthContext,
    session: SessionId,
    workspace: WorkspaceId,
    resource: ResourceIncarnation,
}

const SECRET: &str = "s3cret-bearer-value";
const OWNER: &str = "account-1";

fn observation(tag: &str) -> ContentDigest {
    ContentDigest::of_bytes(tag.as_bytes())
}

fn request(body: &[u8]) -> RequestIdentity {
    RequestIdentity::new(
        "https://api.example.invalid/v1/chat",
        "POST",
        "openai-chat",
        b"provider-key",
        "grok-4",
        body,
    )
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let authority = HostAuthority::open(&root, OWNER).unwrap();
    authority
        .set_credentials(&[HostCredential::new("primary", SECRET).unwrap()], OWNER)
        .unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let session = authority.issue_session(&auth).unwrap();
    let workspace = authority
        .issue_workspace(&auth, &root.join("workspace"))
        .unwrap();
    let resource = authority
        .issue_resource(&auth, session, workspace, observation("frame-1"))
        .unwrap();
    Fixture {
        _dir: dir,
        root,
        authority,
        auth,
        session,
        workspace,
        resource,
    }
}

/// Mint a lease for `request` on the fixture's resource.
fn lease_for(f: &Fixture, req: &RequestIdentity) -> EffectLease {
    let cap = f
        .authority
        .seal_capability(&f.auth, f.resource, EffectClass::ProviderSend, 60_000)
        .unwrap();
    f.authority
        .mint_lease(&f.auth, &cap, req.digest(), 60_000)
        .unwrap()
}

// ───────────────────────────── Gate 1: principal root ─────────────────────────────

#[test]
fn bearer_must_match_a_durable_credential() {
    let f = fixture();
    assert!(matches!(
        f.authority.authenticate("wrong"),
        Err(AuthorityError::Unauthenticated)
    ));
    assert!(matches!(
        f.authority.authenticate(""),
        Err(AuthorityError::Unauthenticated)
    ));
    // The bearer prefix is accepted but is not itself authority.
    assert!(
        f.authority
            .authenticate(&format!("Bearer {SECRET}"))
            .is_ok()
    );
}

#[test]
fn rotating_a_secret_kills_the_previous_bearer_and_its_context() {
    let f = fixture();
    let old = f.auth.clone();

    f.authority
        .set_credentials(
            &[HostCredential::new("primary", "rotated-secret").unwrap()],
            OWNER,
        )
        .unwrap();

    // The old secret no longer authenticates.
    assert!(matches!(
        f.authority.authenticate(SECRET),
        Err(AuthorityError::Unauthenticated)
    ));
    // And a context captured under the old incarnation is stale, so a bearer
    // captured earlier cannot be resurrected against the new generation.
    assert!(matches!(
        f.authority.require_current(&old),
        Err(AuthorityError::StalePrincipal)
    ));
}

#[test]
fn reinstalling_the_same_credential_name_does_not_resurrect_old_authority() {
    let f = fixture();
    let old = f.auth.clone();

    // Rotate away and then back to the *original* secret under the same slot.
    f.authority
        .set_credentials(&[HostCredential::new("primary", "interim").unwrap()], OWNER)
        .unwrap();
    f.authority
        .set_credentials(&[HostCredential::new("primary", SECRET).unwrap()], OWNER)
        .unwrap();

    // The secret authenticates again, but as a *new* incarnation.
    let fresh = f.authority.authenticate(SECRET).unwrap();
    assert!(
        f.authority.require_current(&old).is_err(),
        "a context from before the rotation must not become current again"
    );
    assert_ne!(
        old.auth_generation(),
        fresh.auth_generation(),
        "re-installing a secret must advance the authentication generation"
    );
}

#[test]
fn removing_a_credential_revokes_everything_derived_from_it() {
    let f = fixture();
    let cap = f
        .authority
        .seal_capability(&f.auth, f.resource, EffectClass::ProviderSend, 60_000)
        .unwrap();

    // A second slot replaces the first entirely.
    f.authority
        .set_credentials(
            &[HostCredential::new("other", "another-secret").unwrap()],
            OWNER,
        )
        .unwrap();

    let other = f.authority.authenticate("another-secret").unwrap();
    // The resource issued to the removed principal is gone, not inherited.
    assert!(matches!(
        f.authority.resource_binding(&other, f.resource),
        Err(AuthorityError::UnknownResource)
    ));
    // And the capability sealed under it cannot be leased.
    assert!(
        f.authority
            .mint_lease(&other, &cap, observation("x"), 60_000)
            .is_err()
    );
}

#[test]
fn a_resource_the_host_never_issued_cannot_be_claimed_by_naming_it() {
    let f = fixture();
    let other = fixture();
    // `other.resource` is a perfectly well-formed incarnation — just not one
    // this host issued. Naming it must not create a binding for the caller.
    assert!(matches!(
        f.authority.resource_binding(&f.auth, other.resource),
        Err(AuthorityError::UnknownResource)
    ));
    assert!(matches!(
        f.authority
            .seal_capability(&f.auth, other.resource, EffectClass::ProviderSend, 60_000),
        Err(AuthorityError::UnknownResource)
    ));
}

#[test]
fn a_second_principal_cannot_use_the_first_principals_resource() {
    let dir = tempfile::tempdir().unwrap();
    let authority = HostAuthority::open(dir.path(), OWNER).unwrap();
    authority
        .set_credentials(
            &[
                HostCredential::new("a", "secret-a").unwrap(),
                HostCredential::new("b", "secret-b").unwrap(),
            ],
            OWNER,
        )
        .unwrap();
    let a = authority.authenticate("secret-a").unwrap();
    let b = authority.authenticate("secret-b").unwrap();
    assert_ne!(a.principal(), b.principal());

    let session = authority.issue_session(&a).unwrap();
    let workspace = authority.issue_workspace(&a, dir.path()).unwrap();
    let resource = authority
        .issue_resource(&a, session, workspace, observation("frame-1"))
        .unwrap();

    // b holds a live context, but not this resource.
    assert!(matches!(
        authority.resource_binding(&b, resource),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
    assert!(matches!(
        authority.seal_capability(&b, resource, EffectClass::ProviderSend, 60_000),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
    assert!(matches!(
        authority.record_observation(&b, resource, observation("frame-2")),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
}

#[test]
fn a_capability_cannot_be_leased_by_a_different_principal() {
    let dir = tempfile::tempdir().unwrap();
    let authority = HostAuthority::open(dir.path(), OWNER).unwrap();
    authority
        .set_credentials(
            &[
                HostCredential::new("a", "secret-a").unwrap(),
                HostCredential::new("b", "secret-b").unwrap(),
            ],
            OWNER,
        )
        .unwrap();
    let a = authority.authenticate("secret-a").unwrap();
    let b = authority.authenticate("secret-b").unwrap();
    let session = authority.issue_session(&a).unwrap();
    let workspace = authority.issue_workspace(&a, dir.path()).unwrap();
    let resource = authority
        .issue_resource(&a, session, workspace, observation("frame-1"))
        .unwrap();
    let cap = authority
        .seal_capability(&a, resource, EffectClass::ProviderSend, 60_000)
        .unwrap();

    assert!(matches!(
        authority.mint_lease(&b, &cap, observation("act"), 60_000),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
}

// ─────────────────────── Gate 2: sealed capabilities and leases ───────────────────────

#[test]
fn a_lease_is_one_use() {
    let f = fixture();
    let req = request(b"{\"m\":1}");
    let lease = lease_for(&f, &req);

    let permit = f
        .authority
        .begin_send(&f.auth, lease.clone(), &req)
        .unwrap();
    assert!(matches!(
        f.authority.settle_settled(permit),
        SendOutcome::Settled { .. }
    ));

    // Presenting the same lease again is a replay and must be refused.
    assert!(matches!(
        f.authority.begin_send(&f.auth, lease, &req),
        Err(AuthorityError::AlreadyConsumed)
    ));
}

#[test]
fn an_expired_capability_cannot_be_leased() {
    let f = fixture();
    let cap = f
        .authority
        .seal_capability(&f.auth, f.resource, EffectClass::ProviderSend, 1)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(matches!(
        f.authority
            .mint_lease(&f.auth, &cap, observation("act"), 60_000),
        Err(AuthorityError::Expired)
    ));
}

#[test]
fn rotating_the_capability_generation_invalidates_sealed_grants() {
    let f = fixture();
    let cap = f
        .authority
        .seal_capability(&f.auth, f.resource, EffectClass::ProviderSend, 60_000)
        .unwrap();
    f.authority.rotate_capability_generation().unwrap();

    // The old context is stale, and so is the grant.
    assert!(matches!(
        f.authority.require_current(&f.auth),
        Err(AuthorityError::StaleCapability)
    ));
    let fresh = f.authority.authenticate(SECRET).unwrap();
    assert!(matches!(
        f.authority
            .mint_lease(&fresh, &cap, observation("act"), 60_000),
        Err(AuthorityError::StaleCapability)
    ));
}

#[test]
fn a_capability_for_one_effect_class_does_not_authorise_another() {
    let f = fixture();
    let req = request(b"body");
    let cap = f
        .authority
        .seal_capability(&f.auth, f.resource, EffectClass::ComputerUseAct, 60_000)
        .unwrap();
    let lease = f
        .authority
        .mint_lease(&f.auth, &cap, req.digest(), 60_000)
        .unwrap();
    assert!(matches!(
        f.authority.begin_send(&f.auth, lease, &req),
        Err(AuthorityError::NotPermitted)
    ));
}

// ─────────────────────── Gate 3: the physical-send lattice ───────────────────────

#[test]
fn a_permit_is_bound_to_the_whole_request_identity() {
    // Every component of the request identity must be load-bearing: changing
    // any one of them after the lease was minted invalidates the send.
    let base = request(b"{\"m\":1}");
    let variants: Vec<(&str, RequestIdentity)> = vec![
        (
            "url",
            RequestIdentity::new(
                "https://evil.example.invalid/v1/chat",
                "POST",
                "openai-chat",
                b"provider-key",
                "grok-4",
                b"{\"m\":1}",
            ),
        ),
        (
            "method",
            RequestIdentity::new(
                "https://api.example.invalid/v1/chat",
                "PUT",
                "openai-chat",
                b"provider-key",
                "grok-4",
                b"{\"m\":1}",
            ),
        ),
        (
            "dialect",
            RequestIdentity::new(
                "https://api.example.invalid/v1/chat",
                "POST",
                "anthropic-messages",
                b"provider-key",
                "grok-4",
                b"{\"m\":1}",
            ),
        ),
        (
            "credential",
            RequestIdentity::new(
                "https://api.example.invalid/v1/chat",
                "POST",
                "openai-chat",
                b"a-different-key",
                "grok-4",
                b"{\"m\":1}",
            ),
        ),
        (
            "model",
            RequestIdentity::new(
                "https://api.example.invalid/v1/chat",
                "POST",
                "openai-chat",
                b"provider-key",
                "grok-4-heavy",
                b"{\"m\":1}",
            ),
        ),
        (
            "body",
            RequestIdentity::new(
                "https://api.example.invalid/v1/chat",
                "POST",
                "openai-chat",
                b"provider-key",
                "grok-4",
                b"{\"m\":2}",
            ),
        ),
    ];

    for (what, altered) in variants {
        let f = fixture();
        let lease = lease_for(&f, &base);
        assert!(
            matches!(
                f.authority.begin_send(&f.auth, lease, &altered),
                Err(AuthorityError::DigestMismatch)
            ),
            "changing the {what} after admission must invalidate the permit"
        );
    }
}

#[test]
fn an_action_cannot_be_applied_after_the_surface_moves() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);

    // The surface moves between minting the lease and spending it.
    f.authority
        .record_observation(&f.auth, f.resource, observation("frame-2"))
        .unwrap();

    assert!(matches!(
        f.authority.begin_send(&f.auth, lease, &req),
        Err(AuthorityError::StaleObservation)
    ));
}

#[test]
fn rotating_the_control_epoch_retires_in_flight_admissions() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    f.authority.rotate_control_epoch().unwrap();

    let fresh = f.authority.authenticate(SECRET).unwrap();
    assert!(matches!(
        f.authority.begin_send(&fresh, lease, &req),
        Err(AuthorityError::AlreadyConsumed) | Err(AuthorityError::StaleControlEpoch)
    ));
}

#[test]
fn ambiguity_after_dispatch_is_uncertain_and_offers_no_retry() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let attempt = permit.attempt();

    let outcome = f
        .authority
        .settle_uncertain(permit, UncertainReason::TransportAfterPossibleWrite);
    assert!(matches!(outcome, SendOutcome::Uncertain { .. }));
    assert!(outcome.may_have_taken_effect());
    assert!(
        !outcome.is_safe_to_resend(),
        "an ambiguous attempt must never be advertised as safe to resend"
    );

    let projection = f.authority.attempt_projection(attempt).unwrap().unwrap();
    assert!(projection.ambiguous);

    // The only exit is an explicit reconciliation against provider truth.
    f.authority.reconcile_attempt(attempt, true).unwrap();
    let projection = f.authority.attempt_projection(attempt).unwrap().unwrap();
    assert!(!projection.ambiguous);
    // Reconciling twice is refused: the attempt is no longer uncertain.
    assert!(f.authority.reconcile_attempt(attempt, true).is_err());
}

#[test]
fn only_a_proven_unwritten_request_is_reported_as_failed() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();

    let outcome = f
        .authority
        .settle_failed_before_write(permit, FailedReason::ConnectRefusedBeforeWrite);
    assert!(matches!(outcome, SendOutcome::Failed { .. }));
    assert!(!outcome.may_have_taken_effect());
    assert!(outcome.is_safe_to_resend());
}

#[test]
fn a_crash_between_dispatch_and_settlement_settles_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let attempt = {
        let authority = HostAuthority::open(&root, OWNER).unwrap();
        authority
            .set_credentials(&[HostCredential::new("primary", SECRET).unwrap()], OWNER)
            .unwrap();
        let auth = authority.authenticate(SECRET).unwrap();
        let session = authority.issue_session(&auth).unwrap();
        let workspace = authority.issue_workspace(&auth, &root).unwrap();
        let resource = authority
            .issue_resource(&auth, session, workspace, observation("frame-1"))
            .unwrap();
        let cap = authority
            .seal_capability(&auth, resource, EffectClass::ProviderSend, 60_000)
            .unwrap();
        let req = request(b"body");
        let lease = authority
            .mint_lease(&auth, &cap, req.digest(), 60_000)
            .unwrap();
        let permit = authority.begin_send(&auth, lease, &req).unwrap();
        let id = permit.attempt();
        // Drop the permit without settling: the process "crashes" here.
        std::mem::forget(permit);
        id
    };

    // A new host incarnation recovers.
    let authority = HostAuthority::open(&root, OWNER).unwrap();
    let recovered = authority.recover_incomplete().unwrap();
    assert_eq!(recovered, vec![attempt]);

    let projection = authority.attempt_projection(attempt).unwrap().unwrap();
    assert!(
        projection.ambiguous,
        "an attempt in flight at crash time must be ambiguous, never retried"
    );

    // Recovery is idempotent and still never retries.
    assert!(authority.recover_incomplete().unwrap().is_empty());
}

#[test]
fn concurrent_spends_of_one_lease_admit_exactly_one() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let authority = Arc::new(HostAuthority::open(&root, OWNER).unwrap());
    authority
        .set_credentials(&[HostCredential::new("primary", SECRET).unwrap()], OWNER)
        .unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let session = authority.issue_session(&auth).unwrap();
    let workspace = authority.issue_workspace(&auth, &root).unwrap();
    let resource = authority
        .issue_resource(&auth, session, workspace, observation("frame-1"))
        .unwrap();
    let cap = authority
        .seal_capability(&auth, resource, EffectClass::ProviderSend, 60_000)
        .unwrap();
    let req = request(b"body");
    let lease = authority
        .mint_lease(&auth, &cap, req.digest(), 60_000)
        .unwrap();

    let admitted = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let authority = Arc::clone(&authority);
        let admitted = Arc::clone(&admitted);
        let auth = auth.clone();
        let lease = lease.clone();
        handles.push(std::thread::spawn(move || {
            let req = request(b"body");
            if let Ok(permit) = authority.begin_send(&auth, lease, &req) {
                admitted.fetch_add(1, Ordering::SeqCst);
                let _ = authority.settle_settled(permit);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        admitted.load(Ordering::SeqCst),
        1,
        "a one-use lease must admit exactly one physical send under concurrency"
    );
}

// ───────────────────────────── Gate 4: typed audit ─────────────────────────────

#[test]
fn intent_is_recorded_before_the_outcome_and_the_chain_verifies() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);

    let records = f.authority.audit_records().unwrap();
    let intent = records
        .iter()
        .position(|r| matches!(r.event, AuditEvent::SendIntent { .. }))
        .expect("a send intent must be recorded");
    let outcome = records
        .iter()
        .position(|r| matches!(r.event, AuditEvent::SendOutcome { .. }))
        .expect("a send outcome must be recorded");
    assert!(
        intent < outcome,
        "intent must be durable before the outcome is written"
    );
    assert!(f.authority.audit_chain_intact().unwrap());

    // Sequence numbers are dense and monotonic.
    for (i, record) in records.iter().enumerate() {
        assert_eq!(record.sequence, i as u64 + 1);
    }
}

#[test]
fn a_truncated_audit_log_is_detected() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);

    let path = f.root.join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    // Drop a record from the middle: the chain must no longer verify.
    lines.remove(1);
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let reopened = HostAuthority::open(&f.root, OWNER).unwrap();
    assert!(
        !reopened.audit_chain_intact().unwrap(),
        "removing a record must break the audit hash chain"
    );
}

// ─────────────────── Projections carry no secrets, content, or paths ───────────────────

#[test]
fn public_projections_are_secret_content_and_path_free() {
    let f = fixture();
    let req = request(b"super-secret-user-content");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let attempt = permit.attempt();

    let rendered = format!(
        "{:?} {:?} {:?} {:?} {:?}",
        f.auth,
        permit,
        permit.binding(),
        f.session,
        f.workspace
    );
    let _ = f.authority.settle_settled(permit);

    let projection = f.authority.attempt_projection(attempt).unwrap().unwrap();
    let audit = serde_json::to_string(&f.authority.audit_records().unwrap()).unwrap();
    let state = std::fs::read_to_string(f.root.join("authority.json")).unwrap();
    let haystacks = [
        rendered,
        format!("{projection:?}"),
        audit,
        // The durable root may hold digests, but never these.
        state,
    ];

    for haystack in &haystacks {
        assert!(
            !haystack.contains("super-secret-user-content"),
            "request body leaked into a projection: {haystack}"
        );
        assert!(
            !haystack.contains(SECRET),
            "bearer secret leaked into a projection"
        );
        assert!(
            !haystack.contains("provider-key"),
            "provider credential leaked into a projection"
        );
        assert!(
            !haystack.contains("api.example.invalid"),
            "provider URL leaked into a projection"
        );
        assert!(
            !haystack.contains(f.root.to_str().unwrap()),
            "a filesystem path leaked into a projection"
        );
    }
}

#[test]
fn a_corrupt_authority_root_refuses_service_rather_than_inventing_authority() {
    let f = fixture();
    // A record written by some other build that lacks a required field.
    std::fs::write(
        f.root.join("authority.json"),
        r#"{"schema_version":1,"owner_id":"account-1"}"#,
    )
    .unwrap();
    let reopened = HostAuthority::open(&f.root, OWNER);
    // Opening cannot repair it, and nothing downstream gets authority.
    match reopened {
        Err(AuthorityError::CorruptState(_)) => {}
        Err(other) => panic!("expected corrupt state, got {other:?}"),
        Ok(authority) => {
            assert!(matches!(
                authority.authenticate(SECRET),
                Err(AuthorityError::CorruptState(_)) | Err(AuthorityError::Unauthenticated)
            ));
        }
    }
}
