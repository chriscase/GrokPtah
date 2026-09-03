//! Operator reconciliation of Uncertain attempts and scoped redacted reads.
//!
//! These tests never perform provider I/O. The lattice under test has no HTTP
//! client, no credentials for a real provider, and no resend API.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use xai_host_authority::*;

const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";
const SECRET_A: &str = "secret-a-value-32-bytes-minimum!!";
const SECRET_B: &str = "secret-b-value-32-bytes-minimum!!";
const ROUTE: &str = "agent-step";

fn admin_credential() -> HostAdminCredential {
    HostAdminCredential::new(ADMIN_SECRET).unwrap()
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

struct Host {
    _dir: tempfile::TempDir,
    authority: HostAuthority,
    admin: HostAdminAuthority,
}

fn open_host(secrets: &[(&str, &str)]) -> Host {
    let dir = tempfile::tempdir().unwrap();
    let (authority, admin) = HostAuthority::open(dir.path(), &admin_credential()).unwrap();
    authority
        .set_credentials(
            &admin,
            &secrets
                .iter()
                .map(|(id, secret)| HostCredential::new(*id, *secret).unwrap())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    Host {
        _dir: dir,
        authority,
        admin,
    }
}

fn reopen_root(root: &Path) -> (HostAuthority, HostAdminAuthority) {
    HostAuthority::open(root, &admin_credential()).unwrap()
}

fn prepared(
    authority: &HostAuthority,
    auth: &AuthContext,
    route: &str,
    body: &[u8],
    workspace_path: &Path,
) -> (SessionId, WorkspaceId, PhysicalSendPermit) {
    let workspace = authority.issue_workspace(auth, workspace_path).unwrap();
    let resource = authority
        .obtain_provider_send_surface(auth, workspace, route)
        .unwrap();
    let binding = authority.resource_binding(auth, resource).unwrap();
    let capability = authority
        .seal_capability(
            auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let req = request(body);
    let lease = authority
        .mint_lease(auth, &capability, req.digest(), 60_000)
        .unwrap();
    let permit = authority.begin_send(auth, lease, &req, route).unwrap();
    (binding.session(), binding.workspace(), permit)
}

fn assert_redacted(projection: &AttemptProjection) {
    let blob = format!("{projection:?}");
    assert!(!blob.contains("https://"), "{blob}");
    assert!(!blob.contains("provider-key"), "{blob}");
    assert!(!blob.contains("/tmp"), "{blob}");
    assert!(!blob.contains(ROUTE), "{blob}");
    assert!(!blob.contains("grokptah-"), "{blob}");
    assert!(!blob.contains("api.example"), "{blob}");
}

#[test]
fn scoped_list_and_get_collapse_foreign_and_unknown() {
    let host = open_host(&[("a", SECRET_A), ("b", SECRET_B)]);
    let a = host.authority.authenticate(SECRET_A).unwrap();
    let b = host.authority.authenticate(SECRET_B).unwrap();
    let (session_a, workspace_a, permit_a) = prepared(
        &host.authority,
        &a,
        ROUTE,
        b"one",
        Path::new("/tmp/gp-ws-a"),
    );
    let attempt_a = permit_a.attempt();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&a, permit_a).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let handle = attempt_a.public_handle();

    let listed_a = host
        .authority
        .list_scoped_attempt_projections(&a, session_a, workspace_a)
        .unwrap();
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].attempt, handle);
    assert_redacted(&listed_a[0]);

    let got_a = host
        .authority
        .scoped_attempt_projection(&a, session_a, workspace_a, &handle)
        .unwrap();
    assert!(got_a.is_some());

    let (session_b, workspace_b, permit_b) = prepared(
        &host.authority,
        &b,
        ROUTE,
        b"two",
        Path::new("/tmp/gp-ws-b"),
    );
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&b, permit_b).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let listed_b = host
        .authority
        .list_scoped_attempt_projections(&b, session_b, workspace_b)
        .unwrap();
    assert_eq!(listed_b.len(), 1);
    assert_ne!(listed_b[0].attempt, handle);

    let foreign = host
        .authority
        .scoped_attempt_projection(&b, session_b, workspace_b, &handle)
        .unwrap();
    let unknown = host
        .authority
        .scoped_attempt_projection(&b, session_b, workspace_b, "att_deadbeefdeadbeef")
        .unwrap();
    assert_eq!(foreign, unknown);
    assert!(foreign.is_none());

    let cross_session = host
        .authority
        .scoped_attempt_projection(&a, session_b, workspace_a, &handle)
        .unwrap();
    let cross_workspace = host
        .authority
        .scoped_attempt_projection(&a, session_a, workspace_b, &handle)
        .unwrap();
    assert_eq!(cross_session, unknown);
    assert_eq!(cross_workspace, unknown);

    let foreign_err = host
        .authority
        .mint_reconciliation_grant(
            &b,
            session_b,
            workspace_b,
            &handle,
            ReconciliationDisposition::Review,
            60_000,
        )
        .err();
    let unknown_err = host
        .authority
        .mint_reconciliation_grant(
            &b,
            session_b,
            workspace_b,
            "att_deadbeefdeadbeef",
            ReconciliationDisposition::Review,
            60_000,
        )
        .err();
    assert_eq!(foreign_err, unknown_err);
    assert_eq!(foreign_err, Some(AuthorityError::UnknownResource));
}

