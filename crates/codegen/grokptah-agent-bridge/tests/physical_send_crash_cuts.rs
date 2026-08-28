//! Crash cuts taken *through* the physical send path.
//!
//! [`provider_attempt_crash_cuts`] cuts the ledger directly. This suite cuts
//! the thing that drives it: a synthetic transport that makes exactly the
//! calls the real HTTP client makes, in the same order, against the real
//! [`OrchStore`]. That is what makes these cuts meaningful — the sequence
//! under test is the production sequence, and only the socket is fake.
//!
//! No credential is resolved, no endpoint is contacted, and nothing here
//! depends on a clock or a scheduler: each cut is a deterministic choice of
//! where to stop.
//!
//! The question every cut asks is the expensive one: after this instant, may
//! the host send the same request again on its own?

use grokptah_agent_bridge::attempt_binding_testkit as binding;
use grokptah_agent_bridge::orchestration::OrchStore;
use grokptah_agent_bridge::physical_send_testkit as wire;
use grokptah_agent_sdk::account::{AccountReference, AccountReferenceSource, CredentialMethod};
use grokptah_agent_sdk::attempt::{
    AttemptIntent, AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId, ProviderAttempt,
    SendState,
};
use grokptah_agent_sdk::launch::{
    BaseCategory, ModelReference, ProviderClass, RequestDialect, RouteClass,
};
use tempfile::{tempdir, TempDir};

fn bounded(value: &str) -> BoundedId {
    BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
}

/// One attempt bound to `dialect`, so the wire-key rule can be exercised on
/// both a dialect that publishes an idempotency contract and one that does not.
fn attempt(run_id: &str, ordinal: u32, dialect: RequestDialect) -> ProviderAttempt {
    ProviderAttempt::open(
        bounded(&format!("att-{run_id}-{ordinal}")),
        bounded(run_id),
        ordinal,
        AttemptSubject {
            principal: Some(bounded("prn-0a1b2c3d")),
            tenant: None,
            project: None,
            workspace: bounded("wsp:0a1b2c3d"),
            session: bounded("ses:4e5f6a7b"),
        },
        AuthorityRevisions {
            auth: grokptah_agent_sdk::attempt::Revision(1),
            policy: grokptah_agent_sdk::attempt::Revision(1),
            capability: grokptah_agent_sdk::attempt::Revision(1),
            credential: grokptah_agent_sdk::attempt::Revision(1),
        },
        AttemptRoute {
            provider: ProviderClass::Xai,
            profile: Some(bounded("xai")),
            credential_method: CredentialMethod::GrokBuildOidc,
            route: RouteClass::XaiFirstParty,
            base: BaseCategory::XaiOfficial,
            dialect,
            model: ModelReference::new("grok-4").expect("bounded model"),
            effort: Some(bounded("high")),
            account_reference: AccountReference::new(
                "usr-0a1b2c3d",
                AccountReferenceSource::UserId,
            ),
        },
        AttemptIntent {
            digest: bounded("sha256:0a1b2c3d"),
            request_id: bounded("req-0001"),
            provider_idempotency_key: binding::provider_idempotency_key(run_id, ordinal),
        },
    )
}

/// Reopen the ledger the way a restarted process would.
///
/// `OrchStore::open` runs the interrupted-attempt sweep, so this is also what
/// exercises crash recovery rather than a test-only shortcut.
fn restart(home: &TempDir, previous: Option<OrchStore>) -> OrchStore {
    drop(previous);
    OrchStore::open(home.path().join("orch")).expect("reopen the ledger")
}

fn open_ledger(home: &TempDir, run_id: &str, dialect: RequestDialect) -> OrchStore {
    let store = OrchStore::open(home.path().join("orch")).expect("open the ledger");
    store
        .open_attempt(&attempt(run_id, 1, dialect))
        .expect("record the attempt before anything can be sent");
    store
}

fn state(store: &OrchStore, run_id: &str) -> SendState {
    store
        .list_attempts_for_run(run_id)
        .expect("read the ledger")
        .first()
        .expect("exactly one attempt per run in these cuts")
        .send_state
}

fn only(store: &OrchStore, run_id: &str) -> ProviderAttempt {
    store
        .list_attempts_for_run(run_id)
        .expect("read the ledger")
        .into_iter()
        .next()
        .expect("exactly one attempt per run in these cuts")
}

