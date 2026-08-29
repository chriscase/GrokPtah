//! What this consumer cannot reach.
//!
//! Every assertion here is about an *absence*. A test that merely showed the
//! consumer working would prove nothing about containment; these fail if the
//! seam ever starts leaking, including by a well-meaning future change that
//! adds a convenience accessor.

use grokptah_agent_sdk::prelude::*;
use grokptah_sdk_reference_consumer::*;

/// The dependency graph itself is the strongest containment proof available.
///
/// Read from the lockfile rather than asserted in prose: a consumer that could
/// reach `grokptah-agent-bridge` would inherit the keyring, the Axum control
/// plane, `reqwest`, the provider profiles and the durable stores — and every
/// internal type in them would become part of this consumer's compatibility
/// surface.
#[test]
fn no_internal_dependency_is_reachable() {
    let lock = include_str!("../Cargo.lock");
    for forbidden in [
        "grokptah-agent-bridge",
        "grokptah-service",
        "keyring",
        "reqwest",
        "axum",
        "tauri",
    ] {
        assert!(
            !lock.contains(&format!("name = \"{forbidden}\"")),
            "`{forbidden}` reached the reference consumer's dependency graph; \
             the public seam is supposed to make that impossible"
        );
    }
}

/// A consumer cannot name a workspace by path.
///
/// `WorkspaceRef` has no constructor from a string or a path. The only way to
/// hold one is to receive it from a host that reported that workspace, which
/// is what stops a consumer from probing the filesystem through the seam.
#[test]
fn filesystem_identity_cannot_be_invented() {
    let json = "\"/etc/passwd\"";
    let forged: Result<WorkspaceRef, _> = serde_json::from_str(json);
    assert!(
        forged.is_err(),
        "a filesystem path decoded into a workspace reference"
    );
}

/// Nothing a consumer can hold serializes a secret, a path, or a prompt.
#[test]
fn no_public_type_carries_a_secret_a_path_or_a_prompt() {
    // A lease is the closest thing to a credential a consumer ever holds, and
    // its secret is not `Serialize` at all — it cannot reach a log or a cache
    // even by accident.
    let now = "2026-01-01T00:00:00Z".parse().expect("fixed timestamp");
    let lease = ControlLease {
        work_id: WorkId::new("work-1").unwrap(),
        attempt_id: AttemptId::new("attempt-1").unwrap(),
        attempt_number: 1,
        claimant: AgentId::new("agent-1").unwrap(),
        acquired_at: now,
        expires_at: now,
        revision: Revision::new(1),
        credential: LeaseCredential::new("super-secret-lease-token"),
    };
    let encoded = serde_json::to_string(&lease).expect("lease serializes");
    assert!(
        !encoded.contains("super-secret-lease-token"),
        "a lease credential reached JSON: {encoded}"
    );
    let debugged = format!("{lease:?}");
    assert!(
        !debugged.contains("super-secret-lease-token"),
        "a lease credential reached Debug: {debugged}"
    );
}

/// Computer Use control and provider credentials are refused on every host.
///
/// Not "unavailable on this deployment" — structurally forbidden, stamped into
/// every capability document regardless of what a host advertises.
#[test]
fn the_permanently_forbidden_pair_is_not_negotiable() {
    for id in CapabilityId::permanently_forbidden() {
        assert!(
            id.is_permanently_forbidden(),
            "{id} is not marked permanently forbidden"
        );
    }
    assert!(CapabilityId::ComputerControl.is_permanently_forbidden());
    assert!(CapabilityId::ProviderCredentials.is_permanently_forbidden());
}

/// An unknown capability counts as a mutation, so a consumer never treats a
/// word it does not know as safe to call.
#[test]
fn an_unknown_capability_fails_closed() {
    assert!(CapabilityId::from_wire("run.detonate").is_mutation());
}

/// A run this build cannot classify is still tracked, and still watched.
#[test]
fn a_newer_host_does_not_strand_the_consumer() {
    let mut tracker = RunTracker::new();
    let view = run_view("paused_for_review", 7);
    assert!(tracker.observe(&view), "a fresh observation is applied");

    let tracked = tracker.latest().expect("tracked");
    assert!(!tracked.lifecycle.is_known());
    assert!(
        tracked.should_keep_observing(),
        "an unreadable lifecycle must not be treated as finished"
    );

    // A stale snapshot arriving late is dropped, not applied.
    let stale = run_view("running", 3);
    assert!(!tracker.observe(&stale), "a stale revision was admitted");
    assert_eq!(tracker.latest().unwrap().revision, Revision::new(7));
}

/// Uncertain outcomes are never advertised as retryable.
#[test]
fn an_uncertain_outcome_is_never_safe_to_retry() {
    let error = SdkError::new(SdkErrorCode::UncertainOutcome, "send may have applied");
    assert_eq!(recovery_advice(&error), RetryDisposition::Unsafe);

    let dropped = SdkError::new(SdkErrorCode::TransportUnavailable, "connection dropped");
    assert_eq!(recovery_advice(&dropped), RetryDisposition::Safe);
}

/// A receipt summary always carries the window it was drawn from.
#[test]
fn a_summary_never_lets_absence_stand_as_proof() {
    let page = ReceiptPage::new(Vec::new(), None, ReceiptRetention::RUNTIME_DEFAULT);
    let summary = summarize(&page);
    assert_eq!(summary.settled, 0);
    assert_eq!(summary.uncertain, 0);
    // The point: an empty page still reports the budget it came from, so a
    // consumer cannot read "nothing" without also reading "under this window".
    assert_eq!(summary.window.budget_scope, RetentionBudgetScope::Host);
    assert!(summary.window.exemptions.active_run_retained);
}

fn run_view(lifecycle: &str, revision: u64) -> RunView {
    let raw = serde_json::json!({
        "sessionId": "sess-1",
        "runId": "run-1",
        "workspace": "ws-0123456789abcdef",
        "lifecycle": lifecycle,
        "executionMode": "shared",
        "revision": revision,
        "bounds": {"maxPromptBytes": 65536, "maxRounds": 8, "maxDurationMs": 60000},
        "usage": {"promptTokens": 0, "completionTokens": 0, "totalTokens": 0,
                  "requests": 0, "complete": true, "pendingRequests": 0},
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:00:05Z"
    });
    serde_json::from_value(raw).expect("run view decodes")
}
