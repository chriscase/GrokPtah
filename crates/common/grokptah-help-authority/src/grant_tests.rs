//! Grant minting, verification, and forgery resistance.

use crate::grant::*;
use crate::*;

const CORPUS: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const INDEX: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const MANIFEST: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
const SOURCE_DIGEST: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";

fn key() -> GrantMintingKey {
    GrantMintingKey::new(vec![7u8; 32]).expect("key is long enough")
}

fn other_key() -> GrantMintingKey {
    GrantMintingKey::new(vec![9u8; 32]).expect("key is long enough")
}

fn manifest() -> ServedManifest {
    ServedManifest {
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: MANIFEST.into(),
    }
}

fn principal(capabilities: Vec<Capability>) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        principal_id: "alice".into(),
        tenant_id: "tenant-a".into(),
        project_ids: vec!["proj-1".into()],
        capabilities,
        policy_revision: "policy-7".into(),
    }
}

fn acceptance(action: Action) -> GrantAcceptance {
    GrantAcceptance {
        action,
        manifest: manifest(),
        policy_revision: "policy-7".into(),
        current_revision: 42,
        now_ms: 1_000,
    }
}

fn mint(action: Action, cap: Visibility, capabilities: Vec<Capability>) -> HelpGrant {
    mint_grant(
        &key(),
        &principal(capabilities),
        &manifest(),
        action,
        cap,
        42,
        1_000,
        60_000,
    )
    .expect("grant mints")
}

fn source(id: &str, visibility: Visibility) -> SourceDescriptor {
    SourceDescriptor {
        source_id: id.into(),
        visibility,
        tenant_id: "tenant-a".into(),
        project_id: match visibility {
            Visibility::Project => Some("proj-1".into()),
            _ => None,
        },
        owner_principal_id: match visibility {
            Visibility::Private => Some("alice".into()),
            _ => None,
        },
        digest: SOURCE_DIGEST.into(),
    }
}

// ------------------------------------------------------------ minting

#[test]
fn a_minted_grant_verifies_against_the_host_that_minted_it() {
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    assert_eq!(
        verify_grant(&key(), &grant, &acceptance(Action::Search)),
        Ok(())
    );
}

#[test]
fn the_minting_key_never_renders_its_secret() {
    // A derived Debug on any struct holding a key would otherwise print it.
    let rendered = format!("{:?}", key());
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(!rendered.contains('7'), "{rendered}");
}

#[test]
fn a_short_key_is_refused() {
    assert!(GrantMintingKey::new(vec![1u8; 31]).is_err());
    assert!(GrantMintingKey::new(vec![1u8; 32]).is_ok());
}

// ------------------------------------------------------------ forgery

