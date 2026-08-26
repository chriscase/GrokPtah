//! Tauri commands for executing a bounded Help answer.
//!
//! An IPC adapter and nothing more. The executor, the host key, and the served
//! digests all live in `grokptah-help-answer`; this file holds no policy that
//! could drift from what the crate's own tests cover.
//!
//! Two boundaries are worth naming, because they are the reason this file is
//! shaped the way it is:
//!
//! 1. **The renderer does not supply the expectation.** What corpus, index,
//!    manifest, and revision are being served is the host's knowledge, read
//!    here from managed state. A command that accepted those from its caller
//!    would let the caller describe the world it wants to be admitted into.
//! 2. **The renderer does not supply a provider.** The executor is constructed
//!    at setup, by code holding host key material. There is no command that
//!    installs one, because "no caller-injected production transport" has to
//!    be a shape, not a comment.
//!
//! No filesystem, workspace, or network access is reachable from here.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use grokptah_help_answer::{
    AnswerRequestCore, ExecutionReceipt, ExecutorStats, HelpAnswerExecutor,
};
use grokptah_help_authority::{AdmissionExpectation, AnswerAdmission};
use tauri::State;

/// What this build is currently serving.
#[derive(Debug, Clone)]
pub struct ServedAnswerState {
    /// Corpus digest being served.
    pub corpus_digest: String,
    /// Index digest being served.
    pub index_digest: String,
    /// Manifest digest being served.
    pub manifest_digest: String,
    /// Grant revision currently in force.
    pub current_revision: u64,
    /// Policy revision currently in force.
    pub policy_revision: String,
}

/// The host's answer executor, when one has been configured.
///
/// `None` is an ordinary state, not a failure: a build with no provider
/// configured still has fully useful offline Help search, and the renderer is
/// told exactly that rather than being handed a broken execution path.
pub struct HelpAnswerState {
    executor: Option<HelpAnswerExecutor>,
    served: Mutex<ServedAnswerState>,
}

impl HelpAnswerState {
    /// Register a configured executor.
    ///
    /// Unreachable in this build, and deliberately so: no provider is
    /// configured yet, and wiring one is product wiring this lane stops short
    /// of. Kept — and covered by this file's own tests — because deleting the
    /// only way to configure an executor would make the unconfigured path the
    /// only path, which is not the same thing as "not configured yet".
    #[allow(dead_code)]
    #[must_use]
    pub fn configured(executor: HelpAnswerExecutor, served: ServedAnswerState) -> Self {
        Self {
            executor: Some(executor),
            served: Mutex::new(served),
        }
    }

