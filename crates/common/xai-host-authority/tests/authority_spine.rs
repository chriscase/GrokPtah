//! Adversarial tests for the canonical authority spine.
//!
//! Each test names the attack it closes rather than the function it calls.

use std::path::PathBuf;

use xai_host_authority::*;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    authority: HostAuthority,
    admin: HostAdminAuthority,
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
    let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
    authority
        .set_credentials(
            &admin,
            &[HostCredential::new("primary", SECRET).unwrap()],
            OWNER,
        )
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
        admin,
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
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
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
            &f.admin,
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
        .set_credentials(
            &f.admin,
            &[HostCredential::new("primary", "interim").unwrap()],
            OWNER,
        )
        .unwrap();
    f.authority
        .set_credentials(
            &f.admin,
            &[HostCredential::new("primary", SECRET).unwrap()],
            OWNER,
        )
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
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();

    // A second slot replaces the first entirely.
    f.authority
        .set_credentials(
            &f.admin,
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
        f.authority.seal_capability(
            &f.auth,
            other.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000
        ),
        Err(AuthorityError::UnknownResource)
    ));
}

#[test]
fn a_second_principal_cannot_use_the_first_principals_resource() {
    let dir = tempfile::tempdir().unwrap();
    let (authority, admin) = HostAuthority::open(dir.path(), OWNER).unwrap();
    authority
        .set_credentials(
            &admin,
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
        authority.seal_capability(
            &b,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000
        ),
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
    let (authority, admin) = HostAuthority::open(dir.path(), OWNER).unwrap();
    authority
        .set_credentials(
            &admin,
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
        .seal_capability(
            &a,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
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
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            1,
        )
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
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    f.authority.rotate_capability_generation(&f.admin).unwrap();

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
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ComputerUseAct,
            60_000,
        )
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
    f.authority.rotate_control_epoch(&f.admin).unwrap();

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

    let projection = f
        .authority
        .attempt_projection(&f.auth, attempt)
        .unwrap()
        .unwrap();
    assert!(projection.ambiguous);

    // The only exit is an explicit reconciliation against provider truth.
    f.authority
        .reconcile_attempt(&f.admin, attempt, true)
        .unwrap();
    let projection = f
        .authority
        .attempt_projection(&f.auth, attempt)
        .unwrap()
        .unwrap();
    assert!(!projection.ambiguous);
    // Reconciling twice is refused: the attempt is no longer uncertain.
    assert!(
        f.authority
            .reconcile_attempt(&f.admin, attempt, true)
            .is_err()
    );
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
        let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
        authority
            .set_credentials(
                &admin,
                &[HostCredential::new("primary", SECRET).unwrap()],
                OWNER,
            )
            .unwrap();
        let auth = authority.authenticate(SECRET).unwrap();
        let session = authority.issue_session(&auth).unwrap();
        let workspace = authority.issue_workspace(&auth, &root).unwrap();
        let resource = authority
            .issue_resource(&auth, session, workspace, observation("frame-1"))
            .unwrap();
        let cap = authority
            .seal_capability(
                &auth,
                resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000,
            )
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
    let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
    let _ = &admin;
    let recovered = authority.recover_incomplete(&admin).unwrap();
    assert_eq!(recovered, vec![attempt]);
    let auth = authority.authenticate(SECRET).unwrap();

    let projection = authority
        .attempt_projection(&auth, attempt)
        .unwrap()
        .unwrap();
    assert!(
        projection.ambiguous,
        "an attempt in flight at crash time must be ambiguous, never retried"
    );

    // Recovery is idempotent and still never retries.
    assert!(authority.recover_incomplete(&admin).unwrap().is_empty());
}

#[test]
fn concurrent_spends_of_one_lease_admit_exactly_one() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
    let authority = Arc::new(authority);
    authority
        .set_credentials(
            &admin,
            &[HostCredential::new("primary", SECRET).unwrap()],
            OWNER,
        )
        .unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let session = authority.issue_session(&auth).unwrap();
    let workspace = authority.issue_workspace(&auth, &root).unwrap();
    let resource = authority
        .issue_resource(&auth, session, workspace, observation("frame-1"))
        .unwrap();
    let cap = authority
        .seal_capability(
            &auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
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

    let records = f.authority.audit_records(&f.admin).unwrap();
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
    assert!(f.authority.audit_chain_intact(&f.admin).unwrap());

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
    let root = f.root.clone();
    drop(f.authority);

    let (reopened, reopened_admin) = HostAuthority::open(&root, OWNER).unwrap();
    assert!(
        !reopened.audit_chain_intact(&reopened_admin).unwrap(),
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

    let projection = f
        .authority
        .attempt_projection(&f.auth, attempt)
        .unwrap()
        .unwrap();
    let audit = serde_json::to_string(&f.authority.audit_records(&f.admin).unwrap()).unwrap();
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
    let root = f.root.clone();
    drop(f.authority);
    let reopened = HostAuthority::open(&root, OWNER);
    // Opening cannot repair it, and nothing downstream gets authority.
    match reopened {
        Err(AuthorityError::CorruptState(_)) => {}
        Err(other) => panic!("expected corrupt state, got {other:?}"),
        Ok((authority, _admin)) => {
            assert!(matches!(
                authority.authenticate(SECRET),
                Err(AuthorityError::CorruptState(_)) | Err(AuthorityError::Unauthenticated)
            ));
        }
    }
}

#[test]
fn a_send_intent_always_names_its_producing_principal_and_generations() {
    // An audit entry whose producer could be absent would let an unattributed
    // intent sit beside attributed ones and read as equivalent. Every field is
    // required, so that state is unrepresentable.
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);

    let records = f.authority.audit_records(&f.admin).unwrap();
    let intent = records
        .iter()
        .find_map(|r| match &r.event {
            AuditEvent::SendIntent {
                principal,
                auth_generation,
                capability_generation,
                session,
                workspace,
                resource,
                ..
            } => Some((
                principal.clone(),
                *auth_generation,
                *capability_generation,
                session.clone(),
                workspace.clone(),
                resource.clone(),
            )),
            _ => None,
        })
        .expect("a send intent must be recorded");

    assert_eq!(intent.0, f.auth.principal().public_handle());
    assert_eq!(intent.1, 1, "the producing authentication generation");
    assert_eq!(intent.2, 1, "the producing capability generation");
    assert_eq!(intent.3, f.session.public_handle());
    assert_eq!(intent.4, f.workspace.public_handle());
    assert_eq!(intent.5, f.resource.public_handle());
}

#[test]
fn there_is_no_unauthenticated_path_that_mutates_or_prunes_the_log() {
    // The audit surface is append-plus-read only: there is no retention,
    // deletion, or compaction entry point at all, authenticated or otherwise,
    // so no operator act can drop evidence without leaving the chain broken.
    let f = fixture();
    let before = f.authority.audit_records(&f.admin).unwrap().len();
    assert!(before > 0);
    // Reading never mutates.
    let _ = f.authority.audit_records(&f.admin).unwrap();
    assert!(f.authority.audit_chain_intact(&f.admin).unwrap());
    assert_eq!(f.authority.audit_records(&f.admin).unwrap().len(), before);
}

#[test]
fn a_refused_send_is_recorded_against_the_principal_that_asked() {
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    // Spend the lease, then replay it.
    let permit = f
        .authority
        .begin_send(&f.auth, lease.clone(), &req)
        .unwrap();
    let _ = f.authority.settle_settled(permit);
    assert!(f.authority.begin_send(&f.auth, lease, &req).is_err());

    let denied = f
        .authority
        .audit_records(&f.admin)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r.event {
            AuditEvent::Denied { principal, reason } => Some((principal, reason)),
            _ => None,
        })
        .next()
        .expect("a refusal must be attributable");
    assert_eq!(denied.0, f.auth.principal().public_handle());
    assert!(denied.1.contains("AlreadyConsumed"), "got {}", denied.1);
}

