//! Gate 1 slice: host-issued principal and authentication-generation spine.
//!
//! These tests name the #477 first-slice acceptance criteria rather than the
//! helper functions they exercise.

use std::path::PathBuf;

use xai_host_authority::*;

const SECRET_A: &str = "secret-a-value-32-bytes-minimum!!";
const SECRET_B: &str = "secret-b-value-32-bytes-minimum!!";
const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";

fn admin_credential() -> HostAdminCredential {
    HostAdminCredential::new(ADMIN_SECRET).unwrap()
}

struct HostFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    authority: HostAuthority,
    admin: HostAdminAuthority,
}

fn open_host() -> HostFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    HostFixture {
        _dir: dir,
        root,
        authority,
        admin,
    }
}

fn install_two_credentials(host: &HostFixture) {
    host.authority
        .set_credentials(
            &host.admin,
            &[
                HostCredential::new("a", SECRET_A).unwrap(),
                HostCredential::new("b", SECRET_B).unwrap(),
            ],
        )
        .unwrap();
}

#[test]
fn two_credentials_mint_two_distinct_principals() {
    let host = open_host();
    install_two_credentials(&host);
    let a = host.authority.authenticate(SECRET_A).unwrap();
    let b = host.authority.authenticate(SECRET_B).unwrap();
    assert_ne!(a.principal(), b.principal());
    assert_ne!(a.credential_id(), b.credential_id());

    let proj_a = host.authority.principal_projection(&a).unwrap();
    let proj_b = host.authority.principal_projection(&b).unwrap();
    assert_ne!(proj_a.principal, proj_b.principal);
    assert_eq!(proj_a.credential_id, "a");
    assert_eq!(proj_b.credential_id, "b");
}

#[test]
fn a_second_principal_cannot_project_or_bind_the_first_principals_workspace() {
    let host = open_host();
    install_two_credentials(&host);
    let a = host.authority.authenticate(SECRET_A).unwrap();
    let b = host.authority.authenticate(SECRET_B).unwrap();
    let session = host.authority.issue_session(&a).unwrap();
    let workspace = host
        .authority
        .issue_workspace(&a, host.root.as_path())
        .unwrap();
    let resource = host
        .authority
        .issue_resource(&a, session, workspace, ContentDigest::of_bytes(b"frame"))
        .unwrap();

    assert!(host.authority.principal_projection(&b).is_ok());
    assert!(matches!(
        host.authority.resource_binding(&b, resource),
        Err(AuthorityError::UnknownResource)
    ));
}

#[test]
fn restart_reopens_durable_generations_and_rejects_stale_contexts() {
    let host = open_host();
    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let before = host.authority.authenticate(SECRET_A).unwrap();
    let session = host.authority.issue_session(&before).unwrap();
    let workspace = host
        .authority
        .issue_workspace(&before, host.root.as_path())
        .unwrap();
    let resource = host
        .authority
        .issue_resource(
            &before,
            session,
            workspace,
            ContentDigest::of_bytes(b"frame"),
        )
        .unwrap();
    let root = host.root.clone();
    drop(host.authority);

    let (reopened, _admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    assert!(matches!(
        reopened.require_current(&before),
        Err(AuthorityError::StaleControlEpoch)
    ));
    let after = reopened.authenticate(SECRET_A).unwrap();
    assert!(reopened.require_current(&after).is_ok());
    // Resources issued under the previous control epoch are not resurrected.
    assert!(matches!(
        reopened.resource_binding(&after, resource),
        Err(AuthorityError::UnknownResource)
    ));

    let probe = InternalServiceAuthority::open_probe(&root).unwrap();
    let liveness = probe.liveness_projection().unwrap();
    assert!(liveness.credentials_configured);
    assert_eq!(liveness.schema_version, 2);
    assert!(liveness.policy_revision >= 1);
}

#[test]
fn credential_rotation_advances_auth_generation_and_fails_closed() {
    let host = open_host();
    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let old = host.authority.authenticate(SECRET_A).unwrap();
    let old_projection = host.authority.principal_projection(&old).unwrap();

    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", "rotated-secret-value-32b!!").unwrap()],
        )
        .unwrap();

    assert!(matches!(
        host.authority.authenticate(SECRET_A),
        Err(AuthorityError::Unauthenticated)
    ));
    assert!(matches!(
        host.authority.require_current(&old),
        Err(AuthorityError::StalePrincipal)
    ));
    assert!(matches!(
        host.authority.principal_projection(&old),
        Err(AuthorityError::StalePrincipal)
    ));

    let fresh = host
        .authority
        .authenticate("rotated-secret-value-32b!!")
        .unwrap();
    let fresh_projection = host.authority.principal_projection(&fresh).unwrap();
    assert_ne!(
        old_projection.auth_generation,
        fresh_projection.auth_generation
    );
    assert_eq!(old_projection.principal, fresh_projection.principal);
}

