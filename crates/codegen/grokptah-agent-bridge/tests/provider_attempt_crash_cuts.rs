//! Crash cuts around the provider send boundary.
//!
//! Each test kills the host at a different instant relative to dispatch and
//! then asks the durable record the only question that matters on restart:
//! **is it safe to send this again?** A wrong answer here is not a lost turn,
//! it is a duplicate charge and a duplicate set of side effects.
//!
//! The cuts are taken against the real [`OrchStore`], not a mock, because the
//! record on disk is what a restarted process actually reads.

use grokptah_agent_bridge::attempt_binding_testkit as binding;
use grokptah_agent_bridge::orchestration::OrchStore;
use grokptah_agent_sdk::account::{AccountReference, AccountReferenceSource, CredentialMethod};
use grokptah_agent_sdk::attempt::{
    AttemptIntent, AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId, ProviderAttempt,
    ProviderReceipts, Revision, SendState, UsageReceipt,
};
use grokptah_agent_sdk::launch::{
    BaseCategory, ModelReference, ProviderClass, RequestDialect, RouteClass,
};
use grokptah_agent_sdk::outcome::RunFailureKind;
use tempfile::{tempdir, TempDir};

fn bounded(value: &str) -> BoundedId {
    BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
}

fn attempt(run_id: &str, ordinal: u32) -> ProviderAttempt {
    ProviderAttempt::open(
        bounded(&format!("att-{run_id}-{ordinal}")),
        bounded(run_id),
        ordinal,
        AttemptSubject {
            principal: Some(bounded("prn-0a1b2c3d")),
            tenant: Some(bounded("tnt-9z8y")),
            project: Some(bounded("prj-alpha")),
            workspace: bounded("wsp:0a1b2c3d"),
            session: bounded("ses:4e5f6a7b"),
        },
        AuthorityRevisions {
            auth: Revision(7),
            policy: Revision(3),
            capability: Revision(11),
            credential: Revision(2),
        },
        AttemptRoute {
            provider: ProviderClass::Xai,
            profile: Some(bounded("xai")),
            credential_method: CredentialMethod::GrokBuildOidc,
            route: RouteClass::XaiFirstParty,
            base: BaseCategory::XaiOfficial,
            dialect: RequestDialect::XaiChatCompletions,
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

/// Reopen the ledger the way a restarted process would, so every assertion is
/// about what is actually on disk.
///
/// The store holds an exclusive file lock, so the previous handle is consumed
/// and dropped first — which is what a real process restart does anyway.
fn restart(home: &TempDir, previous: Option<OrchStore>) -> OrchStore {
    drop(previous);
    OrchStore::open(home.path().join("orch")).expect("reopen the ledger")
}

/// Cut 1: the host dies after the attempt is recorded but before dispatch.
/// Nothing left, so a fresh send is safe.
#[test]
fn a_crash_before_dispatch_leaves_the_attempt_safely_retryable() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    store.open_attempt(&attempt("run-0001", 1)).unwrap();
    let store = restart(&home, Some(store));
    let recovered = store.list_attempts_for_run("run-0001").unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].send_state, SendState::KnownNotSent);
    assert!(recovered[0].may_auto_retry());
    assert!(store.run_permits_new_attempt("run-0001").unwrap());
    assert!(store.unreconciled_attempts("run-0001").unwrap().is_empty());
}

/// Cut 2: the host dies *during* dispatch. This is the case the whole state
/// machine exists for — the request may have arrived, so it must not be
/// repeated on this host's own initiative.
#[test]
fn a_crash_during_dispatch_is_never_auto_retried() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let opened = attempt("run-0002", 1);
    store.open_attempt(&opened).unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
        .unwrap()
        .expect("the attempt exists");
    // The process dies here, mid-flight.
    let store = restart(&home, Some(store));
    let recovered = store.list_attempts_for_run("run-0002").unwrap();
    assert_eq!(recovered[0].send_state, SendState::Sending);
    assert!(
        !recovered[0].may_auto_retry(),
        "an interrupted send was treated as retryable"
    );
    assert!(
        !store.run_permits_new_attempt("run-0002").unwrap(),
        "an equivalent request was allowed while the first is unreconciled"
    );
    let unreconciled = store.unreconciled_attempts("run-0002").unwrap();
    assert_eq!(unreconciled.len(), 1);
    // Recovery gets the exact key it needs to ask the provider what happened.
    assert_eq!(
        unreconciled[0].intent.provider_idempotency_key,
        binding::provider_idempotency_key("run-0002", 1)
    );
}