    /// A build with no answer provider.
    #[must_use]
    pub fn unconfigured(served: ServedAnswerState) -> Self {
        Self {
            executor: None,
            served: Mutex::new(served),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Execute one admitted Help answer and return its receipt.
///
/// The receipt carries no artifact of the exchange: no question, no answer, no
/// quote, no path.
///
/// # Errors
/// Returns a short reason when no executor is configured or the queue is at
/// its bound. A provider or validation failure is not an error here — it is an
/// outcome, and it comes back in the receipt.
#[tauri::command]
pub fn help_answer_execute(
    state: State<'_, HelpAnswerState>,
    core: AnswerRequestCore,
    admission: AnswerAdmission,
) -> Result<ExecutionReceipt, String> {
    execute(&state, core, admission)
}

/// The body of [`help_answer_execute`], separated from the IPC signature.
///
/// `State` cannot be constructed outside a running Tauri app, so keeping the
/// logic here is what makes the adapter's own behaviour testable rather than
/// only assertable by reading it.
fn execute(
    state: &HelpAnswerState,
    core: AnswerRequestCore,
    admission: AnswerAdmission,
) -> Result<ExecutionReceipt, String> {
    let Some(executor) = state.executor.as_ref() else {
        return Err("no-provider-configured".to_string());
    };
    let served = match state.served.lock() {
        Ok(served) => served.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let expectation = AdmissionExpectation {
        corpus_digest: served.corpus_digest,
        index_digest: served.index_digest,
        manifest_digest: served.manifest_digest,
        current_revision: served.current_revision,
        policy_revision: served.policy_revision,
        // Recomputed by the executor from the body it dispatches. Supplying a
        // placeholder here is deliberate: the value a caller could influence
        // is never the one that decides.
        request_digest: String::new(),
        now_ms: now_ms(),
    };

    match executor.submit(core, admission, &expectation) {
        Ok(task) => Ok(task.join()),
        Err(refusal) => Err(refusal.rejection.to_string()),
    }
}

/// What the executor is doing right now.
///
/// Exposed so a caller can see reduced capacity rather than discovering it as
/// unexplained latency.
///
/// # Errors
/// Returns a short reason when no executor is configured.
#[tauri::command]
pub fn help_answer_stats(state: State<'_, HelpAnswerState>) -> Result<ExecutorStats, String> {
    stats(&state)
}

fn stats(state: &HelpAnswerState) -> Result<ExecutorStats, String> {
    state
        .executor
        .as_ref()
        .map(HelpAnswerExecutor::stats)
        .ok_or_else(|| "no-provider-configured".to_string())
}

/// The JSON Schema for the answer contracts.
///
/// Emitted from the crate so a consumer validates against exactly the document
/// this build enforces, rather than a copy that has drifted from it.
#[tauri::command]
#[must_use]
pub fn help_answer_schema() -> serde_json::Value {
    grokptah_help_answer::schema::json_schema()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use grokptah_help_answer::{
        request_digest, AnswerCitationInput, AnswerContextChunk, AnswerReply, CancelToken,
        ExecutionOutcome, ExecutorConfig, FailureReason, HelpAnswerProvider, ProviderError,
        ReplyValidator, ReplyVerdict,
    };
    use grokptah_help_authority::{mint_admission, AnswerRoute, BoundCitation, GrantMintingKey};

    use super::*;

    const CORPUS: &str = "sha256:corpus";
    const INDEX: &str = "sha256:index";
    const MANIFEST: &str = "sha256:manifest";

    struct Fixed {
        admission_id: String,
    }

    impl HelpAnswerProvider for Fixed {
        fn answer(
            &self,
            request: &AnswerRequestCore,
            _cancel: &CancelToken,
        ) -> Result<AnswerReply, ProviderError> {
            Ok(AnswerReply {
                schema: grokptah_help_answer::HELP_ANSWER_RESPONSE_SCHEMA.to_string(),
                answer: "Recover a durable run safely.".to_string(),
                citations: vec![AnswerCitationInput {
                    claim_index: 0,
                    chunk_id: request.context[0].chunk_id.clone(),
                    article_id: request.context[0].article_id.clone(),
                    source_id: request.context[0].source_ids[0].clone(),
                    quote: request.context[0].text.clone(),
                }],
                uncertainty: "Live state must be re-checked.".to_string(),
                corpus_digest: request.corpus_digest.clone(),
                admission_id: self.admission_id.clone(),
            })
        }
    }

    struct Accepting;

    impl ReplyValidator for Accepting {
        fn verify(&self, _: &AnswerRequestCore, reply: &AnswerReply) -> ReplyVerdict {
            ReplyVerdict {
                accepted: true,
                citations: reply
                    .citations
                    .iter()
                    .map(|citation| BoundCitation {
                        claim_index: citation.claim_index,
                        chunk_id: citation.chunk_id.clone(),
                        chunk_digest: "sha256:chunk".to_string(),
                        source_id: citation.source_id.clone(),
                        start_utf8: 0,
                        end_utf8: 28,
                    })
                    .collect(),
                cited_source_ids: reply
                    .citations
                    .iter()
                    .map(|citation| citation.source_id.clone())
                    .collect(),
                claim_count: 1,
            }
        }
    }

    fn key() -> GrantMintingKey {
        GrantMintingKey::new(vec![3u8; 32]).expect("key")
    }

    fn core() -> AnswerRequestCore {
        AnswerRequestCore {
            schema: grokptah_help_answer::HELP_ANSWER_REQUEST_SCHEMA.to_string(),
            query: "durable run recovery".to_string(),
            corpus_digest: CORPUS.to_string(),
            index_digest: INDEX.to_string(),
            context: vec![AnswerContextChunk {
                chunk_id: "c1".to_string(),
                article_id: "operations.durable-recovery".to_string(),
                chunk_digest: "sha256:chunk".to_string(),
                text: "Recover a durable run safely".to_string(),
                source_ids: vec!["durable.lifecycle".to_string()],
            }],
            instruction: "Answer only from the supplied Help context.".to_string(),
            tools_disabled: true,
            conversation_disabled: true,
            max_answer_chars: 4_000,
        }
    }

    fn served() -> ServedAnswerState {
        ServedAnswerState {
            corpus_digest: CORPUS.to_string(),
            index_digest: INDEX.to_string(),
            manifest_digest: MANIFEST.to_string(),
            current_revision: 7,
            policy_revision: "policy-1".to_string(),
        }
    }

    fn admission_for(core: &AnswerRequestCore) -> AnswerAdmission {
        mint_admission(
            &key(),
            &AnswerRoute {
                provider_id: "company-gateway".to_string(),
                tenant_id: "tenant-a".to_string(),
                project_id: None,
                model_id: "review-model".to_string(),
            },
            &request_digest(core),
            CORPUS,
            INDEX,
            MANIFEST,
            7,
            "policy-1",
            now_ms(),
            60_000,
        )
        .expect("mints")
    }

    #[test]
    fn an_unconfigured_build_says_so_rather_than_failing_obscurely() {
        let state = HelpAnswerState::unconfigured(served());
        let core = core();
        let admission = admission_for(&core);
        assert_eq!(
            execute(&state, core, admission),
            Err("no-provider-configured".to_string())
        );
        assert_eq!(stats(&state), Err("no-provider-configured".to_string()));
    }

    #[test]
    fn a_configured_build_reaches_the_same_outcome_as_the_crate() {
        let core = core();
        let admission = admission_for(&core);
        let executor = HelpAnswerExecutor::new(
            key(),
            Arc::new(Fixed {
                admission_id: admission.admission_id.clone(),
            }),
            Arc::new(Accepting),
            ExecutorConfig {
                capacity: 1,
                queue_limit: 2,
                deadline_ms: 2_000,
                join_budget_ms: 500,
                tick_ms: 10,
            },
        );
        let state = HelpAnswerState::configured(executor, served());

        let receipt = execute(&state, core, admission.clone()).expect("executes");
        assert_eq!(receipt.outcome, ExecutionOutcome::Answered);
        assert_eq!(receipt.admission_id, admission.admission_id);
        assert!(receipt.outcome_digest.is_some());

        let live = stats(&state).expect("configured");
        assert_eq!(live.capacity, 1);
        assert_eq!(live.stuck, 0);
    }

    #[test]
    fn the_renderer_cannot_describe_the_world_it_wants_admitted_into() {
        // The command takes no expectation. An admission minted against a
        // different served manifest is refused using the host's own state,
        // whatever the caller believes is being served.
        let core = core();
        let elsewhere = mint_admission(
            &key(),
            &AnswerRoute {
                provider_id: "company-gateway".to_string(),
                tenant_id: "tenant-a".to_string(),
                project_id: None,
                model_id: "review-model".to_string(),
            },
            &request_digest(&core),
            CORPUS,
            INDEX,
            "sha256:some-other-manifest",
            7,
            "policy-1",
            now_ms(),
            60_000,
        )
        .expect("mints");

        let executor = HelpAnswerExecutor::new(
            key(),
            Arc::new(Fixed {
                admission_id: elsewhere.admission_id.clone(),
            }),
            Arc::new(Accepting),
            ExecutorConfig {
                capacity: 1,
                queue_limit: 2,
                deadline_ms: 2_000,
                join_budget_ms: 500,
                tick_ms: 10,
            },
        );
        let state = HelpAnswerState::configured(executor, served());

        let receipt = execute(&state, core, elsewhere).expect("executes");
        assert_eq!(receipt.outcome, ExecutionOutcome::Denied);
        assert_eq!(receipt.failure, Some(FailureReason::AdmissionRefused));

        if let Some(executor) = state.executor.as_ref() {
            executor.shutdown(Duration::from_secs(2));
        }
    }
}