/// What the synthetic transport observed while it held the binding.
#[derive(Debug, Default)]
struct Observed {
    /// Whether a durable attempt was bound at all.
    bound: bool,
    /// The `Idempotency-Key` value the request would have carried.
    wire_key: Option<String>,
}

/// Where the host dies relative to one physical request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// Killed with the request prepared but the boundary not yet crossed.
    BeforeSend,
    /// The bytes left; no reply ever came back.
    AfterWireBeforeResponse,
    /// A reply began and the body stopped arriving part-way through.
    Streaming,
    /// A complete, parsed reply.
    Settled,
}

/// A synthetic provider transport.
///
/// Reads the binding and drives the durable record through exactly the calls
/// `call_xai_agent_step` makes, in the same order: read the wire key,
/// `mark_sending` immediately before the socket write, `mark_sent` on a
/// response, `mark_responding` when the body starts, `mark_uncertain` when the
/// request left and nothing came back.
async fn synthetic_send(cut: Cut, provider_request_id: Option<&str>) -> Observed {
    let observed = Observed {
        bound: wire::is_bound(),
        wire_key: wire::wire_idempotency_key(),
    };
    if cut == Cut::BeforeSend {
        return observed;
    }
    wire::mark_sending();
    // The request is on the wire from here.
    if cut == Cut::AfterWireBeforeResponse {
        wire::mark_uncertain();
        return observed;
    }
    wire::mark_sent(provider_request_id);
    if cut == Cut::Streaming {
        return observed;
    }
    wire::mark_responding();
    observed
}

/// Run one physical request under this run's binding, cut at `cut`.
async fn send_under_binding(
    store: &OrchStore,
    run_id: &str,
    cut: Cut,
    provider_request_id: Option<&str>,
) -> Observed {
    let bound = binding::send_binding(store, run_id);
    wire::scope_optional(bound, synthetic_send(cut, provider_request_id)).await
}

// ---------------------------------------------------------------------------

/// Cut: killed before the send boundary is crossed at all.
///
/// The record still says the request never left, which is the only state that
/// may be repeated without asking anyone.
#[tokio::test]
async fn a_cut_before_the_send_boundary_leaves_a_repeatable_request() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0001";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    // The turn ran but never reached a socket — a slash command, or a session
    // with no resolvable credential. `mark_sending` was never called.
    send_under_binding(&store, run, Cut::BeforeSend, None).await;
    let store = restart(&home, Some(store));

    assert_eq!(state(&store, run), SendState::KnownNotSent);
    assert!(only(&store, run).may_auto_retry());
    assert!(store.run_permits_new_attempt(run).expect("read the ledger"));
    // Nothing was dispatched, so nothing could have carried a key.
    assert!(!only(&store, run).receipts.acknowledged());
}

/// Cut: the bytes left this host and no reply ever came back.
///
/// The request may or may not have executed. That is exactly `uncertain`, and
/// it must survive the restart rather than being cleaned up into something
/// retryable.
#[tokio::test]
async fn a_cut_after_the_wire_but_before_a_reply_is_never_auto_retried() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0002";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    let observed = send_under_binding(&store, run, Cut::AfterWireBeforeResponse, None).await;
    assert!(observed.bound, "the transport must see a durable attempt");

    let store = restart(&home, Some(store));

    assert_eq!(state(&store, run), SendState::Uncertain);
    assert!(!only(&store, run).may_auto_retry());
    assert!(!store.run_permits_new_attempt(run).expect("read the ledger"));
    assert_eq!(store.unreconciled_attempts(run).expect("read").len(), 1);
}

/// Cut: a reply began and then the stream stopped part-way through.
///
/// The provider demonstrably received the request, so this is not a delivery
/// ambiguity — and it is emphatically not repeatable.
#[tokio::test]
async fn a_cut_mid_stream_still_blocks_an_equivalent_request() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0003";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    send_under_binding(&store, run, Cut::Streaming, Some("req-provider-77")).await;
    let store = restart(&home, Some(store));

    assert_eq!(state(&store, run), SendState::Sent);
    assert!(!only(&store, run).may_auto_retry());
    assert!(!store.run_permits_new_attempt(run).expect("read the ledger"));
    // The receipt is the provider's own identifier, never this host's key.
    let recorded = only(&store, run);
    assert_eq!(
        recorded.receipts.request.as_ref().map(BoundedId::as_str),
        Some("req-provider-77")
    );
    assert_ne!(
        recorded.receipts.request.as_ref(),
        Some(&recorded.intent.provider_idempotency_key),
        "this host's idempotency key is not evidence the provider issued a receipt"
    );
}