/// A restart must not be able to "reset" an interrupted send into a retryable
/// one, whatever code path tries it.
#[test]
fn the_durable_record_refuses_to_rewind_an_interrupted_send() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let opened = attempt("run-0003", 1);
    store.open_attempt(&opened).unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
        .unwrap();

    for forbidden in [SendState::KnownNotSent] {
        let outcome = store.update_attempt(opened.attempt_id.as_str(), |attempt| {
            // Bypass `advance` entirely: a caller could just assign the field.
            attempt.send_state = forbidden;
            Ok(())
        });
        assert!(outcome.is_err(), "{forbidden:?} rewind was accepted");
    }
    // And the record on disk is untouched by the rejected write.
    let recovered = restart(&home, Some(store))
        .list_attempts_for_run("run-0003")
        .unwrap();
    assert_eq!(recovered[0].send_state, SendState::Sending);
}

/// The binding is what makes a drift detectable after the fact, so it is
/// write-once for the life of the attempt.
#[test]
fn an_attempts_binding_cannot_be_rewritten_after_it_is_opened() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let opened = attempt("run-0004", 1);
    store.open_attempt(&opened).unwrap();

    let rewrites: Vec<(&str, Box<dyn Fn(&mut ProviderAttempt)>)> = vec![
        (
            "model",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.route.model = ModelReference::new("grok-3").expect("bounded");
            }),
        ),
        (
            "provider",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.route.provider = ProviderClass::OpenAiCompatible;
            }),
        ),
        (
            "account",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.route.account_reference =
                    AccountReference::new("usr-someone-else", AccountReferenceSource::UserId);
            }),
        ),
        (
            "policy revision",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.authority.policy = Revision(99);
            }),
        ),
        (
            "workspace",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.subject.workspace = BoundedId::new("wsp:deadbeef").expect("bounded");
            }),
        ),
        (
            "idempotency key",
            Box::new(|attempt: &mut ProviderAttempt| {
                attempt.intent.provider_idempotency_key =
                    BoundedId::new("idem:rewritten").expect("bounded");
            }),
        ),
    ];
    for (name, rewrite) in rewrites {
        let outcome = store.update_attempt(opened.attempt_id.as_str(), |attempt| {
            rewrite(attempt);
            Ok(())
        });
        assert!(outcome.is_err(), "{name} rewrite was accepted");
    }
    let recovered = restart(&home, Some(store))
        .list_attempts_for_run("run-0004")
        .unwrap();
    assert_eq!(recovered[0], opened, "a rejected rewrite half-applied");
}

/// Cut 3: the reply arrives but cannot be read. The request may well have run,
/// so the honest record is `uncertain` and it stays unreconciled.
#[test]
fn an_unreadable_reply_settles_as_uncertain_and_blocks_an_equivalent_request() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let opened = attempt("run-0005", 1);
    store.open_attempt(&opened).unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
        .unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Uncertain)
                .map_err(anyhow::Error::msg)?;
            attempt.failure = Some(RunFailureKind::MalformedOutput);
            Ok(())
        })
        .unwrap();

    let store = restart(&home, Some(store));
    let recovered = store.list_attempts_for_run("run-0005").unwrap();
    assert_eq!(recovered[0].send_state, SendState::Uncertain);
    assert_eq!(recovered[0].failure, Some(RunFailureKind::MalformedOutput));
    assert!(!recovered[0].may_auto_retry());
    assert!(!store.run_permits_new_attempt("run-0005").unwrap());
    // And it never claims success.
    assert!(!RunFailureKind::MalformedOutput.verdict().claims_success());
}

/// Cut 4: the provider answered. The attempt is finished, so a *new* intent
/// may proceed — what is forbidden is repeating this one blindly.
#[test]
fn an_acknowledged_attempt_is_finished_and_stops_blocking_the_run() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let opened = attempt("run-0006", 1);
    store.open_attempt(&opened).unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
        .unwrap();
    store
        .update_attempt(opened.attempt_id.as_str(), |attempt| {
            attempt.receipts = ProviderReceipts {
                request: Some(BoundedId::new("prq-abc123").expect("bounded")),
                run: None,
                usage: Some(UsageReceipt {
                    input_tokens: 1_200,
                    output_tokens: 340,
                }),
                provider_replied: true,
            };
            attempt.advance(SendState::Sent).map_err(anyhow::Error::msg)
        })
        .unwrap();

    let store = restart(&home, Some(store));
    let recovered = store.list_attempts_for_run("run-0006").unwrap();
    assert_eq!(recovered[0].send_state, SendState::Sent);
    assert!(
        !recovered[0].may_auto_retry(),
        "a delivered request was re-sent"
    );
    assert!(store.run_permits_new_attempt("run-0006").unwrap());
    assert!(store.unreconciled_attempts("run-0006").unwrap().is_empty());
    // The usage is a count and nothing else.
    let usage = recovered[0].receipts.usage.expect("usage was recorded");
    assert_eq!(usage.input_tokens, 1_200);
    assert_eq!(usage.output_tokens, 340);
}

