//! What happens when the audit log cannot be written.
//!
//! The two cases are deliberately asymmetric, and that asymmetry is the whole
//! point of the ordering in `begin_send` and `settle`:
//!
//! * Before dispatch, a persistence failure must *prevent* the effect.
//! * After dispatch, a persistence failure must not be reported as an ordinary
//!   failure, because the effect may already have happened.
//!
//! The failure is injected by replacing `audit.log` with a directory, which
//! makes an append fail with `EISDIR` for any user, including root.

use std::path::{Path, PathBuf};

use xai_host_authority::*;

const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";

fn admin_credential() -> HostAdminCredential {
    HostAdminCredential::new(ADMIN_SECRET).unwrap()
}
const SECRET: &str = "s3cret-bearer-value";

fn admit_wire(
    authority: &HostAuthority,
    auth: &AuthContext,
    permit: PhysicalSendPermit,
) -> PhysicalSendPermit {
    authority.admit_sending(auth, permit).unwrap()
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

/// Make every subsequent audit append fail.
fn break_audit_log(root: &Path) {
    let path = root.join("audit.log");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(&path).unwrap();
}

struct Ready {
    _dir: tempfile::TempDir,
    root: PathBuf,
    authority: HostAuthority,
    auth: AuthContext,
    _admin: HostAdminAuthority,
    lease: EffectLease,
}

fn ready() -> Ready {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    authority
        .set_credentials(&admin, &[HostCredential::new("primary", SECRET).unwrap()])
        .unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let session = authority.issue_session(&auth).unwrap();
    let workspace = authority.issue_workspace(&auth, &root).unwrap();
    let resource = authority
        .issue_resource(
            &auth,
            session,
            workspace,
            ContentDigest::of_bytes(b"frame-1"),
        )
        .unwrap();
    let capability = authority
        .seal_capability(
            &auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let lease = authority
        .mint_lease(&auth, &capability, request(b"body").digest(), 60_000)
        .unwrap();
    Ready {
        _dir: dir,
        root,
        authority,
        auth,
        _admin: admin,
        lease,
    }
}

#[test]
fn a_pre_effect_persistence_failure_prevents_dispatch() {
    let r = ready();
    break_audit_log(&r.root);

    let req = request(b"body");
    let outcome = r.authority.begin_send(&r.auth, r.lease, &req, "test-route");

    // No permit exists, so the physical send that it would have authorised
    // cannot happen. The whole point: the effect is prevented, not recorded.
    assert!(
        matches!(outcome, Err(AuthorityError::Durability(_))),
        "expected a durability denial, got {outcome:?}"
    );
    assert!(
        outcome.unwrap_err().is_pre_effect(),
        "a denial from begin_send is always pre-effect"
    );
}

#[test]
fn audit_trouble_after_dispatch_never_reports_an_ordinary_failure() {
    let r = ready();
    let req = request(b"body");

    // Admission succeeds and the permit exists: wire admission makes a
    // physical send possible.
    let permit = admit_wire(
        &r.authority,
        &r.auth,
        r.authority
            .begin_send(&r.auth, r.lease, &req, "test-route")
            .unwrap(),
    );
    let attempt = permit.attempt();

    // The audit log breaks while the request is in flight.
    break_audit_log(&r.root);

    // Even though the caller is asserting a clean, proven-unwritten failure,
    // the outcome cannot be an ordinary failure: the audit record that would
    // have proven it is not durable.
    let outcome = r
        .authority
        .settle_failed_before_write(permit, FailedReason::ConnectRefusedBeforeWrite);

    assert!(
        matches!(
            outcome,
            SendOutcome::Uncertain {
                reason: UncertainReason::AuditNotDurableAfterDispatch,
                ..
            }
        ),
        "expected an ambiguous outcome, got {outcome:?}"
    );
    assert!(!outcome.is_safe_to_resend());
    assert!(outcome.may_have_taken_effect());
    assert_eq!(
        match outcome {
            SendOutcome::Uncertain { attempt, .. } => attempt,
            other => panic!("unexpected {other:?}"),
        },
        attempt
    );
}

#[test]
fn a_successful_send_with_a_broken_audit_log_is_also_ambiguous() {
    let r = ready();
    let req = request(b"body");
    let permit = admit_wire(
        &r.authority,
        &r.auth,
        r.authority
            .begin_send(&r.auth, r.lease, &req, "test-route")
            .unwrap(),
    );
    break_audit_log(&r.root);

    let outcome = r.authority.settle_settled(permit);
    assert!(
        matches!(
            outcome,
            SendOutcome::Uncertain {
                reason: UncertainReason::AuditNotDurableAfterDispatch,
                ..
            }
        ),
        "a settlement that cannot be recorded is ambiguous, got {outcome:?}"
    );
}
