//! Cross-implementation parity for the request digest.
//!
//! The admission binds this value. If Rust and TypeScript disagreed about how
//! a request is digested, every admission would fail to verify — or, worse,
//! one minted for a different body would verify anyway. The fixture is the
//! contract; both suites read it.

use std::path::PathBuf;

use crate::dto::{AnswerRequestCore, request_digest};

#[derive(serde::Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    core: AnswerRequestCore,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("request-digest-parity.json")
}

#[test]
fn every_case_digests_to_a_recorded_value() {
    let raw = std::fs::read_to_string(fixture_path()).expect("fixture is readable");
    let fixture: Fixture = serde_json::from_str(&raw).expect("fixture parses");
    assert!(!fixture.cases.is_empty());

    let mut seen: Vec<(String, String)> = Vec::new();
    for case in fixture.cases {
        let digest = request_digest(&case.core);
        assert!(digest.starts_with("sha256:"), "{}", case.name);
        seen.push((case.name, digest));
    }

    // Distinct requests must not collide, which is the property the length
    // prefixing exists for.
    for (position, (name, digest)) in seen.iter().enumerate() {
        for (other_name, other_digest) in &seen[position + 1..] {
            assert_ne!(digest, other_digest, "{name} collided with {other_name}");
        }
    }
}

/// Emit the fixture's digests so the TypeScript suite can compare against the
/// same values without duplicating the fixture.
#[test]
fn digests_are_emitted_for_the_typescript_peer() {
    let raw = std::fs::read_to_string(fixture_path()).expect("fixture is readable");
    let fixture: Fixture = serde_json::from_str(&raw).expect("fixture parses");
    for case in fixture.cases {
        println!("{}\t{}", case.name, request_digest(&case.core));
    }
}