/// One unreconciled attempt poisons the whole run, not just itself: a second
/// ordinal must not sneak past while the first is unresolved.
#[test]
fn one_unreconciled_attempt_blocks_every_later_attempt_on_that_run() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let first = attempt("run-0007", 1);
    store.open_attempt(&first).unwrap();
    store
        .update_attempt(first.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
        .unwrap();
    store.open_attempt(&attempt("run-0007", 2)).unwrap();

    let store = restart(&home, Some(store));
    assert!(
        !store.run_permits_new_attempt("run-0007").unwrap(),
        "a later ordinal was allowed to send while an earlier one is unresolved"
    );
    let recovered = store.list_attempts_for_run("run-0007").unwrap();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[0].ordinal, 1, "attempts order by ordinal");
    assert_eq!(recovered[1].ordinal, 2);
    // Each ordinal carries its own provider idempotency key, so reconciling
    // one cannot be mistaken for reconciling the other.
    assert_ne!(
        recovered[0].intent.provider_idempotency_key,
        recovered[1].intent.provider_idempotency_key
    );
}

/// The idempotency key is derived, not random, so a host that crashes and
/// re-reads its own record produces the identical key.
#[test]
fn the_provider_idempotency_key_is_reproducible_across_a_restart() {
    let first = binding::provider_idempotency_key("run-0008", 1);
    let again = binding::provider_idempotency_key("run-0008", 1);
    assert_eq!(first, again, "the key is not reproducible");
    assert_ne!(
        first,
        binding::provider_idempotency_key("run-0008", 2),
        "two ordinals share one key"
    );
    assert_ne!(
        first,
        binding::provider_idempotency_key("run-0009", 1),
        "two runs share one key"
    );
    // And it carries no run identity in the clear.
    assert!(!first.as_str().contains("run-0008"));
}

/// Reconciliation moves an interrupted send to `uncertain` and leaves every
/// other state exactly where it was.
#[test]
fn reconciliation_only_touches_an_interrupted_send() {
    let mut sending = attempt("run-0010", 1);
    sending.advance(SendState::Sending).unwrap();
    binding::reconcile_interrupted(&mut sending).unwrap();
    assert_eq!(sending.send_state, SendState::Uncertain);

    let mut unsent = attempt("run-0010", 2);
    binding::reconcile_interrupted(&mut unsent).unwrap();
    assert_eq!(
        unsent.send_state,
        SendState::KnownNotSent,
        "a request that never left was needlessly made uncertain"
    );

    let mut uncertain = attempt("run-0010", 3);
    uncertain.advance(SendState::Sending).unwrap();
    uncertain.advance(SendState::Uncertain).unwrap();
    binding::reconcile_interrupted(&mut uncertain).unwrap();
    assert_eq!(uncertain.send_state, SendState::Uncertain);
}

/// An attempt is recorded before anything can reach a provider, so opening one
/// that already claims to have been sent is refused outright.
#[test]
fn the_ledger_refuses_to_open_an_attempt_that_already_claims_to_have_been_sent() {
    let home = tempdir().unwrap();
    let store = restart(&home, None);
    let mut presumptuous = attempt("run-0011", 1);
    presumptuous.send_state = SendState::Sending;
    assert!(store.open_attempt(&presumptuous).is_err());

    let mut invalid = attempt("run-0012", 1);
    invalid.ordinal = 0;
    assert!(
        store.open_attempt(&invalid).is_err(),
        "an attempt that fails its own validator was recorded"
    );

    // Opening the same attempt twice is refused, so a retry loop cannot
    // silently reset a record it already wrote.
    let once = attempt("run-0013", 1);
    store.open_attempt(&once).unwrap();
    assert!(store.open_attempt(&once).is_err());
}
