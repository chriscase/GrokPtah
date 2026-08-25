//! Manifest parsing, source-byte digests, and descriptor enforcement.

use crate::grant::*;
use crate::manifest::*;
use crate::*;

const CORPUS: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const INDEX: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

fn entry(source_id: &str, visibility: Visibility, body: &str) -> ManifestEntry {
    ManifestEntry {
        source_id: source_id.into(),
        path: "docs/EXAMPLE.md".into(),
        heading: "Lifecycle".into(),
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
        digest: source_digest(
            source_id,
            "docs/EXAMPLE.md",
            "Lifecycle",
            visibility,
            "tenant-a",
            match visibility {
                Visibility::Project => Some("proj-1"),
                _ => None,
            },
            match visibility {
                Visibility::Private => Some("alice"),
                _ => None,
            },
            body,
        ),
    }
}

// -------------------------------------------------------- source bytes

#[test]
fn a_source_digest_changes_when_its_bytes_change() {
    // The original digest covered id, path, heading, and visibility only, so
    // substituting the text behind a citation changed nothing observable.
    let original = entry("s1", Visibility::Public, "Resume only from a checkpoint.");
    let substituted = entry("s1", Visibility::Public, "Resume freely after a restart.");
    assert_ne!(
        original.digest, substituted.digest,
        "substituting the source bytes must change the digest"
    );
}

#[test]
fn a_source_digest_changes_when_its_visibility_changes() {
    // Bytes alone would let the same text be relabelled without detection.
    let public = entry("s1", Visibility::Public, "same text");
    let private = entry("s1", Visibility::Private, "same text");
    assert_ne!(public.digest, private.digest);
}

#[test]
fn normalization_folds_line_endings_but_not_content() {
    assert_eq!(
        normalize_source_bytes("a\r\nb  \r\nc\n\n"),
        normalize_source_bytes("a\nb\nc")
    );
    // Interior blank lines and wording are content, not formatting.
    assert_ne!(
        normalize_source_bytes("a\n\nb"),
        normalize_source_bytes("a\nb")
    );
    assert_ne!(
        normalize_source_bytes("a b"),
        normalize_source_bytes("a  b")
    );
}

#[test]
fn two_sections_sharing_a_heading_do_not_share_a_digest() {
    let first = source_digest(
        "s1",
        "docs/A.md",
        "Lifecycle",
        Visibility::Public,
        "tenant-a",
        None,
        None,
        "one body",
    );
    let second = source_digest(
        "s2",
        "docs/B.md",
        "Lifecycle",
        Visibility::Public,
        "tenant-a",
        None,
        None,
        "another body",
    );
    assert_ne!(first, second);
}

// ------------------------------------------------------ duplicate keys

#[test]
fn a_duplicate_json_key_is_refused_before_parsing() {
    // serde_json keeps the last duplicate silently, so this document would
    // otherwise parse as `public`.
    let raw = r#"[{"sourceId":"s1","visibility":"private","visibility":"public","path":"p","heading":"h","tenantId":"t","digest":"d"}]"#;
    match SourceManifest::from_json(raw) {
        Err(ManifestError::DuplicateKey(key)) => assert_eq!(key, "visibility"),
        other => panic!("expected a duplicate-key refusal, got {other:?}"),
    }
}

#[test]
fn duplicate_keys_are_caught_at_any_nesting_depth() {
    let nested = r#"{"outer":{"inner":{"a":1,"a":2}}}"#;
    assert!(matches!(
        reject_duplicate_keys(nested),
        Err(ManifestError::DuplicateKey(_))
    ));
}

#[test]
fn repeated_keys_in_sibling_objects_are_not_duplicates() {
    // Every object has its own key set; `sourceId` appearing once per entry is
    // ordinary, and rejecting it would make the check useless.
    let siblings = r#"[{"sourceId":"a"},{"sourceId":"b"}]"#;
    assert_eq!(reject_duplicate_keys(siblings), Ok(()));
}

#[test]
fn a_key_containing_a_brace_or_quote_does_not_confuse_the_scanner() {
    let tricky = r#"{"a{b":1,"c\"d":2}"#;
    assert_eq!(reject_duplicate_keys(tricky), Ok(()));
    let tricky_duplicate = r#"{"a{b":1,"a{b":2}"#;
    assert!(matches!(
        reject_duplicate_keys(tricky_duplicate),
        Err(ManifestError::DuplicateKey(_))
    ));
}

#[test]
fn string_values_are_not_mistaken_for_keys() {
    // A value that looks like a repeated key must not trip the scanner.
    let values = r#"{"a":"x","b":"x"}"#;
    assert_eq!(reject_duplicate_keys(values), Ok(()));
}

// ------------------------------------------------ descriptor enforcement

#[test]
fn a_descriptor_that_is_not_the_manifests_record_is_refused() {
    let manifest = SourceManifest::from_entries(vec![entry("s1", Visibility::Public, "body")])
        .expect("builds");
    let genuine = manifest.describe(&["s1".to_string()]);
    assert_eq!(manifest.enforce_descriptor(&genuine[0]), Ok(()));

    // A fabricated digest.
    let mut forged = genuine[0].clone();
    forged.digest = "sha256:0000".into();
    assert!(manifest.enforce_descriptor(&forged).is_err());

    // A relabelled visibility, keeping the manifest's digest.
    let mut relabelled = genuine[0].clone();
    relabelled.visibility = Visibility::Private;
    assert!(manifest.enforce_descriptor(&relabelled).is_err());

    // A retenanted record.
    let mut retenanted = genuine[0].clone();
    retenanted.tenant_id = "tenant-z".into();
    assert!(manifest.enforce_descriptor(&retenanted).is_err());

    // An id the manifest never had.
    let mut unknown = genuine[0].clone();
    unknown.source_id = "ghost".into();
    assert!(manifest.enforce_descriptor(&unknown).is_err());
}