#[test]
fn every_authority_field_is_covered_by_the_mac() {
    // This is the caller-forgery case: a renderer editing the grant it was
    // handed, trying to authorize a different or wider identity.
    let base = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    type Mutation = (&'static str, Box<dyn Fn(&mut HelpGrant)>);
    let mutations: Vec<Mutation> = vec![
        (
            "principal",
            Box::new(|g: &mut HelpGrant| g.principal_id = "mallory".into()),
        ),
        (
            "tenant",
            Box::new(|g: &mut HelpGrant| g.tenant_id = "tenant-z".into()),
        ),
        (
            "capabilities",
            Box::new(|g: &mut HelpGrant| g.capabilities.push(Capability::HelpSearchPrivate)),
        ),
        (
            "projects",
            Box::new(|g: &mut HelpGrant| g.project_ids.push("proj-9".into())),
        ),
        (
            "visibility cap",
            Box::new(|g: &mut HelpGrant| g.max_visibility = Visibility::Private),
        ),
        (
            "policy revision",
            Box::new(|g: &mut HelpGrant| g.policy_revision = "policy-8".into()),
        ),
        (
            "corpus digest",
            Box::new(|g: &mut HelpGrant| g.corpus_digest = "sha256:0".into()),
        ),
        (
            "index digest",
            Box::new(|g: &mut HelpGrant| g.index_digest = "sha256:0".into()),
        ),
        (
            "manifest digest",
            Box::new(|g: &mut HelpGrant| g.manifest_digest = "sha256:0".into()),
        ),
        (
            "grant revision",
            Box::new(|g: &mut HelpGrant| g.grant_revision = 43),
        ),
        (
            "expiry",
            Box::new(|g: &mut HelpGrant| g.expires_at_ms += 60_000),
        ),
        (
            "action",
            Box::new(|g: &mut HelpGrant| g.action = Action::Answer),
        ),
    ];
    for (name, mutate) in mutations {
        let mut forged = base.clone();
        mutate(&mut forged);
        assert_eq!(
            verify_grant(&key(), &forged, &acceptance(Action::Search)),
            Err(GrantRejection::Forged),
            "editing {name} was not detected"
        );
    }
}

#[test]
fn a_grant_minted_by_another_host_is_refused() {
    let foreign = mint_grant(
        &other_key(),
        &principal(vec![Capability::HelpSearch]),
        &manifest(),
        Action::Search,
        Visibility::Public,
        42,
        1_000,
        60_000,
    )
    .expect("mints");
    assert_eq!(
        verify_grant(&key(), &foreign, &acceptance(Action::Search)),
        Err(GrantRejection::Forged)
    );
}

#[test]
fn a_wholly_fabricated_grant_is_refused() {
    let mut fabricated = mint(
        Action::Search,
        Visibility::Private,
        vec![Capability::HelpSearch, Capability::HelpSearchPrivate],
    );
    fabricated.mac = format!("hmac-sha256:{}", "0".repeat(64));
    assert_eq!(
        verify_grant(&key(), &fabricated, &acceptance(Action::Search)),
        Err(GrantRejection::Forged)
    );
}

// -------------------------------------------------------- revision/time

#[test]
fn a_grant_from_a_previous_policy_or_index_revision_is_refused() {
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );

    let mut moved_policy = acceptance(Action::Search);
    moved_policy.policy_revision = "policy-8".into();
    assert_eq!(
        verify_grant(&key(), &grant, &moved_policy),
        Err(GrantRejection::StaleRevision)
    );

    let mut moved_revision = acceptance(Action::Search);
    moved_revision.current_revision = 43;
    assert_eq!(
        verify_grant(&key(), &grant, &moved_revision),
        Err(GrantRejection::StaleRevision)
    );
}

#[test]
fn a_grant_minted_against_a_different_index_is_refused() {
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    let mut rebuilt = acceptance(Action::Search);
    rebuilt.manifest.index_digest = "sha256:0".into();
    assert_eq!(
        verify_grant(&key(), &grant, &rebuilt),
        Err(GrantRejection::IndexMismatch)
    );
}

#[test]
fn a_grant_is_refused_outside_its_window() {
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    let mut expired = acceptance(Action::Search);
    expired.now_ms = 61_001;
    assert_eq!(
        verify_grant(&key(), &grant, &expired),
        Err(GrantRejection::Expired)
    );

    let mut early = acceptance(Action::Search);
    early.now_ms = 999;
    assert_eq!(
        verify_grant(&key(), &grant, &early),
        Err(GrantRejection::Expired)
    );
}

#[test]
fn a_grant_cannot_be_replayed_into_another_action() {
    let search = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch, Capability::HelpAnswer],
    );
    assert_eq!(
        verify_grant(&key(), &search, &acceptance(Action::Answer)),
        Err(GrantRejection::ActionMismatch)
    );
}

// ------------------------------------------------- grant-based decisions

#[test]
fn the_renderer_cannot_supply_its_own_identity_or_index() {
    // `authorize_with_grant` takes no principal and no served digests from the
    // caller: both come from the verified grant, which is the whole point.
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    let response = authorize_with_grant(
        &key(),
        &grant,
        &acceptance(Action::Search),
        &[source("pub-1", Visibility::Public)],
    );
    assert!(response.allowed);
    assert_eq!(response.receipt.principal_id, "alice");
    assert_eq!(response.receipt.tenant_id, "tenant-a");
    assert_eq!(response.receipt.index_digest, INDEX);
}

#[test]
fn a_forged_grant_denies_the_action_and_every_source() {
    let mut forged = mint(
        Action::Search,
        Visibility::Private,
        vec![Capability::HelpSearch, Capability::HelpSearchPrivate],
    );
    forged.principal_id = "mallory".into();
    let response = authorize_with_grant(
        &key(),
        &forged,
        &acceptance(Action::Search),
        &[source("priv-1", Visibility::Private)],
    );
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::ForgedGrant));
    assert!(response.receipt.allowed_source_ids.is_empty());
}

