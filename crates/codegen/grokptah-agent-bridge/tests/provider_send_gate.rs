//! The provider-send gate.
//!
//! #478 asks for *one* provider-send lattice. Its first acceptance criterion is
//! structural rather than durable: **every provider call site uses the same
//! transport boundary, and a gate rejects unbound call sites.** That half needs
//! no ledger, no permit and no authority, so it can be enforced on current main
//! — and it is the half that keeps a fifth send path from appearing quietly.
//!
//! The durable half — recording which attempt reached which state, and the
//! `Settled` distinction — is #497's G3 and is deliberately not built here. A
//! second durable attempt ledger beside G3 is what the exact-head audit of this
//! branch rejected.
//!
//! This gate keys on **constructing a provider request**: the completions URL
//! and the HTTP client. A gate that inspected only the two known helpers would
//! report full coverage while `provider_qualification.rs` spent the operator's
//! credential unrecorded — which is the historical regression #494 found and
//! which is still true on `main` today.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Count matches per file, keyed by path relative to `src/`.
fn count_matching(needle: &str) -> BTreeMap<String, usize> {
    let root = src_root();
    let mut found = BTreeMap::new();
    for path in rust_sources(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let hits = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains(needle))
            .count();
        if hits > 0 {
            let rel = path
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            found.insert(rel, hits);
        }
    }
    found
}

/// Every site that builds an outbound provider completions request.
///
/// The inventory is exact on purpose. A new entry means a new physical send
/// path, and this test failing is the intended way to find out.
#[test]
fn every_provider_send_site_is_inventoried() {
    let sites = count_matching(r#"format!("{}/chat/completions""#);

    let expected: BTreeMap<String, usize> = [
        // Bound: the instrumented agent step. Carries a
        // `ProviderObservationContext`, and its transient retry stands down
        // unless non-delivery is proven.
        ("host_helpers.rs".to_string(), 2),
        // Unbound: qualification probes. See the dedicated test below.
        ("provider_qualification.rs".to_string(), 2),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        sites, expected,
        "the set of provider-send sites changed; a new physical send path must \
         be reviewed and bound, not added silently"
    );
}

/// **Characterization — three of the four send sites are unrecorded.**
///
/// `provider_qualification.rs` builds its own client and POSTs to
/// `/chat/completions` with **no** observation instrumentation at all, so those
/// sends spend the operator's credential without producing a record. #494 found
/// exactly this and it is still true on `main`.
///
/// This test asserts the gap rather than papering over it: closing it means
/// routing those probes through the instrumented boundary, which is #478's
/// remaining work above #497's G3.
#[test]
fn provider_qualification_sends_are_not_yet_instrumented() {
    let instrumented = count_matching("ProviderObservationContext");
    assert!(
        instrumented.contains_key("host_helpers.rs"),
        "the agent step must stay instrumented"
    );
    assert!(
        !instrumented.contains_key("provider_qualification.rs"),
        "provider_qualification.rs is expected to be uninstrumented today; if it \
         is now bound, this gap is closed and the test should assert that instead"
    );
}

/// Every HTTP client in the crate is accounted for, so a provider send cannot
/// hide behind a client built for something else.
#[test]
fn every_http_client_is_accounted_for() {
    let mut clients = count_matching("reqwest::Client::builder()");
    for (file, hits) in count_matching("reqwest::Client::new()") {
        *clients.entry(file).or_insert(0) += hits;
    }

    // Each entry is either a provider send or has a stated reason to be
    // something else. Counts are not asserted here — only that no *file* builds
    // a client without appearing in this reviewed list.
    let accounted: BTreeMap<&str, &str> = [
        (
            "host_helpers.rs",
            "provider sends, plus the bounded web_fetch tool",
        ),
        (
            "provider_qualification.rs",
            "provider qualification probes (unbound; see above)",
        ),
        (
            "provider_discovery.rs",
            "GET model listing, not a completions send",
        ),
        ("auth_store.rs", "OIDC token endpoint, not a provider send"),
        ("mcp_control.rs", "loopback control plane, not a provider"),
        ("mcp_control_client.rs", "loopback control plane client"),
    ]
    .into_iter()
    .collect();

    let unaccounted: Vec<&String> = clients
        .keys()
        .filter(|file| !accounted.contains_key(file.as_str()))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "these files build an HTTP client with no reviewed reason: {unaccounted:?}"
    );
}

/// The durable core stays transport-free, so the gate above can never be
/// satisfied by moving a send into it.
#[test]
fn the_durable_core_builds_no_http_client() {
    for needle in [
        "reqwest::Client::builder()",
        "reqwest::Client::new()",
        r#"format!("{}/chat/completions""#,
    ] {
        let matches = count_matching(needle);
        let offenders: Vec<&String> = matches
            .keys()
            .filter(|file| file.starts_with("durable/"))
            .collect();
        assert!(
            offenders.is_empty(),
            "the durable core must not construct provider requests: {offenders:?}"
        );
    }
}

/// The retry rule applies at the one send site that retries on transport
/// failure, and that site is the instrumented one.
#[test]
fn the_transport_retry_rule_lives_at_the_instrumented_send_site() {
    let gated = count_matching("if delivery.may_auto_retry()");
    assert_eq!(
        gated.get("host_helpers.rs").copied(),
        Some(1),
        "exactly one transport retry is gated on proven non-delivery"
    );
    assert_eq!(
        gated.len(),
        1,
        "no other file may hold a transport-retry decision: {gated:?}"
    );
}