#[test]
fn mark_not_sent_requires_pre_wire_evidence() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"pre-wire",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    std::mem::forget(permit);

    let grant = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkNotSent,
            60_000,
        )
        .unwrap();
    let projection = host
        .authority
        .apply_reconciliation(&auth, grant, ReconciliationEvidence::default())
        .unwrap();
    assert_eq!(projection.state, "failed");
    assert!(!projection.ambiguous);
    assert_redacted(&projection);

    let (session2, workspace2, permit2) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"after-wire",
        Path::new("/tmp/gp-ws"),
    );
    let handle2 = permit2.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit2).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let denied = host.authority.mint_reconciliation_grant(
        &auth,
        session2,
        workspace2,
        &handle2,
        ReconciliationDisposition::MarkNotSent,
        60_000,
    );
    assert_eq!(
        denied.unwrap_err(),
        AuthorityError::Invalid("mark-not-sent requires host-proven pre-wire evidence")
    );
}

#[test]
fn mark_settled_rejects_timestamp_only_evidence() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"settle",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let grant = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let denied = host.authority.apply_reconciliation(
        &auth,
        grant,
        ReconciliationEvidence::observed_at_only(1),
    );
    assert_eq!(
        denied.unwrap_err(),
        AuthorityError::Invalid("mark-settled requires provider receipt or operator observation")
    );

    let grant = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let projection = host
        .authority
        .apply_reconciliation(
            &auth,
            grant,
            ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"prov-rcpt")),
        )
        .unwrap();
    assert_eq!(projection.state, "settled");
    assert!(!projection.ambiguous);

    let listed = host
        .authority
        .list_scoped_attempt_projections(&auth, session, workspace)
        .unwrap();
    let ordinal = listed
        .iter()
        .find(|row| row.attempt == handle)
        .unwrap()
        .ordinal;
    let grant = host.authority.mint_reconciliation_grant(
        &auth,
        session,
        workspace,
        &handle,
        ReconciliationDisposition::MarkSettled,
        60_000,
    );
    // Already settled: remint is allowed for idempotent replay of the same
    // decision, but the ordinal does not reopen.
    let _ = grant.unwrap();
    let after = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(after.state, "settled");
    assert_eq!(after.ordinal, ordinal);
}

