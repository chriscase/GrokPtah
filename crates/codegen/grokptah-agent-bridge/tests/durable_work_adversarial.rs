//! Adversarial tests against the **real** durable Work ledger on `main`.
//!
//! These deliberately drive `OrchStore` itself rather than a model of it. A
//! second claim ledger would be a second authority, which is what #478 and #492
//! exist to prevent; the useful thing this train can add is evidence about the
//! ledger that already exists — both where it holds and where it does not.
//!
//! Two of these are characterization tests. They assert today's behaviour, not
//! the behaviour we want, and each says which PR closes the gap. Pinning them
//! means a fix shows up as a deliberate change to this file rather than as
//! silence.

use std::sync::{Arc, Barrier};
use std::thread;

use grokptah_agent_bridge::orchestration::{OrchStore, WorkItem, WorkPolicy};
use grokptah_agent_bridge::provider_observation::AttemptDisposition;
use tempfile::tempdir;
use uuid::Uuid;

fn seeded(store_path: &std::path::Path, objective: &str) -> (OrchStore, WorkItem) {
    let store = OrchStore::open(store_path).expect("open work store");
    let item = add_item(&store, objective);
    (store, item)
}

/// Add another item to a store that is already open. Opening one home twice
/// takes the advisory lock twice, which is refused by design.
fn add_item(store: &OrchStore, objective: &str) -> WorkItem {
    let item = WorkItem::new(
        "test",
        objective,
        Uuid::new_v4(),
        "/tmp/project",
        "test-operator",
        WorkPolicy::default(),
    )
    .expect("construct work item");
    store.save_work_item(&item).expect("seed work item");
    item
}

/// Duplicate workers. Two processes reaching the same item must not both get a
/// lease, however the race falls.
#[test]
fn two_workers_racing_for_one_item_yield_exactly_one_lease() {
    let home = tempdir().unwrap();
    let (store, item) = seeded(home.path(), "claimed once");
    let store = Arc::new(store);

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["worker-a", "worker-b"]
        .into_iter()
        .map(|worker| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let work_id = item.work_id.clone();
            thread::spawn(move || {
                barrier.wait();
                store.claim_work(&work_id, worker, Some(60_000)).is_ok()
            })
        })
        .collect();

    let winners = handles
        .into_iter()
        .filter(|_| true)
        .map(|h| h.join().expect("worker thread"))
        .filter(|claimed| *claimed)
        .count();

    assert_eq!(winners, 1, "exactly one worker may hold the lease");

    // And a third, arriving after the race, is refused too.
    assert!(
        store
            .claim_work(&item.work_id, "worker-c", Some(60_000))
            .is_err(),
        "a live lease refuses every later claimant"
    );
}

/// Restart. A claim written before a crash is still there, with its revision,
/// after the store is reopened.
#[test]
fn a_claim_survives_a_restart_with_its_revision() {
    let home = tempdir().unwrap();
    let (store, item) = seeded(home.path(), "survive a restart");
    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(60_000))
        .expect("claimed");
    let claimed_revision = claim.work.revision;
    assert!(
        claimed_revision > item.revision,
        "claiming advances the revision"
    );
    drop(store);

    let reopened = OrchStore::open(home.path()).expect("reopen after restart");
    let recovered = reopened
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("the work item survived");
    assert_eq!(recovered.revision, claimed_revision);
    assert!(
        reopened
            .claim_work(&item.work_id, "worker-b", Some(60_000))
            .is_err(),
        "the surviving lease is still honoured after a restart"
    );
}

/// **Characterization — this is a gap, not a guarantee.**
///
/// `save_work_item` writes unconditionally. The store has revision
/// compare-and-set for manager plans (`save_manager_plan_with_work_cas`) but
/// not for work items, so a caller holding a stale copy silently overwrites a
/// newer one and the revision goes *backwards*.
///
/// #470 closes this. When it lands, this test should start failing and be
/// rewritten to assert the refusal.
#[test]
fn a_generic_save_still_clobbers_a_newer_revision() {
    let home = tempdir().unwrap();
    let (store, item) = seeded(home.path(), "clobbered");
    let stale = item.clone();

    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(60_000))
        .expect("claimed");
    assert!(claim.work.revision > stale.revision);

    store
        .save_work_item(&stale)
        .expect("today this is accepted, which is the gap");

    let after = store
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("present");
    assert_eq!(
        after.revision, stale.revision,
        "a stale generic save wins today; #470 is what makes it lose"
    );
}

