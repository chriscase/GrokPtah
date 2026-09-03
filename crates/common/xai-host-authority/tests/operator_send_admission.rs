//! Caller-seam tests for operator `admit_operator_send`.
//!
//! These use a local [`HostAuthority`] with no HTTP: a refused admit is
//! proof that no provider bytes can move. Sampler/embedding crates add
//! fake-transport coverage on top of this seam.

use xai_host_authority::*;

const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";
const OPERATOR: &str = "operator-secret-32-bytes-minimum!!";
const OTHER: &str = "other-operator-secret-32-bytes-min!!";
const ROUTE: &str = "sampler-chat-completions";

fn request(body: &[u8]) -> RequestIdentity {
    RequestIdentity::new(
        "http://127.0.0.1/v1/chat/completions",
        "POST",
        "sampler_chat_completions",
        b"Bearer test-key",
        "test-model",
        body,
    )
}

#[test]
fn provider_request_id_separates_physical_attempts_without_changing_body_digest() {
    let first = RequestIdentity::new_with_provider_request_id(
        "http://127.0.0.1/v1/responses",
        "POST",
        "sampler_responses",
        b"Bearer test-key",
        "test-model",
        "attempt-1",
        b"same-body",
    );
    let second = RequestIdentity::new_with_provider_request_id(
        "http://127.0.0.1/v1/responses",
        "POST",
        "sampler_responses",
        b"Bearer test-key",
        "test-model",
        "attempt-2",
        b"same-body",
    );
    assert_ne!(first.digest(), second.digest());
    assert_eq!(first.body_digest(), second.body_digest());
}

fn host() -> (tempfile::TempDir, HostAuthority, HostAdminAuthority) {
    let dir = tempfile::tempdir().unwrap();
    let (authority, admin) =
        HostAuthority::open(dir.path(), &HostAdminCredential::new(ADMIN_SECRET).unwrap()).unwrap();
    authority
        .set_credentials(
            &admin,
            &[
                HostCredential::new("operator", OPERATOR).unwrap(),
                HostCredential::new("other", OTHER).unwrap(),
            ],
        )
        .unwrap();
    (dir, authority, admin)
}

#[test]
fn admit_operator_send_transitions_to_sending_before_any_caller_dispatch() {
    let (_dir, authority, _admin) = host();
    let auth = authority.authenticate(OPERATOR).unwrap();
    let permit = authority
        .admit_operator_send(&auth, &request(b"{\"model\":\"test-model\"}"), ROUTE)
        .unwrap();
    assert!(permit.wire_admitted());
    let projection = authority
        .attempt_projection(&auth, permit.attempt())
        .unwrap()
        .unwrap();
    assert_eq!(projection.state, "sending");
    let _ = authority.settle_settled(permit);
}

#[test]
fn duplicate_digest_while_sending_is_ambiguous_and_is_not_admitted() {
    let (_dir, authority, _admin) = host();
    let auth = authority.authenticate(OPERATOR).unwrap();
    let first = authority
        .admit_operator_send(&auth, &request(b"same-body"), ROUTE)
        .unwrap();
    let second = authority.admit_operator_send(&auth, &request(b"same-body"), ROUTE);
    assert!(matches!(second, Err(AuthorityError::AmbiguousPriorAttempt)));
    let _ = authority.settle_uncertain(first, UncertainReason::TransportAfterPossibleWrite);
    assert!(matches!(
        authority.admit_operator_send(&auth, &request(b"same-body"), ROUTE),
        Err(AuthorityError::AmbiguousPriorAttempt)
    ));
}

#[test]
fn possible_write_uncertain_outcome_is_not_safe_to_resend() {
    let (_dir, authority, _admin) = host();
    let auth = authority.authenticate(OPERATOR).unwrap();
    let permit = authority
        .admit_operator_send(&auth, &request(b"possible-write"), ROUTE)
        .unwrap();
    let outcome = authority.settle_uncertain(permit, UncertainReason::TransportAfterPossibleWrite);
    assert!(!outcome.is_safe_to_resend());
    assert!(outcome.may_have_taken_effect());
}

#[test]
fn wrong_principal_cannot_admit_another_principal_s_prepared_send() {
    let (_dir, authority, _admin) = host();
    let operator = authority.authenticate(OPERATOR).unwrap();
    let other = authority.authenticate(OTHER).unwrap();
    let workspace = authority
        .issue_workspace(&operator, std::path::Path::new("/tmp"))
        .unwrap();
    let resource = authority
        .obtain_provider_send_surface(&operator, workspace, ROUTE)
        .unwrap();
    let capability = authority
        .seal_capability(
            &operator,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap();
    let req = request(b"body");
    let lease = authority
        .mint_lease(&operator, &capability, req.digest(), 30_000)
        .unwrap();
    let permit = authority.begin_send(&operator, lease, &req, ROUTE).unwrap();
    assert!(matches!(
        authority.admit_sending(&other, permit),
        Err(AuthorityError::ResourceOwnershipMismatch)
    ));
}

#[test]
fn stale_capability_generation_fails_closed_before_dispatch() {
    let (_dir, authority, admin) = host();
    let auth = authority.authenticate(OPERATOR).unwrap();
    let workspace = authority
        .issue_workspace(&auth, std::path::Path::new("/tmp"))
        .unwrap();
    let resource = authority
        .obtain_provider_send_surface(&auth, workspace, ROUTE)
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
    let req = request(b"stale-generation");
    let lease = authority
        .mint_lease(&auth, &capability, req.digest(), 30_000)
        .unwrap();
    let permit = authority.begin_send(&auth, lease, &req, ROUTE).unwrap();
    authority.rotate_capability_generation(&admin).unwrap();
    let auth = authority.authenticate(OPERATOR).unwrap();
    assert!(matches!(
        authority.admit_sending(&auth, permit),
        Err(AuthorityError::StaleCapability)
    ));
}

#[test]
fn unauthenticated_bearer_is_denied_before_begin_send() {
    let (_dir, authority, _admin) = host();
    assert!(matches!(
        authority.authenticate("not-a-live-operator-secret"),
        Err(AuthorityError::Unauthenticated)
    ));
}

#[test]
fn cancellation_after_admission_is_uncertain_and_cannot_auto_retry() {
    let (_dir, authority, _admin) = host();
    let auth = authority.authenticate(OPERATOR).unwrap();
    let permit = authority
        .admit_operator_send(&auth, &request(b"cancelled"), ROUTE)
        .unwrap();
    let outcome = authority.settle_uncertain(permit, UncertainReason::CancelledAfterPossibleWrite);
    assert!(!outcome.is_safe_to_resend());
    assert!(matches!(
        authority.admit_operator_send(&auth, &request(b"cancelled"), ROUTE),
        Err(AuthorityError::AmbiguousPriorAttempt)
    ));
}