#[test]
fn discard_is_explicit_and_does_not_resend() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"discard",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let grant = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::Discard,
            60_000,
        )
        .unwrap();
    let projection = host
        .authority
        .apply_reconciliation(&auth, grant, ReconciliationEvidence::default())
        .unwrap();
    assert_eq!(projection.state, "discarded");
    assert!(projection.settled);
    assert!(!projection.ambiguous);

    let conflict = host.authority.mint_reconciliation_grant(
        &auth,
        session,
        workspace,
        &handle,
        ReconciliationDisposition::MarkSettled,
        60_000,
    );
    assert_eq!(
        conflict.unwrap_err(),
        AuthorityError::Invalid("attempt is already settled")
    );
}

#[test]
fn review_does_not_mutate_and_grant_expires() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"review",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let before = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    let grant = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::Review,
            60_000,
        )
        .unwrap();
    let after = host
        .authority
        .apply_reconciliation(&auth, grant, ReconciliationEvidence::default())
        .unwrap();
    assert_eq!(before.state, after.state);
    assert_eq!(before.revision, after.revision);
    assert_eq!(before.ordinal, after.ordinal);

    let expired = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::Review,
            100,
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let err = host
        .authority
        .apply_reconciliation(&auth, expired, ReconciliationEvidence::default())
        .unwrap_err();
    assert_eq!(err, AuthorityError::Expired);
    let still = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(still.state, "uncertain");
}

#[test]
fn stale_revision_and_duplicate_decisions() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"cas",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let first = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let stale = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::Discard,
            60_000,
        )
        .unwrap();
    host.authority
        .apply_reconciliation(
            &auth,
            first,
            ReconciliationEvidence::operator_observation(ContentDigest::of_bytes(b"obs")),
        )
        .unwrap();
    let err = host
        .authority
        .apply_reconciliation(&auth, stale, ReconciliationEvidence::default())
        .unwrap_err();
    assert_eq!(err, AuthorityError::Invalid("stale revision"));
    let current = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(current.state, "settled");

    let replay = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let again = host
        .authority
        .apply_reconciliation(
            &auth,
            replay,
            ReconciliationEvidence::operator_observation(ContentDigest::of_bytes(b"obs")),
        )
        .unwrap();
    assert_eq!(again.state, "settled");
}

#[test]
fn concurrent_operators_serialize_on_revision() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"race",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let left = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let right = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();

    std::thread::scope(|scope| {
        let authority = &host.authority;
        let (tx, rx) = mpsc::channel();
        scope.spawn({
            let tx = tx.clone();
            let auth = auth.clone();
            move || {
                tx.send(authority.apply_reconciliation(
                    &auth,
                    left,
                    ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
                ))
                .unwrap();
            }
        });
        scope.spawn({
            let auth = auth.clone();
            move || {
                tx.send(authority.apply_reconciliation(
                    &auth,
                    right,
                    ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
                ))
                .unwrap();
            }
        });
        let first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert!(first.is_ok());
        assert!(second.is_ok());
    });
    let projection = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(projection.state, "settled");
}