/// A complete reply settles, and only then may an equivalent request follow.
#[tokio::test]
async fn a_settled_send_is_terminal_and_releases_the_run() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0004";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    send_under_binding(&store, run, Cut::Settled, Some("req-provider-88")).await;
    assert_eq!(state(&store, run), SendState::Responding);

    binding::settle_run(&store, run, true, None);
    let store = restart(&home, Some(store));

    assert_eq!(state(&store, run), SendState::Settled);
    assert!(only(&store, run).send_state.is_terminal());
    assert!(store.run_permits_new_attempt(run).expect("read the ledger"));
}

/// A restart does not invent a different key for the same attempt.
///
/// The whole reconciliation story depends on this: an operator asking the
/// provider "did you already run this?" must be able to ask about the same key
/// the crashed process would have sent.
#[tokio::test]
async fn a_restart_reproduces_the_same_wire_key_for_the_same_attempt() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0005";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    let before = send_under_binding(&store, run, Cut::AfterWireBeforeResponse, None).await;
    let store = restart(&home, Some(store));
    // The key the crashed process would have presented is still derivable
    // from the record itself, which is what makes reconciliation possible.
    let recorded = only(&store, run);
    assert_eq!(state(&store, run), SendState::Uncertain);

    let key = before.wire_key.expect("an xAI attempt carries its key");
    assert_eq!(key, recorded.intent.provider_idempotency_key.as_str());
    assert_eq!(
        key,
        binding::provider_idempotency_key(run, 1).as_str(),
        "the key is derived from the run and ordinal, not from anything transient"
    );
}

/// Two dispatches of one attempt present the identical key.
///
/// Without this a "retry" would be a fresh request as far as the provider is
/// concerned, and the recorded key would be evidence of nothing.
#[tokio::test]
async fn a_duplicate_dispatch_presents_the_identical_idempotency_key() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0006";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    let first = send_under_binding(&store, run, Cut::BeforeSend, None).await;
    let second = send_under_binding(&store, run, Cut::BeforeSend, None).await;

    assert!(first.wire_key.is_some());
    assert_eq!(first.wire_key, second.wire_key);
}

/// A dialect that publishes no idempotency contract gets no key on the wire —
/// and is still never retried automatically.
///
/// The two facts have to move independently. Sending the header anyway would
/// claim a deduplication the gateway never promised; treating the missing
/// header as "unbound" would hand that same gateway the retry loop, which is
/// the more expensive of the two mistakes.
#[tokio::test]
async fn an_unsupported_dialect_carries_no_key_but_still_blocks_a_retry() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0007";
    let store = open_ledger(&home, run, RequestDialect::OpenAiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    let observed = send_under_binding(&store, run, Cut::AfterWireBeforeResponse, None).await;

    assert!(
        observed.bound,
        "an attempt on any dialect is bound, so the client stands down from retrying"
    );
    assert_eq!(
        observed.wire_key, None,
        "a compatible gateway publishes no idempotency contract to carry a key under"
    );
    // The key is still recorded, so reconciliation remains possible by hand.
    assert_eq!(
        only(&store, run).intent.provider_idempotency_key.as_str(),
        binding::provider_idempotency_key(run, 1).as_str()
    );

    let store = restart(&home, Some(store));
    assert_eq!(state(&store, run), SendState::Uncertain);
    assert!(!store.run_permits_new_attempt(run).expect("read the ledger"));
}

/// Nothing bound means nothing claimed.
///
/// An unbound send reads no key and reports itself unbound, which is what the
/// HTTP client's retry guards key off. This is the state every desktop send
/// was in before it was routed through the attempt lattice.
#[tokio::test]
async fn an_unbound_send_reads_no_binding_and_no_key() {
    let observed = wire::scope_optional(None, synthetic_send(Cut::Settled, Some("req-x"))).await;
    assert!(!observed.bound);
    assert_eq!(observed.wire_key, None);
}