#[test]
fn the_manifest_digest_follows_its_entries() {
    let one = SourceManifest::from_entries(vec![entry("s1", Visibility::Public, "body")])
        .expect("builds");
    let substituted =
        SourceManifest::from_entries(vec![entry("s1", Visibility::Public, "different body")])
            .expect("builds");
    assert_ne!(one.manifest_digest(), substituted.manifest_digest());

    // Order must not matter; content must.
    let forward = SourceManifest::from_entries(vec![
        entry("s1", Visibility::Public, "a"),
        entry("s2", Visibility::Public, "b"),
    ])
    .expect("builds");
    let reversed = SourceManifest::from_entries(vec![
        entry("s2", Visibility::Public, "b"),
        entry("s1", Visibility::Public, "a"),
    ])
    .expect("builds");
    assert_eq!(forward.manifest_digest(), reversed.manifest_digest());
}

#[test]
fn a_repeated_source_id_is_refused() {
    let result = SourceManifest::from_entries(vec![
        entry("s1", Visibility::Public, "a"),
        entry("s1", Visibility::Private, "b"),
    ]);
    assert!(matches!(result, Err(ManifestError::DuplicateSourceId(_))));
}

// --------------------------------------------- end-to-end authorization

fn key() -> GrantMintingKey {
    GrantMintingKey::new(vec![7u8; 32]).expect("key")
}

fn manifest_of(entries: Vec<ManifestEntry>) -> SourceManifest {
    SourceManifest::from_entries(entries).expect("builds")
}

fn grant_for(
    manifest: &SourceManifest,
    capabilities: Vec<Capability>,
) -> (HelpGrant, GrantAcceptance) {
    let served = ServedManifest {
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: manifest.manifest_digest().to_string(),
    };
    let principal = AuthenticatedPrincipal {
        principal_id: "alice".into(),
        tenant_id: "tenant-a".into(),
        project_ids: vec!["proj-1".into()],
        capabilities,
        policy_revision: "policy-7".into(),
    };
    let grant = mint_grant(
        &key(),
        &principal,
        &served,
        Action::Search,
        Visibility::Private,
        42,
        1_000,
        60_000,
    )
    .expect("mints");
    let acceptance = GrantAcceptance {
        action: Action::Search,
        manifest: served,
        policy_revision: "policy-7".into(),
        current_revision: 42,
        now_ms: 1_000,
    };
    (grant, acceptance)
}

#[test]
fn authorization_rebuilds_descriptors_from_the_manifest() {
    let manifest = manifest_of(vec![
        entry("pub-1", Visibility::Public, "public body"),
        entry("priv-1", Visibility::Private, "private body"),
    ]);
    let (grant, acceptance) = grant_for(&manifest, vec![Capability::HelpSearch]);
    let response = authorize_against_manifest(
        &key(),
        &grant,
        &acceptance,
        &manifest,
        &["pub-1".to_string(), "priv-1".to_string()],
    );
    assert!(response.allowed);
    // The private source is default-denied: the grant carries no private
    // capability, and the caller had no way to relabel it.
    assert_eq!(
        response.receipt.allowed_source_ids,
        vec!["pub-1".to_string()]
    );
}

#[test]
fn an_unknown_source_id_is_denied_rather_than_dropped() {
    let manifest = manifest_of(vec![entry("pub-1", Visibility::Public, "body")]);
    let (grant, acceptance) = grant_for(&manifest, vec![Capability::HelpSearch]);
    let response = authorize_against_manifest(
        &key(),
        &grant,
        &acceptance,
        &manifest,
        &["pub-1".to_string(), "ghost".to_string()],
    );
    assert!(
        response
            .receipt
            .denied
            .iter()
            .any(|decision| decision.source_id == "ghost"
                && decision.denied_because == Some(DenyReason::SourceDigestMismatch)),
        "an unknown id must appear as a denial, not vanish: {:?}",
        response.receipt.denied
    );
}

#[test]
fn a_manifest_rebuild_invalidates_the_grant() {
    // Source-byte substitution changes the manifest digest, which the grant is
    // bound to, so a grant minted against the old bytes stops verifying.
    let manifest = manifest_of(vec![entry("pub-1", Visibility::Public, "original")]);
    let (grant, mut acceptance) = grant_for(&manifest, vec![Capability::HelpSearch]);
    let rebuilt = manifest_of(vec![entry("pub-1", Visibility::Public, "substituted")]);
    acceptance.manifest.manifest_digest = rebuilt.manifest_digest().to_string();

    let response = authorize_against_manifest(
        &key(),
        &grant,
        &acceptance,
        &rebuilt,
        &["pub-1".to_string()],
    );
    assert!(!response.allowed);
    assert_eq!(response.denied_because, Some(DenyReason::StaleIndex));
}