#[test]
fn the_visibility_cap_narrows_even_a_capable_grant() {
    // A grant minted for public reach must not be widened by a request that
    // names a project source the principal could otherwise read.
    let capped = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch, Capability::HelpSearchProject],
    );
    let response = authorize_with_grant(
        &key(),
        &capped,
        &acceptance(Action::Search),
        &[
            source("pub-1", Visibility::Public),
            source("proj-1", Visibility::Project),
        ],
    );
    assert!(response.allowed);
    assert_eq!(
        response.receipt.allowed_source_ids,
        vec!["pub-1".to_string()]
    );
    assert!(
        response
            .receipt
            .denied
            .iter()
            .any(|decision| decision.denied_because == Some(DenyReason::VisibilityCapped)),
        "the project source should be capped, not silently allowed"
    );
}

#[test]
fn a_wider_grant_reaches_what_it_was_minted_for() {
    let wide = mint(
        Action::Search,
        Visibility::Project,
        vec![Capability::HelpSearch, Capability::HelpSearchProject],
    );
    let response = authorize_with_grant(
        &key(),
        &wide,
        &acceptance(Action::Search),
        &[
            source("pub-1", Visibility::Public),
            source("proj-1", Visibility::Project),
        ],
    );
    assert!(response.allowed);
    assert_eq!(response.receipt.allowed_source_ids.len(), 2);
}

// -------------------------------------------------------- receipt identity

#[test]
fn distinct_decisions_do_not_collide_in_the_receipt_digest() {
    // The earlier digest covered action, principal, and bare source ids only,
    // so these all produced the same value.
    let base = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    let baseline = authorize_with_grant(
        &key(),
        &base,
        &acceptance(Action::Search),
        &[source("pub-1", Visibility::Public)],
    )
    .receipt
    .receipt_digest;

    // Same allowed ids, different capability set.
    let wider = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch, Capability::HelpAnswer],
    );
    let wider_digest = authorize_with_grant(
        &key(),
        &wider,
        &acceptance(Action::Search),
        &[source("pub-1", Visibility::Public)],
    )
    .receipt
    .receipt_digest;
    assert_ne!(
        baseline, wider_digest,
        "capability set must change the receipt"
    );

    // Same allowed ids, different source bytes.
    let mut substituted = source("pub-1", Visibility::Public);
    substituted.digest = "sha256:aaaa".into();
    let substituted_digest =
        authorize_with_grant(&key(), &base, &acceptance(Action::Search), &[substituted])
            .receipt
            .receipt_digest;
    assert_ne!(
        baseline, substituted_digest,
        "source digest must change the receipt"
    );

    // Same ids, allow versus deny.
    let mut powerless_principal = principal(vec![]);
    powerless_principal.capabilities.clear();
    let powerless = mint_grant(
        &key(),
        &powerless_principal,
        &manifest(),
        Action::Search,
        Visibility::Public,
        42,
        1_000,
        60_000,
    )
    .expect("mints");
    let denied_digest = authorize_with_grant(
        &key(),
        &powerless,
        &acceptance(Action::Search),
        &[source("pub-1", Visibility::Public)],
    )
    .receipt
    .receipt_digest;
    assert_ne!(baseline, denied_digest, "outcome must change the receipt");
}

#[test]
fn a_receipt_never_echoes_an_unbounded_or_control_bearing_identifier() {
    // A denial used to clone whatever the caller sent straight into the audit
    // record.
    let grant = mint(Action::Search, Visibility::Public, vec![]);
    let mut hostile = source("x", Visibility::Public);
    hostile.source_id = format!("a\u{1b}[31mb\n{}", "z".repeat(4_000));
    let response = authorize_with_grant(&key(), &grant, &acceptance(Action::Search), &[hostile]);
    assert!(!response.allowed);
    for decision in &response.receipt.denied {
        assert!(
            decision.source_id.len() <= MAX_RECEIPT_ID_BYTES,
            "receipt id was not bounded: {} bytes",
            decision.source_id.len()
        );
        assert!(
            !decision.source_id.chars().any(char::is_control),
            "receipt id carried a control character"
        );
    }
}

#[test]
fn a_receipt_still_carries_no_path_content_or_query_text() {
    let grant = mint(
        Action::Search,
        Visibility::Public,
        vec![Capability::HelpSearch],
    );
    let response = authorize_with_grant(
        &key(),
        &grant,
        &acceptance(Action::Search),
        &[source("pub-1", Visibility::Public)],
    );
    let serialized = serde_json::to_string(&response.receipt).expect("serializes");
    for forbidden in [
        "docs/", "README", ".md", "/Users/", "/home/", "heading", "query",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "receipt leaked {forbidden}"
        );
    }
    // And it never carries the minting secret or the MAC.
    assert!(!serialized.contains("hmac-sha256"));
}