#[test]
fn policy_revision_rotation_invalidates_prior_contexts_without_touching_auth_generation() {
    let host = open_host();
    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let before = host.authority.authenticate(SECRET_A).unwrap();
    let before_projection = host.authority.principal_projection(&before).unwrap();
    let auth_generation = before_projection.auth_generation;

    host.authority.rotate_policy_revision(&host.admin).unwrap();

    assert!(matches!(
        host.authority.require_current(&before),
        Err(AuthorityError::StalePolicy)
    ));
    let after = host.authority.authenticate(SECRET_A).unwrap();
    let after_projection = host.authority.principal_projection(&after).unwrap();
    assert_eq!(after_projection.auth_generation, auth_generation);
    assert!(after_projection.policy_revision > before_projection.policy_revision);
}

#[test]
fn policy_revision_rotation_invalidates_resources_after_reauth() {
    let host = open_host();
    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let session = host.authority.issue_session(&auth).unwrap();
    let workspace = host
        .authority
        .issue_workspace(&auth, host.root.as_path())
        .unwrap();
    let resource = host
        .authority
        .issue_resource(&auth, session, workspace, ContentDigest::of_bytes(b"frame"))
        .unwrap();

    host.authority.rotate_policy_revision(&host.admin).unwrap();
    let fresh = host.authority.authenticate(SECRET_A).unwrap();
    assert!(matches!(
        host.authority.resource_binding(&fresh, resource),
        Err(AuthorityError::UnknownResource)
    ));
    assert!(matches!(
        host.authority.seal_capability(
            &fresh,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000
        ),
        Err(AuthorityError::UnknownResource)
    ));
}