// ─────────────────── Probes for the reviewed P0 defect classes ───────────────────

#[test]
fn a_planted_old_resource_record_cannot_be_used_after_rotation() {
    // The attack: a record written under a previous credential incarnation
    // survives (restored from a backup, or never pruned) and is presented to
    // the current principal as its own work.
    let f = fixture();
    let planted = {
        let raw = std::fs::read_to_string(f.root.join("authority.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["resources"].clone()
    };
    assert!(planted.as_object().is_some_and(|m| !m.is_empty()));

    // Rotate the secret. The resource is revoked along with its incarnation.
    f.authority
        .set_credentials(
            &f.admin,
            &[HostCredential::new("primary", "rotated-secret").unwrap()],
            OWNER,
        )
        .unwrap();
    let after = f.authority.authenticate("rotated-secret").unwrap();
    assert!(matches!(
        f.authority.resource_binding(&after, f.resource),
        Err(AuthorityError::UnknownResource)
    ));

    // Plant the stale record back into the durable root.
    let raw = std::fs::read_to_string(f.root.join("authority.json")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value["resources"] = planted;
    std::fs::write(
        f.root.join("authority.json"),
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();

    // Release the root first: admin authority is exclusive, so a second live
    // holder is refused outright.
    // Only the store is released; the fixture's other fields stay usable.
    let root = f.root.clone();
    drop(f.authority);

    // It is present again, but bound to an incarnation that no longer exists,
    // so the current principal cannot read or act on it.
    let (reopened, reopened_admin) = HostAuthority::open(&root, OWNER).unwrap();
    let _ = &reopened_admin;
    let current = reopened.authenticate("rotated-secret").unwrap();
    assert!(
        reopened.resource_binding(&current, f.resource).is_err(),
        "a record from a dead incarnation must not become the caller's work"
    );
    assert!(
        reopened
            .seal_capability(
                &current,
                f.resource,
                ActorClass::VerifiedOperator,
                EffectClass::ProviderSend,
                60_000
            )
            .is_err(),
        "nor may it be used to seal a capability"
    );
}

#[test]
fn a_missing_authority_root_with_prior_evidence_refuses_service() {
    // Deleting the root must not mint a fresh lineage: that would orphan every
    // record the old lineage produced and let removed credentials return.
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);
    drop(f.authority);

    std::fs::remove_file(f.root.join("authority.json")).unwrap();
    assert!(f.root.join("audit.log").exists());

    match HostAuthority::open(&f.root, OWNER) {
        Err(AuthorityError::CorruptState(_)) => {}
        Err(other) => panic!("expected a refusal to re-establish, got {other:?}"),
        Ok(_) => panic!("a deleted authority root must not silently mint a new lineage"),
    }
}

#[test]
fn one_principal_cannot_read_another_principals_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let (authority, admin) = HostAuthority::open(dir.path(), OWNER).unwrap();
    authority
        .set_credentials(
            &admin,
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
        .seal_capability(
            &a,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let req = request(b"body");
    let lease = authority
        .mint_lease(&a, &cap, req.digest(), 60_000)
        .unwrap();
    let permit = authority.begin_send(&a, lease, &req).unwrap();
    let attempt = permit.attempt();
    let _ = authority.settle_settled(permit);

    // The owner sees it.
    assert!(authority.attempt_projection(&a, attempt).unwrap().is_some());
    // Another principal sees exactly what it would see for an attempt that
    // does not exist, so this is not an existence oracle.
    assert!(authority.attempt_projection(&b, attempt).unwrap().is_none());
}

#[test]
fn a_model_sealed_capability_is_never_operator_authority() {
    let f = fixture();
    let model = f
        .authority
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedModel,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    assert_eq!(model.actor(), ActorClass::VerifiedModel);
    assert!(
        !model.actor().is_operator(),
        "a model proposal must never read as operator authority"
    );

    // The actor is carried onto the lease and cannot be swapped there.
    let req = request(b"body");
    let lease = f
        .authority
        .mint_lease(&f.auth, &model, req.digest(), 60_000)
        .unwrap();
    assert_eq!(lease.actor(), ActorClass::VerifiedModel);

    // And it reaches the audit trail, so an auditor can tell which grants a
    // human stood behind.
    let sealed_actors: Vec<String> = f
        .authority
        .audit_records(&f.admin)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r.event {
            AuditEvent::CapabilitySealed { actor, .. } => Some(actor),
            _ => None,
        })
        .collect();
    assert!(sealed_actors.contains(&"verified_model".to_string()));

    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);
    let intent_actors: Vec<String> = f
        .authority
        .audit_records(&f.admin)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r.event {
            AuditEvent::SendIntent { actor, .. } => Some(actor),
            _ => None,
        })
        .collect();
    assert_eq!(intent_actors, vec!["verified_model".to_string()]);
}

#[test]
fn an_unrecognised_durable_actor_is_corrupt_state_not_an_implicit_operator() {
    let f = fixture();
    let cap = f
        .authority
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedModel,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let req = request(b"body");
    let lease = f
        .authority
        .mint_lease(&f.auth, &cap, req.digest(), 60_000)
        .unwrap();

    // Rewrite the stored lease's actor to something this build does not know.
    let path = f.root.join("authority.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (_, stored) in value["leases"].as_object_mut().unwrap().iter_mut() {
        stored["actor"] = serde_json::Value::String("some_future_actor".into());
    }
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let error = f
        .authority
        .begin_send(&f.auth, lease, &req)
        .expect_err("an unknown actor must not authorise a send");
    assert!(
        matches!(error, AuthorityError::CorruptState(_)),
        "expected corrupt state, got {error:?}"
    );
}

#[test]
fn a_lease_actor_must_match_the_capability_it_came_from() {
    // Seal as operator, then rewrite the durable capability to claim the model
    // actor: the presented capability and the record must agree in full.
    let f = fixture();
    let cap = f
        .authority
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();

    let path = f.root.join("authority.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (_, stored) in value["capabilities"].as_object_mut().unwrap().iter_mut() {
        stored["actor"] = serde_json::Value::String("verified_model".into());
    }
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    assert!(matches!(
        f.authority
            .mint_lease(&f.auth, &cap, observation("act"), 60_000),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
}

// ───────────────────── Probes for the exact-head audit findings ─────────────────────

#[test]
fn admin_authority_is_scarce() {
    // Opening a root hands out admin, so a second holder would be a second
    // admin over the same state. Any `&HostAuthority` could otherwise mint one
    // simply by opening the root it already knows.
    let f = fixture();
    let error = HostAuthority::open(&f.root, OWNER)
        .expect_err("a second holder must be refused while the first is live");
    assert!(
        matches!(error, AuthorityError::Durability(_)),
        "expected an exclusivity refusal, got {error:?}"
    );

    // Released, the root can be taken again — by exactly one holder.
    let root = f.root.clone();
    drop(f.authority);
    let (reopened, _admin) = HostAuthority::open(&root, OWNER).unwrap();
    assert!(HostAuthority::open(&root, OWNER).is_err());
    drop(reopened);
}

#[test]
fn a_root_cannot_be_opened_under_a_different_owner() {
    // The stored owner is the root's identity; accepting a caller-supplied one
    // would let anybody name themselves the owner of someone else's root.
    let f = fixture();
    let root = f.root.clone();
    drop(f.authority);

    let error =
        HostAuthority::open(&root, "someone-else").expect_err("a different owner must be refused");
    assert!(
        matches!(error, AuthorityError::CorruptState(_)),
        "expected an owner mismatch, got {error:?}"
    );
    assert!(HostAuthority::open(&root, OWNER).is_ok());
}

#[test]
fn one_capability_cannot_authorise_two_sends() {
    // Minting several leases before spending any must not turn one grant into
    // several physical sends.
    let f = fixture();
    let first = request(b"one");
    let second = request(b"two");
    let capability = f
        .authority
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let lease_a = f
        .authority
        .mint_lease(&f.auth, &capability, first.digest(), 60_000)
        .unwrap();
    let lease_b = f
        .authority
        .mint_lease(&f.auth, &capability, second.digest(), 60_000)
        .unwrap();

    let permit = f.authority.begin_send(&f.auth, lease_a, &first).unwrap();
    let _ = f.authority.settle_settled(permit);

    assert!(
        matches!(
            f.authority.begin_send(&f.auth, lease_b, &second),
            Err(AuthorityError::AlreadyConsumed)
        ),
        "the capability was spent by the first send"
    );
}

#[test]
fn a_lease_cannot_outlive_its_capability() {
    let f = fixture();
    let req = request(b"body");
    let capability = f
        .authority
        .seal_capability(
            &f.auth,
            f.resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            30,
        )
        .unwrap();
    // Ask for a lease that would outlast the grant it came from.
    let lease = f
        .authority
        .mint_lease(&f.auth, &capability, req.digest(), 600_000)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    assert!(
        matches!(
            f.authority.begin_send(&f.auth, lease, &req),
            Err(AuthorityError::Expired)
        ),
        "a lease must expire with its capability, not outlast it"
    );
}

#[test]
fn an_uncertain_outcome_is_always_reconcilable() {
    // A settlement whose write fails reports Uncertain. If the durable record
    // said "settled", reconciliation would refuse the very attempt the caller
    // was told to reconcile.
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let attempt = permit.attempt();

    // Break the audit log after admission, then settle.
    let log = f.root.join("audit.log");
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir(&log).unwrap();

    let outcome = f.authority.settle_settled(permit);
    assert!(matches!(outcome, SendOutcome::Uncertain { .. }));

    // Repair the log and reconcile the attempt the caller was handed.
    std::fs::remove_dir(&log).unwrap();
    f.authority
        .reconcile_attempt(&f.admin, attempt, true)
        .expect("the attempt the caller was told to reconcile must be reconcilable");
}

#[test]
fn a_damaged_audit_log_is_never_resealed() {
    // Dropped or torn evidence must not read as an intact chain, and must not
    // let a later append reuse the missing record's sequence number.
    let f = fixture();
    let req = request(b"body");
    let lease = lease_for(&f, &req);
    let permit = f.authority.begin_send(&f.auth, lease, &req).unwrap();
    let _ = f.authority.settle_settled(permit);

    let path = f.root.join("audit.log");
    let text = std::fs::read_to_string(&path).unwrap();
    // Trailing damage, exactly what a crash mid-append leaves behind.
    std::fs::write(&path, format!("{text}{{\"sequence\":")).unwrap();

    let root = f.root.clone();
    drop(f.authority);
    let (reopened, admin) = HostAuthority::open(&root, OWNER).unwrap();

    assert!(
        !reopened.audit_chain_intact(&admin).unwrap(),
        "a log with unparsable trailing content is not an intact chain"
    );
    assert!(
        reopened.audit_records(&admin).is_err(),
        "reading must not present the parsable prefix as the whole log"
    );
    // And nothing may be appended over the damage. Authentication itself
    // records an entry, so even that fails closed rather than writing over the
    // gap and resealing the chain around it.
    assert!(
        matches!(
            reopened.authenticate(SECRET),
            Err(AuthorityError::CorruptState(_))
        ),
        "a damaged log must not accept new records over the gap"
    );
}
