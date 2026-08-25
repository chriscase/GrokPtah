//! Supervision: bounds, deadlines, cancellation, and held capacity.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use grokptah_help_authority::{
    AdmissionExpectation, AnswerAdmission, AnswerRoute, BoundCitation, GrantMintingKey,
    mint_admission,
};

use crate::dto::*;
use crate::executor::*;
use crate::receipt::*;

const CORPUS: &str = "sha256:corpus";
const INDEX: &str = "sha256:index";
const MANIFEST: &str = "sha256:manifest";

fn key() -> GrantMintingKey {
    GrantMintingKey::new(vec![5u8; 32]).expect("key")
}

fn core(query: &str) -> AnswerRequestCore {
    AnswerRequestCore {
        schema: HELP_ANSWER_REQUEST_SCHEMA.into(),
        query: query.into(),
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        context: vec![AnswerContextChunk {
            chunk_id: "c1".into(),
            article_id: "operations.durable-recovery".into(),
            chunk_digest: "sha256:chunk".into(),
            text: "Recover a durable run safely".into(),
            source_ids: vec!["durable.lifecycle".into()],
        }],
        instruction: "Answer only from the supplied Help context.".into(),
        tools_disabled: true,
        conversation_disabled: true,
        max_answer_chars: 4_000,
    }
}

fn admission_for(core: &AnswerRequestCore) -> AnswerAdmission {
    mint_admission(
        &key(),
        &AnswerRoute {
            provider_id: "company-gateway".into(),
            tenant_id: "tenant-a".into(),
            project_id: None,
            model_id: "review-model".into(),
        },
        &request_digest(core),
        CORPUS,
        INDEX,
        MANIFEST,
        7,
        "policy-1",
        1_000,
        60_000,
    )
    .expect("mints")
}

fn expectation() -> AdmissionExpectation {
    AdmissionExpectation {
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: MANIFEST.into(),
        current_revision: 7,
        policy_revision: "policy-1".into(),
        // Recomputed by the executor from the body it dispatches; this value
        // is deliberately wrong to prove the executor does not trust it.
        request_digest: "sha256:whatever-the-caller-claims".into(),
        now_ms: 1_500,
    }
}

// ------------------------------------------------------------- providers

struct Answering {
    calls: Arc<AtomicUsize>,
    /// The admission id the reply should echo.
    ///
    /// Shared, not thread-local: the provider runs on a worker thread, where a
    /// thread-local set by the test would be empty.
    admission_id: Arc<Mutex<String>>,
    /// How many citations to emit.
    citations: usize,
}

impl Answering {
    fn new(calls: Arc<AtomicUsize>, admission_id: Arc<Mutex<String>>) -> Self {
        Self {
            calls,
            admission_id,
            citations: 1,
        }
    }
}

impl HelpAnswerProvider for Answering {
    fn answer(
        &self,
        request: &AnswerRequestCore,
        _cancel: &CancelToken,
    ) -> Result<AnswerReply, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let citations = (0..self.citations)
            .map(|index| AnswerCitationInput {
                claim_index: index as u32,
                chunk_id: request.context[0].chunk_id.clone(),
                article_id: request.context[0].article_id.clone(),
                source_id: request.context[0].source_ids[0].clone(),
                quote: request.context[0].text.clone(),
            })
            .collect();
        Ok(AnswerReply {
            schema: HELP_ANSWER_RESPONSE_SCHEMA.into(),
            answer: "Recover a durable run safely.".into(),
            citations,
            uncertainty: "Live state must be re-checked.".into(),
            corpus_digest: request.corpus_digest.clone(),
            admission_id: self.admission_id.lock().expect("lock").clone(),
        })
    }
}

struct Failing;

impl HelpAnswerProvider for Failing {
    fn answer(&self, _: &AnswerRequestCore, _: &CancelToken) -> Result<AnswerReply, ProviderError> {
        Err(ProviderError)
    }
}

/// A provider that stops when asked.
struct Cooperative {
    entered: Arc<Barrier>,
}

impl HelpAnswerProvider for Cooperative {
    fn answer(
        &self,
        _: &AnswerRequestCore,
        cancel: &CancelToken,
    ) -> Result<AnswerReply, ProviderError> {
        self.entered.wait();
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(ProviderError)
    }
}

