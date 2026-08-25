//! The closed contract: every code accounted for, every payload free of paths
//! and validated against the shared JSON Schema fixtures.

use std::collections::BTreeSet;

use super::support::Fixture;
use crate::{
    ContentClass, ContentVerdict, DocumentIdentity, Eol, IdentityStability, SOURCE_VIEW_CONTRACT,
    SourceViewError, boundary_message,
};

/// One instance of every variant, in declaration order.
fn every_error() -> Vec<SourceViewError> {
    vec![
        SourceViewError::NoApprovedRoot,
        SourceViewError::SnapshotUnknown,
        SourceViewError::TokenMalformed,
        SourceViewError::TokenSignatureInvalid,
        SourceViewError::TokenExpired,
        SourceViewError::TokenRevoked,
        SourceViewError::PrincipalMismatch,
        SourceViewError::PolicyDrift,
        SourceViewError::UnknownRoot,
        SourceViewError::EmptyPath,
        SourceViewError::NulByte,
        SourceViewError::AbsolutePathOutsideRoot,
        SourceViewError::ParentEscape,
        SourceViewError::InvalidComponent {
            segment: "we*rd".into(),
        },
        SourceViewError::ReservedDeviceName {
            segment: "NUL".into(),
        },
        SourceViewError::AlternateDataStream {
            segment: "notes.txt:hidden".into(),
        },
        SourceViewError::UnsupportedPathForm,
        SourceViewError::SymlinkRejected {
            segment: "src/link".into(),
        },
        SourceViewError::ReparsePointRejected {
            segment: "src/junction".into(),
        },
        SourceViewError::NotFound {
            segment: "src/absent.rs".into(),
        },
        SourceViewError::NotAFile {
            segment: "src".into(),
        },
        SourceViewError::OutsideRoot,
        SourceViewError::RootIdentityChanged,
        SourceViewError::DocumentChanged,
        SourceViewError::TooLarge {
            byte_len: 99,
            max_bytes: 64,
        },
        SourceViewError::RangeInvalid,
        SourceViewError::CursorInvalid,
        SourceViewError::RootUnavailable,
        SourceViewError::Io {
            detail: "permission_denied".into(),
        },
    ]
}

#[test]
fn the_published_code_list_matches_the_enum_exactly() {
    let declared: BTreeSet<&str> = SourceViewError::CODES.iter().copied().collect();
    let observed: BTreeSet<&str> = every_error().iter().map(SourceViewError::code).collect();
    assert_eq!(
        declared, observed,
        "CODES and the enum must not drift; add the code to both or neither",
    );
    assert_eq!(
        SourceViewError::CODES.len(),
        declared.len(),
        "CODES must not contain duplicates",
    );
    assert_eq!(
        every_error().len(),
        declared.len(),
        "every_error() must cover each variant exactly once",
    );
}

#[test]
fn every_error_serialises_with_its_code_as_the_tag() {
    for error in every_error() {
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(
            value.get("code").and_then(serde_json::Value::as_str),
            Some(error.code()),
            "the wire tag is the machine code",
        );
    }
}

#[test]
fn a_boundary_message_leads_with_the_code_and_stays_bounded() {
    for error in every_error() {
        let message = boundary_message(&error);
        assert!(
            message.starts_with(&format!("{}: ", error.code())),
            "`{message}` must lead with its code",
        );
        assert!(message.len() < 512, "a refusal must not be unbounded prose");
    }
}

#[test]
fn no_refusal_carries_an_absolute_path_or_file_content() {
    let fixture = Fixture::new();
    fixture.write("secret.txt", b"TOP-SECRET-CONTENT\n");
    let token = fixture.token();
    let absolute = fixture.root.display().to_string();

    let refusals = vec![
        fixture.open(&token, "../escape").unwrap_err(),
        fixture.open(&token, "src/absent.rs").unwrap_err(),
        fixture.open(&token, "src").unwrap_err(),
        fixture
            .open(&token, &format!("{absolute}-other/leak.txt"))
            .unwrap_err(),
        fixture
            .open(
                "sv1.deadbeefdeadbeefdeadbeefdeadbeef.0.00112233445566778899aabbccddeeff",
                "x",
            )
            .unwrap_err(),
    ];
    for error in refusals {
        let rendered = format!(
            "{}|{}",
            boundary_message(&error),
            serde_json::to_string(&error).expect("json")
        );
        assert!(
            !rendered.contains(&absolute),
            "`{rendered}` leaks the absolute root path",
        );
        assert!(
            !rendered.contains("TOP-SECRET-CONTENT"),
            "`{rendered}` leaks file content",
        );
    }
}

#[test]
fn a_document_never_carries_an_absolute_path() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let document = fixture.open(&token, "src/nested/deep.txt").expect("read");
    let json = serde_json::to_string(&document).expect("serialize");

    assert!(!json.contains(&fixture.root.display().to_string()));
    assert!(!json.contains("absolutePath"));
    assert!(!json.contains("rootPath"));
    assert_eq!(document.contract, SOURCE_VIEW_CONTRACT);
    assert_eq!(document.relative_path, "src/nested/deep.txt");
    assert_eq!(
        document.root.label.matches('/').count(),
        1,
        "the label is two segments"
    );
}