#[test]
fn foreign_resource_handles_fail_closed_without_becoming_existence_oracles() {
    let first = open_host();
    install_two_credentials(&first);
    let a = first.authority.authenticate(SECRET_A).unwrap();
    let session = first.authority.issue_session(&a).unwrap();
    let workspace = first
        .authority
        .issue_workspace(&a, first.root.as_path())
        .unwrap();
    let resource = first
        .authority
        .issue_resource(&a, session, workspace, ContentDigest::of_bytes(b"frame"))
        .unwrap();

    let second = open_host();
    install_two_credentials(&second);
    let other_host = second.authority.authenticate(SECRET_B).unwrap();

    assert!(first.authority.resource_binding(&a, resource).is_ok());
    let b_on_first = first.authority.authenticate(SECRET_B).unwrap();

    let cross_principal = first
        .authority
        .resource_binding(&b_on_first, resource)
        .unwrap_err();
    let foreign_host = second
        .authority
        .resource_binding(&other_host, resource)
        .unwrap_err();
    let never_issued = first
        .authority
        .seal_capability(
            &a,
            second_resource_from_unrelated_host(),
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .unwrap_err();

    for denial in [&cross_principal, &foreign_host, &never_issued] {
        assert!(matches!(denial, AuthorityError::UnknownResource));
    }
    assert_eq!(cross_principal, foreign_host);
    assert_eq!(cross_principal, never_issued);
    assert_eq!(
        cross_principal.to_string(),
        foreign_host.to_string(),
        "denial text must not distinguish foreign from cross-principal"
    );
}

/// A resource incarnation issued by another host root, never present in `first`.
fn second_resource_from_unrelated_host() -> ResourceIncarnation {
    let other = open_host();
    other
        .authority
        .set_credentials(&other.admin, &[HostCredential::new("a", SECRET_A).unwrap()])
        .unwrap();
    let auth = other.authority.authenticate(SECRET_A).unwrap();
    let session = other.authority.issue_session(&auth).unwrap();
    let workspace = other
        .authority
        .issue_workspace(&auth, other.root.as_path())
        .unwrap();
    other
        .authority
        .issue_resource(
            &auth,
            session,
            workspace,
            ContentDigest::of_bytes(b"foreign"),
        )
        .unwrap()
}

#[test]
fn internal_liveness_probe_never_mints_principal_authority() {
    let host = open_host();
    let probe = InternalServiceAuthority::open_probe(&host.root).unwrap();
    let before = probe.liveness_projection().unwrap();
    assert!(!before.credentials_configured);

    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let after = probe.liveness_projection().unwrap();
    assert!(after.credentials_configured);
    assert_eq!(after.policy_revision, 1);

    let rendered = serde_json::to_string(&after).unwrap();
    assert!(!rendered.contains(SECRET_A));
    assert!(!rendered.contains(host.root.to_string_lossy().as_ref()));
}

#[test]
fn principal_projection_is_secret_and_path_free() {
    let host = open_host();
    host.authority
        .set_credentials(
            &host.admin,
            &[HostCredential::new("primary", SECRET_A).unwrap()],
        )
        .unwrap();
    let auth = host.authority.authenticate(SECRET_A).unwrap();
    let projection = host.authority.principal_projection(&auth).unwrap();
    let json = serde_json::to_string(&projection).unwrap();
    assert!(!json.contains(SECRET_A));
    assert!(!json.contains(host.root.to_string_lossy().as_ref()));
    assert_eq!(projection.owner_id, auth.owner_id());
}

#[test]
fn schema_v1_roots_are_rejected_fail_closed() {
    let host = open_host();
    std::fs::write(
        host.root.join("authority.json"),
        r#"{"schema_version":1,"owner_id":"account-1","admin_credential_fingerprint":"abc","control_epoch":1,"capability_generation":1,"next_auth_generation":1,"credentials":[],"resources":{},"capabilities":{},"leases":{},"attempts":{}}"#,
    )
    .unwrap();
    let root = host.root.clone();
    drop(host.authority);

    let probe = InternalServiceAuthority::open_probe(&root).unwrap();
    assert!(matches!(
        probe.liveness_projection(),
        Err(AuthorityError::CorruptState(_))
    ));
    assert!(matches!(
        HostAuthority::open(&root, &admin_credential()),
        Err(AuthorityError::CorruptState(_))
    ));
}

#[test]
fn schema_v1_with_policy_revision_field_is_still_rejected() {
    let host = open_host();
    std::fs::write(
        host.root.join("authority.json"),
        r#"{"schema_version":1,"owner_id":"account-1","admin_credential_fingerprint":"abc","control_epoch":1,"capability_generation":1,"policy_revision":1,"next_auth_generation":1,"credentials":[],"resources":{},"capabilities":{},"leases":{},"attempts":{}}"#,
    )
    .unwrap();
    let root = host.root.clone();
    drop(host.authority);

    let probe = InternalServiceAuthority::open_probe(&root).unwrap();
    let error = probe
        .liveness_projection()
        .expect_err("v1 must not be accepted even with policy_revision present");
    assert!(
        matches!(error, AuthorityError::CorruptState(ref msg) if msg.contains("unsupported authority schema version 1")),
        "got {error:?}"
    );
    assert!(matches!(
        HostAuthority::open(&root, &admin_credential()),
        Err(AuthorityError::CorruptState(_))
    ));
}