/// A provider that never reads its cancellation token.
struct Deaf {
    entered: Arc<Barrier>,
    release: Arc<Mutex<bool>>,
}

impl HelpAnswerProvider for Deaf {
    fn answer(
        &self,
        _: &AnswerRequestCore,
        _cancel: &CancelToken,
    ) -> Result<AnswerReply, ProviderError> {
        self.entered.wait();
        loop {
            if *self.release.lock().expect("lock") {
                return Err(ProviderError);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

// ------------------------------------------------------------ validators

struct Accepting;

impl ReplyValidator for Accepting {
    fn verify(&self, _: &AnswerRequestCore, reply: &AnswerReply) -> ReplyVerdict {
        ReplyVerdict {
            accepted: true,
            citations: reply
                .citations
                .iter()
                .enumerate()
                .map(|(position, citation)| BoundCitation {
                    claim_index: citation.claim_index,
                    chunk_id: citation.chunk_id.clone(),
                    chunk_digest: "sha256:chunk".into(),
                    source_id: citation.source_id.clone(),
                    start_utf8: (position * 100) as u32,
                    end_utf8: (position * 100 + 28) as u32,
                })
                .collect(),
            cited_source_ids: reply
                .citations
                .iter()
                .map(|c| c.source_id.clone())
                .collect(),
            claim_count: 1,
        }
    }
}

struct Refusing;

impl ReplyValidator for Refusing {
    fn verify(&self, _: &AnswerRequestCore, _: &AnswerReply) -> ReplyVerdict {
        ReplyVerdict {
            accepted: false,
            citations: Vec::new(),
            cited_source_ids: Vec::new(),
            claim_count: 0,
        }
    }
}

/// Reports every citation over the same bytes, to exercise overlap refusal.
struct Overlapping;

impl ReplyValidator for Overlapping {
    fn verify(&self, _: &AnswerRequestCore, reply: &AnswerReply) -> ReplyVerdict {
        ReplyVerdict {
            accepted: true,
            citations: reply
                .citations
                .iter()
                .map(|citation| BoundCitation {
                    claim_index: citation.claim_index,
                    chunk_id: "c1".into(),
                    chunk_digest: "sha256:chunk".into(),
                    source_id: citation.source_id.clone(),
                    start_utf8: 0,
                    end_utf8: 28,
                })
                .collect(),
            cited_source_ids: vec!["durable.lifecycle".into()],
            claim_count: 1,
        }
    }
}

fn executor(
    provider: Arc<dyn HelpAnswerProvider>,
    validator: Arc<dyn ReplyValidator>,
    config: ExecutorConfig,
) -> HelpAnswerExecutor {
    HelpAnswerExecutor::new(key(), provider, validator, config)
}

fn fast() -> ExecutorConfig {
    ExecutorConfig {
        capacity: 1,
        queue_limit: 2,
        deadline_ms: 200,
        join_budget_ms: 60,
        tick_ms: 5,
    }
}

// ------------------------------------------------------------------ tests

/// An admission id cell the fixture provider echoes back.
fn echoed() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(String::new()))
}

#[test]
fn an_admitted_request_answers_and_binds_its_outcome() {
    let calls = Arc::new(AtomicUsize::new(0));
    let echo = echoed();
    let exec = executor(
        Arc::new(Answering::new(Arc::clone(&calls), Arc::clone(&echo))),
        Arc::new(Accepting),
        fast(),
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    *echo.lock().expect("lock") = admission.admission_id.clone();

    let task = exec
        .submit(core, admission.clone(), &expectation())
        .unwrap_or_else(|refusal| panic!("submit refused: {refusal}"));
    let receipt = task.join();

    assert_eq!(receipt.outcome, ExecutionOutcome::Answered);
    assert_eq!(receipt.failure, None);
    assert!(receipt.outcome_digest.is_some());
    assert_eq!(
        receipt.cited_source_ids,
        vec!["durable.lifecycle".to_string()]
    );
    assert_eq!(receipt.admission_id, admission.admission_id);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_receipt_carries_no_artifact_of_the_exchange() {
    let echo = echoed();
    let exec = executor(
        Arc::new(Answering::new(
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&echo),
        )),
        Arc::new(Accepting),
        fast(),
    );
    let core = core("my key xai-AbCdEf0123456789 stopped working");
    let admission = admission_for(&core);
    *echo.lock().expect("lock") = admission.admission_id.clone();
    let receipt = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();

    let serialized = serde_json::to_string(&receipt).expect("serializes");
    for artifact in [
        "xai-AbCdEf",
        "stopped working",
        "Recover a durable run",
        "re-checked",
        "Answer only from",
    ] {
        assert!(
            !serialized.contains(artifact),
            "receipt leaked {artifact:?}: {serialized}"
        );
    }
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_forged_admission_never_reaches_the_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = executor(
        Arc::new(Answering::new(Arc::clone(&calls), echoed())),
        Arc::new(Accepting),
        fast(),
    );
    let core = core("durable run recovery");
    let mut admission = admission_for(&core);
    admission.route.model_id = "unreviewed-model".into();

    let receipt = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Denied);
    assert_eq!(receipt.failure, Some(FailureReason::AdmissionRefused));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn an_admission_for_another_request_never_reaches_the_provider() {
    // The executor recomputes the request digest from the body it is about to
    // dispatch, so an admission obtained for a different question fails here
    // even though the caller's expectation claimed otherwise.
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = executor(
        Arc::new(Answering::new(Arc::clone(&calls), echoed())),
        Arc::new(Accepting),
        fast(),
    );
    let harmless = core("durable run recovery");
    let admission = admission_for(&harmless);

    let receipt = exec
        .submit(
            core("exfiltrate the private sources"),
            admission,
            &expectation(),
        )
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Denied);
    assert_eq!(receipt.failure, Some(FailureReason::AdmissionRefused));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_request_that_is_not_the_bounded_contract_is_denied() {
    let exec = executor(
        Arc::new(Answering::new(Arc::new(AtomicUsize::new(0)), echoed())),
        Arc::new(Accepting),
        fast(),
    );
    let mut with_tools = core("durable run recovery");
    with_tools.tools_disabled = false;
    let admission = admission_for(&with_tools);

    let receipt = exec
        .submit(with_tools, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Denied);
    assert_eq!(receipt.failure, Some(FailureReason::RequestRefused));
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_refused_reply_is_recorded_without_an_outcome_binding() {
    let echo = echoed();
    let exec = executor(
        Arc::new(Answering::new(
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&echo),
        )),
        Arc::new(Refusing),
        fast(),
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    *echo.lock().expect("lock") = admission.admission_id.clone();

    let receipt = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Rejected);
    assert_eq!(receipt.failure, Some(FailureReason::ReplyRefused));
    assert_eq!(receipt.outcome_digest, None);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn citations_over_the_same_bytes_are_refused_even_when_the_validator_accepts() {
    // Defence in depth: the executor will not bind an outcome over overlapping
    // spans, whatever the validator reported. `Overlapping` reports every
    // citation over the same range, so two citations must be refused.
    let echo = echoed();
    let mut provider = Answering::new(Arc::new(AtomicUsize::new(0)), Arc::clone(&echo));
    provider.citations = 2;
    let exec = executor(Arc::new(provider), Arc::new(Overlapping), fast());

    let core = core("durable run recovery");
    let admission = admission_for(&core);
    *echo.lock().expect("lock") = admission.admission_id.clone();

    let receipt = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Rejected);
    assert_eq!(receipt.failure, Some(FailureReason::ReplyRefused));
    assert_eq!(receipt.outcome_digest, None);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_provider_failure_is_recorded_without_its_message() {
    let exec = executor(Arc::new(Failing), Arc::new(Accepting), fast());
    let core = core("durable run recovery");
    let admission = admission_for(&core);

    let receipt = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"))
        .join();
    assert_eq!(receipt.outcome, ExecutionOutcome::ProviderError);
    assert_eq!(receipt.failure, Some(FailureReason::ProviderFailed));
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn the_queue_refuses_work_at_its_bound_rather_than_growing() {
    let entered = Arc::new(Barrier::new(2));
    let exec = executor(
        Arc::new(Cooperative {
            entered: Arc::clone(&entered),
        }),
        Arc::new(Accepting),
        ExecutorConfig {
            capacity: 1,
            queue_limit: 1,
            deadline_ms: 5_000,
            join_budget_ms: 5_000,
            tick_ms: 5,
        },
    );

    let running = core("first");
    let running_admission = admission_for(&running);
    let first = exec
        .submit(running, running_admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();

    // The single worker is busy; one more fits the queue.
    let queued = core("second");
    let queued_admission = admission_for(&queued);
    let second = exec
        .submit(queued, queued_admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));

    let overflow = core("third");
    let overflow_admission = admission_for(&overflow);
    match exec.submit(overflow, overflow_admission, &expectation()) {
        Err(refusal) => {
            assert_eq!(refusal.rejection, SubmitRejection::QueueFull);
            assert_eq!(refusal.receipt.outcome, ExecutionOutcome::Refused);
            assert_eq!(refusal.receipt.failure, Some(FailureReason::QueueFull));
            // A refusal is still a receipt: an audit needs to see the load.
            assert!(!refusal.receipt.receipt_digest.is_empty());
        }
        Ok(_) => panic!("expected a queue-full refusal, got a task"),
    }

    first.cancel();
    second.cancel();
    let _ = first.join();
    let _ = second.join();
    exec.shutdown(Duration::from_secs(5));
}

#[test]
fn a_cooperative_provider_stops_and_returns_its_slot() {
    let entered = Arc::new(Barrier::new(2));
    let exec = executor(
        Arc::new(Cooperative {
            entered: Arc::clone(&entered),
        }),
        Arc::new(Accepting),
        fast(),
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    let task = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();
    assert_eq!(exec.stats().in_flight, 1);

    task.cancel();
    let receipt = task.join();
    // The worker stopped, so this is a clean cancellation, not an abandonment.
    assert_eq!(receipt.outcome, ExecutionOutcome::ProviderError);
    assert_eq!(exec.stats().stuck, 0);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_deaf_provider_is_abandoned_and_keeps_the_slot_it_is_using() {
    // This is the case the design exists for. Reporting the slot free while a
    // thread is still inside a provider call is how a "cancelled" answer keeps
    // talking to a gateway with nobody watching.
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Mutex::new(false));
    let exec = executor(
        Arc::new(Deaf {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
        Arc::new(Accepting),
        fast(),
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    let task = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();

    task.cancel();
    let receipt = task.join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Abandoned);
    assert_eq!(receipt.failure, Some(FailureReason::CallerCancelled));

    // The task settled, but the worker is still running, and the executor says
    // so rather than claiming capacity it does not have.
    let held = exec.stats();
    assert_eq!(held.stuck, 1);
    assert_eq!(held.in_flight, 1);

    // Once the provider finally returns, the slot comes back.
    *release.lock().expect("lock") = true;
    let mut settled = exec.stats();
    for _ in 0..200 {
        if settled.stuck == 0 && settled.in_flight == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        settled = exec.stats();
    }
    assert_eq!(settled.stuck, 0);
    assert_eq!(settled.in_flight, 0);
    exec.shutdown(Duration::from_secs(5));
}

#[test]
fn a_deadline_is_enforced_by_the_executor_without_anyone_joining() {
    // No caller joins until after the fact: supervision must not depend on it.
    let entered = Arc::new(Barrier::new(2));
    let exec = executor(
        Arc::new(Cooperative {
            entered: Arc::clone(&entered),
        }),
        Arc::new(Accepting),
        ExecutorConfig {
            capacity: 1,
            queue_limit: 2,
            deadline_ms: 30,
            join_budget_ms: 500,
            tick_ms: 5,
        },
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    let task = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();

    let receipt = task.join();
    // The provider observed the supervisor's cancellation and returned.
    assert_eq!(receipt.outcome, ExecutionOutcome::ProviderError);
    exec.shutdown(Duration::from_secs(2));
}

#[test]
fn a_task_cancelled_before_a_worker_takes_it_never_reaches_the_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let exec = executor(
        Arc::new(Cooperative {
            entered: Arc::clone(&entered),
        }),
        Arc::new(Accepting),
        ExecutorConfig {
            capacity: 1,
            queue_limit: 2,
            deadline_ms: 5_000,
            join_budget_ms: 5_000,
            tick_ms: 5,
        },
    );
    let occupying = core("first");
    let occupying_admission = admission_for(&occupying);
    let first = exec
        .submit(occupying, occupying_admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();

    let queued = core("second");
    let queued_admission = admission_for(&queued);
    let second = exec
        .submit(queued, queued_admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    second.cancel();

    first.cancel();
    let _ = first.join();
    let receipt = second.join();
    assert_eq!(receipt.outcome, ExecutionOutcome::Cancelled);
    assert_eq!(receipt.failure, Some(FailureReason::CallerCancelled));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    exec.shutdown(Duration::from_secs(5));
}

#[test]
fn submitting_after_shutdown_is_refused_with_a_receipt() {
    let exec = executor(
        Arc::new(Answering::new(Arc::new(AtomicUsize::new(0)), echoed())),
        Arc::new(Accepting),
        fast(),
    );
    exec.shutdown(Duration::from_secs(2));

    let core = core("durable run recovery");
    let admission = admission_for(&core);
    match exec.submit(core, admission, &expectation()) {
        Err(refusal) => {
            assert_eq!(refusal.rejection, SubmitRejection::ShuttingDown);
            assert_eq!(refusal.receipt.outcome, ExecutionOutcome::Refused);
            assert_eq!(refusal.receipt.failure, Some(FailureReason::ShuttingDown));
        }
        Ok(_) => panic!("expected a shutdown refusal, got a task"),
    }
}

#[test]
fn join_within_returns_the_task_rather_than_losing_it() {
    let entered = Arc::new(Barrier::new(2));
    let exec = executor(
        Arc::new(Cooperative {
            entered: Arc::clone(&entered),
        }),
        Arc::new(Accepting),
        ExecutorConfig {
            capacity: 1,
            queue_limit: 2,
            deadline_ms: 5_000,
            join_budget_ms: 5_000,
            tick_ms: 5,
        },
    );
    let core = core("durable run recovery");
    let admission = admission_for(&core);
    let task = exec
        .submit(core, admission, &expectation())
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    entered.wait();

    let Err(task) = task.join_within(Duration::from_millis(20)) else {
        panic!("the task is still running, so the handle must come back");
    };
    assert!(!task.is_settled());
    task.cancel();
    let receipt = task.join();
    assert_eq!(receipt.outcome, ExecutionOutcome::ProviderError);
    exec.shutdown(Duration::from_secs(5));
}

#[test]
fn the_receipt_digest_follows_the_outcome() {
    let base = ReceiptInputs {
        admission_id: "sha256:admission".into(),
        request_digest: "sha256:request".into(),
        corpus_digest: CORPUS.into(),
        index_digest: INDEX.into(),
        manifest_digest: MANIFEST.into(),
        grant_revision: 7,
        outcome: ExecutionOutcome::Answered,
        failure: None,
        outcome_digest: Some("sha256:outcome".into()),
        cited_source_ids: vec!["durable.lifecycle".into()],
        claim_count: 1,
        queued_ms: 0,
        ran_ms: 1,
    };
    let answered = build_receipt(base.clone());
    let denied = build_receipt(ReceiptInputs {
        outcome: ExecutionOutcome::Denied,
        failure: Some(FailureReason::AdmissionRefused),
        outcome_digest: None,
        ..base.clone()
    });
    assert_ne!(answered.receipt_digest, denied.receipt_digest);

    // A different set of cited sources must not digest the same.
    let elsewhere = build_receipt(ReceiptInputs {
        cited_source_ids: vec!["providers.gateway".into()],
        ..base.clone()
    });
    assert_ne!(answered.receipt_digest, elsewhere.receipt_digest);

    // Order and duplication of source ids are normalized away.
    let shuffled = build_receipt(ReceiptInputs {
        cited_source_ids: vec!["durable.lifecycle".into(), "durable.lifecycle".into()],
        ..base
    });
    assert_eq!(answered.receipt_digest, shuffled.receipt_digest);
}