/// The one value this change puts on the wire is opaque.
///
/// `launch_admission` already pins that a recorded attempt leaks nothing. The
/// new surface here is narrower and needs the same guarantee: the header a
/// provider now receives, and the key any status read can publish, must carry
/// no prompt, no workspace path, and nothing credential-shaped.
#[tokio::test]
async fn the_wire_key_and_the_record_publish_nothing_secret() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0008";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    let observed = send_under_binding(&store, run, Cut::Settled, Some("req-provider-99")).await;
    let key = observed.wire_key.expect("an xAI attempt carries its key");

    // Derived from the run and ordinal only, and shaped as an opaque handle.
    assert!(key.starts_with("idem:"), "unexpected key shape: {key}");
    assert!(BoundedId::new(&key).is_some(), "the key must stay bounded");

    let encoded = serde_json::to_string(&only(&store, run)).expect("encode the record");
    for needle in [
        "Bearer",
        "refresh_token",
        "apiKey",
        "https://",
        "/tmp",
        "balance",
        "quota",
    ] {
        assert!(
            !encoded.contains(needle) && !key.contains(needle),
            "the send surface leaked {needle:?}"
        );
    }

    // The public projection of the state is the closed vocabulary, not a
    // free-form string a UI could be asked to render.
    let public = grokptah_agent_bridge::orchestration::public_send_state(state(&store, run));
    assert_eq!(
        serde_json::to_string(&public).expect("encode the projection"),
        "\"responding\""
    );
}

/// A turn that never reaches a socket is never recorded as a send.
///
/// Not every turn is a request: a slash command answers locally, a session
/// with no resolvable credential answers with an error, an offline host stubs
/// the whole turn. Marking `sending` when the *turn* starts rather than when
/// the *request* does would record all of those as dispatched — and the
/// lattice is one-way, so the claim could never be withdrawn. Worse, the
/// settlement would then read the turn's success as a provider
/// acknowledgement and manufacture a receipt for a request nobody made.
#[tokio::test]
async fn a_turn_that_never_dispatches_is_not_recorded_as_sent() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0009";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    // The turn ran to a successful local answer without touching a provider.
    send_under_binding(&store, run, Cut::BeforeSend, None).await;
    binding::settle_run(&store, run, true, None);

    let recorded = only(&store, run);
    assert_eq!(
        recorded.send_state,
        SendState::KnownNotSent,
        "a turn that never dispatched must still say so"
    );
    assert!(
        !recorded.receipts.acknowledged(),
        "no provider answered, so no receipt may exist"
    );
    // And because it provably never left, it stays freely repeatable.
    assert!(recorded.may_auto_retry());
    assert!(store.run_permits_new_attempt(run).expect("read the ledger"));

    let store = restart(&home, Some(store));
    assert_eq!(state(&store, run), SendState::KnownNotSent);
}

/// A turn may succeed while its request was never reported on. That does not
/// make the request acknowledged.
///
/// A Chat turn renders a failed model call as its reply and returns success,
/// so turn success is not evidence about the wire. Settling `sending` as
/// `sent` on that basis would record a provider acknowledgement — and a
/// receipt — for a request that may never have arrived, and the lattice
/// cannot take it back.
#[tokio::test]
async fn a_successful_turn_does_not_acknowledge_an_unreported_request() {
    let home = tempdir().expect("temp home");
    let run = "run-wire-0010";
    let store = open_ledger(&home, run, RequestDialect::XaiChatCompletions);
    binding::admit_send(&store, run).expect("admit the send");

    // The bytes went out; the transport never got to report on them.
    send_under_binding(&store, run, Cut::AfterWireBeforeResponse, None).await;
    // Rewind the transport's own verdict so the cut under test is precisely
    // "still `sending` when the turn ended".
    let store = restart(&home, Some(store));
    assert_eq!(state(&store, run), SendState::Uncertain);

    binding::settle_run(&store, run, true, None);
    let recorded = only(&store, run);
    assert!(
        !matches!(recorded.send_state, SendState::Sent),
        "a request nobody reported on was not acknowledged"
    );
    assert_eq!(recorded.send_state, SendState::Settled);
}