#[test]
fn restart_recovers_reconciled_truth_and_sending_stays_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let handle;
    let session;
    let workspace;
    {
        let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
        authority
            .set_credentials(&admin, &[HostCredential::new("a", SECRET_A).unwrap()])
            .unwrap();
        let auth = authority.authenticate(SECRET_A).unwrap();
        let prepared_send = prepared(
            &authority,
            &auth,
            ROUTE,
            b"restart",
            Path::new("/tmp/gp-ws"),
        );
        session = prepared_send.0;
        workspace = prepared_send.1;
        let permit = prepared_send.2;
        handle = permit.attempt().public_handle();
        let _ = authority.settle_uncertain(
            authority.admit_sending(&auth, permit).unwrap(),
            UncertainReason::TransportAfterPossibleWrite,
        );
        let grant = authority
            .mint_reconciliation_grant(
                &auth,
                session,
                workspace,
                &handle,
                ReconciliationDisposition::Discard,
                60_000,
            )
            .unwrap();
        authority
            .apply_reconciliation(&auth, grant, ReconciliationEvidence::default())
            .unwrap();
    }

    let (authority, admin) = reopen_root(&root);
    let auth = authority.authenticate(SECRET_A).unwrap();
    authority.recover_incomplete(&admin).unwrap();
    let projection = authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(projection.state, "discarded");

    let prepared_send = prepared(
        &authority,
        &auth,
        ROUTE,
        b"still-sending",
        Path::new("/tmp/gp-ws"),
    );
    let sending_session = prepared_send.0;
    let sending_workspace = prepared_send.1;
    let permit = prepared_send.2;
    let sending_handle = permit.attempt().public_handle();
    let admitted = authority.admit_sending(&auth, permit).unwrap();
    std::mem::forget(admitted);
    drop(authority);
    drop(admin);

    let (authority, admin) = reopen_root(&root);
    let auth = authority.authenticate(SECRET_A).unwrap();
    authority.recover_incomplete(&admin).unwrap();
    let sending = authority
        .scoped_attempt_projection(&auth, sending_session, sending_workspace, &sending_handle)
        .unwrap()
        .unwrap();
    assert_eq!(sending.state, "uncertain");
    assert!(sending.ambiguous);
}

#[test]
fn crash_cut_before_audit_leaves_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    authority
        .set_credentials(&admin, &[HostCredential::new("a", SECRET_A).unwrap()])
        .unwrap();
    let auth = authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) =
        prepared(&authority, &auth, ROUTE, b"cut", Path::new("/tmp/gp-ws"));
    let handle = permit.attempt().public_handle();
    let _ = authority.settle_uncertain(
        authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let grant = authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();
    let audit = root.join("audit.log");
    let audit_bytes = std::fs::read(&audit).unwrap();
    std::fs::remove_file(&audit).unwrap();
    std::fs::create_dir(&audit).unwrap();
    let err = authority
        .apply_reconciliation(
            &auth,
            grant,
            ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
        )
        .unwrap_err();
    assert!(matches!(err, AuthorityError::Durability(_)));
    drop(authority);
    drop(admin);
    std::fs::remove_dir(&audit).unwrap();
    std::fs::write(&audit, audit_bytes).unwrap();
    let (authority, admin) = reopen_root(&root);
    let auth = authority.authenticate(SECRET_A).unwrap();
    authority.recover_incomplete(&admin).unwrap();
    let projection = authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_ne!(projection.state, "settled");
}

#[test]
fn sending_stays_uncertain_until_explicit_reconciliation() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"wire",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let admitted = host.authority.admit_sending(&auth, permit).unwrap();
    std::mem::forget(admitted);
    let listed = host
        .authority
        .list_scoped_attempt_projections(&auth, session, workspace)
        .unwrap();
    assert_eq!(listed[0].state, "sending");
    assert!(!listed[0].ambiguous);
    let recovered = host.authority.recover_incomplete(&host.admin).unwrap();
    assert_eq!(recovered.len(), 1);
    let after = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(after.state, "uncertain");
    assert!(after.ambiguous);
}

#[test]
fn legacy_reconcile_attempt_fails_closed_with_migration_seam() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"legacy",
        Path::new("/tmp/gp-ws"),
    );
    let attempt = permit.attempt();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let before = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &attempt.public_handle())
        .unwrap()
        .unwrap();
    let audit_before = std::fs::read(host._dir.path().join("audit.log")).unwrap();

    #[allow(deprecated)]
    let err = host
        .authority
        .reconcile_attempt(&host.admin, attempt, true)
        .unwrap_err();
    assert_eq!(
        err,
        AuthorityError::Invalid(
            "legacy reconcile_attempt is retired; use mint_reconciliation_grant and apply_reconciliation"
        )
    );

    let after = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &attempt.public_handle())
        .unwrap()
        .unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(
        std::fs::read(host._dir.path().join("audit.log")).unwrap(),
        audit_before
    );
}

