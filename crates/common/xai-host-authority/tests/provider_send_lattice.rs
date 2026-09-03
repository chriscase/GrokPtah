//! Gate 3 slice: one physical provider-send lattice with preparing/sending
//! phases, per-resource ordinals, and fail-closed retry semantics.

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

fn host_with_credentials(
    secrets: &[(&str, &str)],
) -> (tempfile::TempDir, HostAuthority, HostAdminAuthority) {
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
    (dir, authority, admin)
}

fn prepared_send(
    authority: &HostAuthority,
    auth: &AuthContext,
    route: &str,
    body: &[u8],
) -> PhysicalSendPermit {
    let session = authority.issue_session(auth).unwrap();
    let workspace = authority
        .issue_workspace(auth, std::path::Path::new("/tmp"))
        .unwrap();
    let resource = authority
        .issue_resource(auth, session, workspace, ContentDigest::of_bytes(b"frame"))
        .unwrap();
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
    authority.begin_send(auth, lease, &req, route).unwrap()
}

#[test]
fn two_credentials_assign_distinct_ordinals_on_the_same_route() {
    let (_dir, authority, _admin) = host_with_credentials(&[("a", SECRET_A), ("b", SECRET_B)]);
    let a = authority.authenticate(SECRET_A).unwrap();
    let b = authority.authenticate(SECRET_B).unwrap();

    let permit_a = prepared_send(&authority, &a, ROUTE, b"first");
    let permit_b = prepared_send(&authority, &b, ROUTE, b"second");
    let attempt_a = permit_a.attempt();
    let attempt_b = permit_b.attempt();
    let _ = authority.settle_settled(authority.admit_sending(permit_a).unwrap());
    let _ = authority.settle_settled(authority.admit_sending(permit_b).unwrap());

    let projection_a = authority
        .attempt_projection(&a, attempt_a)
        .unwrap()
        .unwrap();
    let projection_b = authority
        .attempt_projection(&b, attempt_b)
        .unwrap()
        .unwrap();
    assert_eq!(projection_a.ordinal, 1);
    assert_eq!(projection_b.ordinal, 1);
    assert_ne!(projection_a.attempt, projection_b.attempt);
}

#[test]
fn crash_after_preparing_before_wire_is_not_sent_and_may_retry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let attempt = {
        let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
        authority
            .set_credentials(&admin, &[HostCredential::new("primary", SECRET_A).unwrap()])
            .unwrap();
        let auth = authority.authenticate(SECRET_A).unwrap();
        let permit = prepared_send(&authority, &auth, ROUTE, b"body");
        let id = permit.attempt();
        std::mem::forget(permit);
        id
    };

    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    let auth = authority.authenticate(SECRET_A).unwrap();
    let recovered = authority.recover_incomplete(&admin).unwrap();
    assert_eq!(recovered, vec![attempt]);
    let projection = authority
        .attempt_projection(&auth, attempt)
        .unwrap()
        .unwrap();
    assert_eq!(projection.state, "failed");
    assert!(!projection.ambiguous);

    let retry = prepared_send(&authority, &auth, ROUTE, b"body");
    let outcome = authority.settle_settled(authority.admit_sending(retry).unwrap());
    assert!(matches!(outcome, SendOutcome::Settled { .. }));
}

#[test]
fn crash_after_sending_is_uncertain_and_retry_is_not_safe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let attempt = {
        let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
        authority
            .set_credentials(&admin, &[HostCredential::new("primary", SECRET_A).unwrap()])
            .unwrap();
        let auth = authority.authenticate(SECRET_A).unwrap();
        let permit = authority
            .admit_sending(prepared_send(&authority, &auth, ROUTE, b"body"))
            .unwrap();
        let id = permit.attempt();
        std::mem::forget(permit);
        id
    };

    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    let auth = authority.authenticate(SECRET_A).unwrap();
    authority.recover_incomplete(&admin).unwrap();
    let projection = authority
        .attempt_projection(&auth, attempt)
        .unwrap()
        .unwrap();
    assert!(projection.ambiguous);
    assert!(!projection.settled || projection.state == "uncertain");

    let retry = prepared_send(&authority, &auth, ROUTE, b"body");
    let uncertain = authority.settle_uncertain(
        authority.admit_sending(retry).unwrap(),
        UncertainReason::TransportAfterPossibleWrite,
    );
    assert!(!uncertain.is_safe_to_resend());
}

#[test]
fn pre_journal_failure_prevents_a_permit() {
    let dir = tempfile::tempdir().unwrap();
    let (authority, admin) = HostAuthority::open(dir.path(), &admin_credential()).unwrap();
    authority
        .set_credentials(&admin, &[HostCredential::new("primary", SECRET_A).unwrap()])
        .unwrap();
    let auth = authority.authenticate(SECRET_A).unwrap();
    let session = authority.issue_session(&auth).unwrap();
    let workspace = authority.issue_workspace(&auth, dir.path()).unwrap();
    let resource = authority
        .issue_resource(&auth, session, workspace, ContentDigest::of_bytes(b"frame"))
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
    let req = request(b"body");
    let lease = authority
        .mint_lease(&auth, &capability, req.digest(), 60_000)
        .unwrap();

    let audit = dir.path().join("audit.log");
    std::fs::remove_file(&audit).ok();
    std::fs::create_dir(&audit).unwrap();

    assert!(matches!(
        authority.begin_send(&auth, lease, &req, ROUTE),
        Err(AuthorityError::Durability(_))
    ));
}
