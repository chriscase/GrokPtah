//! No provider request may be built outside the admitted send boundary.
//!
//! The type system already forbids most of this: the transport takes an
//! `AdmittedCall` and has no other way to learn a URL, a credential, or a
//! body. What it cannot forbid is somebody building a *fresh* `reqwest` client
//! somewhere new and posting to a chat-completions endpoint directly — which
//! is exactly how the boundary eroded before.
//!
//! So this test reads the crate's own source and holds the line as a fact
//! about the tree rather than a habit. It is deliberately a source scan: an
//! ordinary test cannot observe a call site that no test happens to reach.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Files permitted to construct a provider chat request.
///
/// `provider_transport.rs` is the boundary itself. `request_admission.rs`
/// seals the bytes it sends. Nothing else may.
const PROVIDER_CALL_ALLOWLIST: [&str; 2] = ["provider_transport.rs", "request_admission.rs"];

/// Provider calls that are *known* to bypass the boundary and are not yet
/// fixed. This is a debt list, not an approval.
///
/// `provider_qualification.rs` runs the capability probe, and it has a real
/// ordering problem the rest of the boundary does not: admission refuses a
/// model whose capabilities are unprobed, and this *is* the probe that
/// establishes them. Routing it through `send_admitted` therefore needs a
/// distinct qualification admission — one that records a Run and a
/// ProviderAttempt per physical request, transmits the idempotency key, and
/// fails closed on the ledger, but does not demand the very evidence it is
/// about to produce. That is not built yet.
///
/// Until it is, this probe can issue up to five unrecorded physical requests
/// against a live credential. The list is asserted to match the tree exactly,
/// so the gap cannot quietly widen.
const KNOWN_UNADMITTED_PROVIDER_CALLS: [&str; 1] = ["provider_qualification.rs"];

/// Files permitted to talk to a non-provider HTTP service.
///
/// These are separate concerns — an MCP control plane, a worker API, an OIDC
/// endpoint, model discovery — that do not spend a model credential and are
/// therefore outside this boundary. Listing them explicitly means a *new*
/// network caller has to be justified in a diff rather than blending in.
const NON_PROVIDER_HTTP_ALLOWLIST: [&str; 7] = [
    "mcp_control.rs",
    "mcp_control_client.rs",
    "external_worker.rs",
    "auth_store.rs",
    "provider_discovery.rs",
    "provider_qualification.rs",
    "host_helpers.rs",
];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let entry = entry.expect("read directory entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Strip `#[cfg(test)]` modules so a test's own fake server is not mistaken
/// for a production network caller.
fn without_test_modules(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            // Skip the attribute and, if the next line opens a module, the
            // whole braced body.
            if let Some(next) = lines.peek() {
                if next.contains("mod ") && next.trim_end().ends_with('{') {
                    lines.next();
                    let mut depth = 1usize;
                    for body in lines.by_ref() {
                        depth += body.matches('{').count();
                        depth -= body.matches('}').count().min(depth);
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// Only the boundary may address a chat-completions endpoint.
#[test]
fn only_the_send_boundary_builds_a_provider_chat_request() {
    let mut offenders = BTreeSet::new();
    let mut known = BTreeSet::new();
    for path in rust_sources(&source_root()) {
        let name = file_name(&path);
        if PROVIDER_CALL_ALLOWLIST.contains(&name.as_str()) {
            continue;
        }
        if KNOWN_UNADMITTED_PROVIDER_CALLS.contains(&name.as_str()) {
            known.insert(name);
            continue;
        }
        let source = without_test_modules(&std::fs::read_to_string(&path).expect("read source"));
        for (index, line) in source.lines().enumerate() {
            // The endpoint path is the tell: a chat completion is the only
            // request that spends a model credential.
            if line.contains("chat/completions") && !line.trim_start().starts_with("//") {
                offenders.insert(format!("{name}:{}", index + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these files address a provider chat endpoint outside the admitted send boundary \
         ({PROVIDER_CALL_ALLOWLIST:?}): {offenders:?}. \
         Route the request through provider_transport::send_admitted so it is admitted, \
         sealed, and recorded."
    );
    // The debt list must describe the tree exactly: an entry that no longer
    // bypasses the boundary should be deleted, not left standing as if the
    // gap were still open.
    let expected: BTreeSet<String> = KNOWN_UNADMITTED_PROVIDER_CALLS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        known, expected,
        "the known-unadmitted list no longer matches the tree; \
         remove an entry that is fixed, and never add one without fixing or justifying it"
    );
}

/// A new HTTP caller has to be declared, so one cannot appear unnoticed.
#[test]
fn every_http_caller_in_this_crate_is_accounted_for() {
    let mut undeclared = BTreeSet::new();
    for path in rust_sources(&source_root()) {
        let name = file_name(&path);
        if PROVIDER_CALL_ALLOWLIST.contains(&name.as_str())
            || NON_PROVIDER_HTTP_ALLOWLIST.contains(&name.as_str())
        {
            continue;
        }
        let source = without_test_modules(&std::fs::read_to_string(&path).expect("read source"));
        if source.contains("reqwest::Client::builder") || source.contains("reqwest::Client::new") {
            undeclared.insert(name);
        }
    }
    assert!(
        undeclared.is_empty(),
        "these files build an HTTP client but are on neither allowlist: {undeclared:?}. \
         If the request spends a model credential it belongs behind \
         provider_transport::send_admitted; if it does not, add it to \
         NON_PROVIDER_HTTP_ALLOWLIST with a reason."
    );
}

/// The idempotency key must be transmitted, not merely recorded.
#[test]
fn the_send_boundary_transmits_the_idempotency_key() {
    let transport = std::fs::read_to_string(source_root().join("provider_transport.rs"))
        .expect("read the send boundary");
    assert!(
        transport.contains("IDEMPOTENCY_HEADER"),
        "the boundary no longer names an idempotency header"
    );
    assert!(
        transport.contains(".header(IDEMPOTENCY_HEADER"),
        "the idempotency key is recorded but never put on the wire"
    );
    // And the ordering that makes the record truthful.
    let begin = transport
        .find("call.begin_send(&attempt)")
        .expect("the boundary records the send boundary");
    let send = transport
        .find("request.send()")
        .expect("the boundary sends the request");
    assert!(
        begin < send,
        "the attempt is marked sending after the request has already left"
    );
    let open = transport
        .find("call.open_attempt()")
        .expect("the boundary opens an attempt");
    assert!(
        open < begin,
        "the attempt is opened after it is marked sending"
    );
}

/// The transport must send the admitted bytes, not re-serialize a structure
/// that may since have changed.
#[test]
fn the_send_boundary_transmits_the_admitted_bytes() {
    let transport = std::fs::read_to_string(source_root().join("provider_transport.rs"))
        .expect("read the send boundary");
    assert!(
        transport.contains("request.body(call.body().to_vec())"),
        "the boundary no longer sends the exact admitted bytes"
    );
    assert!(
        transport.contains("call.verify_intact()"),
        "the boundary no longer checks the bytes against the digest it will claim"
    );
    // `.json(` would re-serialize a value rather than send the sealed bytes.
    assert!(
        !transport.contains(".json(&body)"),
        "the boundary re-serializes a body instead of sending the sealed bytes"
    );
}