#[test]
fn a_document_serialises_with_the_documented_shape() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let document = fixture.open(&token, "src/main.rs").expect("read");
    let value = serde_json::to_value(&document).expect("serialize");
    let object = value.as_object().expect("object");

    let expected: BTreeSet<&str> = [
        "contract",
        "root",
        "snapshotId",
        "revision",
        "relativePath",
        "language",
        "byteLen",
        "content",
        "identity",
        "limits",
        "chunk",
    ]
    .into_iter()
    .collect();
    let observed: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(
        observed, expected,
        "the document shape is part of the contract"
    );

    let root = object["root"].as_object().expect("root object");
    let root_keys: BTreeSet<&str> = root.keys().map(String::as_str).collect();
    assert_eq!(
        root_keys,
        [
            "token",
            "kind",
            "label",
            "pathDigest",
            "identityDigest",
            "runId"
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
    );
}

#[test]
fn value_types_serialise_in_their_documented_forms() {
    assert_eq!(
        serde_json::to_string(&ContentVerdict::TextLossy).expect("json"),
        "\"text_lossy\"",
    );
    assert_eq!(serde_json::to_string(&Eol::Crlf).expect("json"), "\"crlf\"");
    assert_eq!(
        serde_json::to_string(&ContentClass {
            verdict: ContentVerdict::Text,
            scanned_bytes: 12,
            complete_scan: true,
        })
        .expect("json"),
        r#"{"verdict":"text","scannedBytes":12,"completeScan":true}"#,
    );
    assert_eq!(
        serde_json::to_string(&DocumentIdentity::Pinned {
            digest: "ab".repeat(32),
            stability: IdentityStability::Heuristic,
        })
        .expect("json"),
        format!(
            r#"{{"kind":"pinned","digest":"{}","stability":"heuristic"}}"#,
            "ab".repeat(32)
        ),
    );
}

#[test]
fn integer_fields_stay_inside_the_range_a_json_consumer_can_hold() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let document = fixture.open(&token, "src/main.rs").expect("read");

    // Every numeric field must round-trip through a double without loss, or a
    // JavaScript consumer silently reads a different number.
    for value in [
        document.byte_len,
        document.chunk.bytes_consumed,
        document.chunk.start_byte,
        document.limits.max_bytes,
    ] {
        assert!(
            value <= (1u64 << 53),
            "{value} exceeds the exactly-representable integer range",
        );
    }
    assert!(document.revision <= (1u64 << 53));
    for line in &document.chunk.lines {
        assert!(line.number >= 1, "line numbers are 1-based");
    }
}

#[test]
fn the_fixture_file_covers_every_published_error_code() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/schemas/grokptah-source-view.v1.fixtures.json"
    ))
    .expect("fixtures parse");
    let errors = fixtures["errors"].as_array().expect("errors array");
    let covered: BTreeSet<&str> = errors
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect();
    let declared: BTreeSet<&str> = SourceViewError::CODES.iter().copied().collect();
    assert_eq!(
        covered, declared,
        "the shared fixtures must exercise every code and no others",
    );
}

#[test]
fn every_serialised_error_matches_its_golden_fixture() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/schemas/grokptah-source-view.v1.fixtures.json"
    ))
    .expect("fixtures parse");
    let golden = fixtures["errors"].as_array().expect("errors array");

    for error in every_error() {
        let produced = serde_json::to_value(&error).expect("serialize");
        let expected = golden
            .iter()
            .find(|entry| entry["code"] == produced["code"])
            .unwrap_or_else(|| panic!("no fixture for {}", error.code()));
        assert_eq!(
            &produced,
            expected,
            "the wire form of {} drifted from its golden fixture",
            error.code(),
        );
    }
}

#[test]
fn the_shared_fixtures_validate_against_the_shared_schema() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/schemas/grokptah-source-view.v1.schema.json"
    ))
    .expect("schema parses");
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/schemas/grokptah-source-view.v1.fixtures.json"
    ))
    .expect("fixtures parse");
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");

    for entry in fixtures["errors"].as_array().expect("errors array") {
        assert!(
            validator.is_valid(entry),
            "a golden error fixture failed schema validation: {entry}",
        );
    }

    for (group, valid) in [("valid", true), ("invalid", false)] {
        for (kind, entries) in fixtures[group].as_object().expect("group object") {
            for entry in entries.as_array().expect("array") {
                let payload = if valid { entry } else { &entry["value"] };
                assert_eq!(
                    validator.is_valid(payload),
                    valid,
                    "{group}/{kind} fixture disagreed with the schema: {payload}",
                );
            }
        }
    }
}

#[test]
fn the_replay_policy_and_contract_ids_are_stable() {
    assert_eq!(SOURCE_VIEW_CONTRACT, "grokptah.source-view.v1");
    assert_eq!(crate::REPLAY_POLICY, "idempotent-within-validity");
    assert_eq!(crate::TOKEN_VERSION, "sv1");
}