#[test]
fn stale_auth_generation_rejects_before_audit_or_lease() {
    let host = open_host(&[("a", SECRET_A)]);
    host.authority
        .set_credentials(&host.admin, &[HostCredential::new("a", SECRET_A).unwrap()])
        .unwrap();
    let old = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &old,
        ROUTE,
        b"rotate",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&old, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    let grant = host
        .authority
        .mint_reconciliation_grant(
            &old,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();

    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("a", "rotated-secret-value-32b!!").unwrap()],
        )
        .unwrap();

    assert!(matches!(
        host.authority.apply_reconciliation(
            &old,
            grant,
            ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
        ),
        Err(AuthorityError::StalePrincipal)
    ));

    let fresh = host
        .authority
        .authenticate("rotated-secret-value-32b!!")
        .unwrap();
    assert!(matches!(
        host.authority.mint_reconciliation_grant(
            &fresh,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        ),
        Err(AuthorityError::StalePrincipal)
    ));

    let projection = host
        .authority
        .scoped_attempt_projection(&fresh, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(projection.state, "uncertain");
    let records = host.authority.audit_records(&host.admin).unwrap();
    assert!(
        !records.iter().any(|record| {
            matches!(
                record.event,
                AuditEvent::AttemptReconciled { .. } | AuditEvent::AttemptReviewed { .. }
            )
        }),
        "stale grants must not append reconciliation audit events"
    );
}

#[test]
fn conflicting_concurrency_and_restart_regression() {
    let host = open_host(&[("a", SECRET_A)]);
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let (session, workspace, permit) = prepared(
        &host.authority,
        &auth,
        ROUTE,
        b"conflict",
        Path::new("/tmp/gp-ws"),
    );
    let handle = permit.attempt().public_handle();
    let _ = host.authority.settle_uncertain(
        host.authority.admit_sending(&auth, permit).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );

    let discard = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::Discard,
            60_000,
        )
        .unwrap();
    let settled = host
        .authority
        .mint_reconciliation_grant(
            &auth,
            session,
            workspace,
            &handle,
            ReconciliationDisposition::MarkSettled,
            60_000,
        )
        .unwrap();

    std::thread::scope(|scope| {
        let authority = &host.authority;
        let (tx, rx) = mpsc::channel();
        scope.spawn({
            let tx = tx.clone();
            let auth = auth.clone();
            move || {
                tx.send(authority.apply_reconciliation(
                    &auth,
                    discard,
                    ReconciliationEvidence::default(),
                ))
                .unwrap();
            }
        });
        scope.spawn({
            let auth = auth.clone();
            move || {
                tx.send(authority.apply_reconciliation(
                    &auth,
                    settled,
                    ReconciliationEvidence::provider_receipt(ContentDigest::of_bytes(b"rcpt")),
                ))
                .unwrap();
            }
        });
        let first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert_eq!(first.is_ok(), second.is_err() || second.is_ok());
        let outcomes = [first, second];
        let successes = outcomes.iter().filter(|result| result.is_ok()).count();
        let failures = outcomes.iter().filter(|result| result.is_err()).count();
        assert_eq!(successes, 1, "exactly one conflicting disposition may win");
        assert_eq!(failures, 1, "the loser must fail closed");
    });

    let projection = host
        .authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert!(
        projection.state == "discarded" || projection.state == "settled",
        "state must reflect the winning disposition only"
    );

    let root = host._dir.path().to_path_buf();
    drop(host.authority);
    drop(host.admin);
    let (authority, admin) = reopen_root(&root);
    let auth = authority.authenticate(SECRET_A).unwrap();
    authority.recover_incomplete(&admin).unwrap();
    let after_restart = authority
        .scoped_attempt_projection(&auth, session, workspace, &handle)
        .unwrap()
        .unwrap();
    assert_eq!(after_restart.state, projection.state);
    assert!(authority.audit_chain_intact(&admin).unwrap());
}