/// Repeated malformed records.
///
/// A work record that cannot be parsed makes the store **fail closed at open**
/// rather than quietly returning a shorter list. That is the property worth
/// having: a caller can tell "no work" from "unreadable work", and corruption
/// cannot silently shrink the ledger however many records are damaged.
///
/// Worth knowing, and deliberately not asserted here: this is *not* uniform
/// across the store. The run and idempotency read paths use
/// `let Ok(record) = serde_json::from_str(..) else { continue }`, so a damaged
/// record there is skipped in silence. Making that uniform means editing the
/// four-donor collision surface in `store.rs`, which is #470's seam, not this
/// branch's.
#[test]
fn a_malformed_work_record_makes_the_store_fail_closed() {
    let home = tempdir().unwrap();
    let (store, keep) = seeded(home.path(), "intact");
    let lost = add_item(&store, "corrupt me");
    assert_eq!(store.list_work_items().expect("list").len(), 2);
    drop(store);

    let mut corrupted = 0;
    for entry in walk(home.path()) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        if text.contains(&lost.work_id) && entry.extension().is_some_and(|e| e == "json") {
            std::fs::write(&entry, b"{ this is not json").expect("corrupt the record");
            corrupted += 1;
        }
    }
    assert_eq!(corrupted, 1, "exactly one record was corrupted");

    assert!(
        OrchStore::open(home.path()).is_err(),
        "a damaged work record must refuse service, not shrink the ledger"
    );

    // Repeated corruption behaves the same way: still refused, never partial.
    for entry in walk(home.path()) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        if text.contains(&keep.work_id) && entry.extension().is_some_and(|e| e == "json") {
            std::fs::write(&entry, b"{ also not json").expect("corrupt the second record");
        }
    }
    assert!(OrchStore::open(home.path()).is_err());
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
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
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// Bounded retries. The work policy caps attempts, and the cap is validated
/// rather than merely documented: zero and an over-large value are both
/// refused, so a caller cannot configure an unbounded retry loop.
#[test]
fn retry_budgets_are_bounded_by_construction() {
    let mut policy = WorkPolicy::default();
    assert!(policy.retry.max_attempts > 0, "a default budget exists");
    assert!(policy.validate().is_ok());

    policy.retry.max_attempts = 0;
    assert!(
        policy.validate().is_err(),
        "an unbounded-by-zero budget is refused"
    );

    policy.retry.max_attempts = 101;
    assert!(
        policy.validate().is_err(),
        "a budget past the workload bound is refused"
    );

    policy.retry.max_attempts = 100;
    assert!(policy.validate().is_ok(), "the bound itself is allowed");
}

/// **Characterization — uncertain and not-sent are not distinguished.**
///
/// `AttemptDisposition` classifies *what went wrong* and carries no
/// delivery-knowledge dimension. A connection refused before any byte moved is
/// `TransportError`; a timeout that may have been fully delivered is `Timeout`;
/// neither says whether the provider saw the request. Two attempts with very
/// different safe answers — one provably retryable, one that must never
/// auto-retry — are therefore indistinguishable from the disposition alone.
///
/// This is the gap #478 names and #497's G3 lattice closes with an explicit
/// `NotSent` / `Uncertain` / `Settled` split. It is recorded here rather than
/// papered over with a second lattice, which is what the audit of this branch
/// rejected.
#[test]
fn attempt_dispositions_do_not_yet_separate_not_sent_from_uncertain() {
    // Every failure disposition today is just a failure kind.
    let failures = [
        AttemptDisposition::HttpError,
        AttemptDisposition::TransportError,
        AttemptDisposition::Timeout,
        AttemptDisposition::Cancelled,
        AttemptDisposition::ProtocolError,
    ];
    for disposition in failures {
        assert_ne!(disposition, AttemptDisposition::Completed);
    }

    // The two that need opposite retry answers are the same shape of value:
    // nothing on the type tells them apart by delivery.
    assert_ne!(
        AttemptDisposition::TransportError,
        AttemptDisposition::Timeout,
        "they are distinct variants"
    );
    let refused_connection = AttemptDisposition::TransportError;
    let may_have_landed = AttemptDisposition::Timeout;
    for disposition in [refused_connection, may_have_landed] {
        // Neither carries, nor can be asked for, a delivery answer. The
        // assertion that matters is what is *absent*: no `delivery_knowledge`,
        // no `may_auto_retry`, no `NotSent` variant.
        let rendered = format!("{disposition:?}").to_ascii_lowercase();
        assert!(
            !rendered.contains("sent") && !rendered.contains("uncertain"),
            "delivery knowledge is not represented today: {rendered}"
        );
    }
}
