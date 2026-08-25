//! Supervised, non-persistent, tool-free execution of bounded Help answers.
//!
//! Help answering is not Chat. There is no conversation, no tool surface, no
//! workspace access, no persistence, and no provider fallback. This crate is
//! the execution side of that: it takes a request the host has *admitted*,
//! runs exactly one provider call under a deadline, validates the reply, and
//! records a receipt that carries no artifact of the exchange.
//!
//! Three properties it exists to hold:
//!
//! 1. **Only the host can execute.** [`HelpAnswerExecutor::new`] requires
//!    [`GrantMintingKey`](grokptah_help_authority::GrantMintingKey) material,
//!    because the key is what verifies admissions. A renderer cannot build an
//!    executor, so it cannot hand one its own provider — which is what "no
//!    caller-injected production transport" has to mean structurally rather
//!    than by convention.
//! 2. **Work is bounded and supervised.** A fixed pool, a bounded queue, a
//!    deadline enforced by the executor rather than by whoever remembers to
//!    join, and a slot that stays held until the worker actually stops.
//! 3. **Nothing is kept.** Receipts carry ids, digests, counts, and timings.
//!    Not the question, not the answer, not a quote, not a path.
//!
//! No filesystem, no network, no process spawning, no Tauri.

#![deny(missing_docs)]

pub mod dto;
pub mod executor;
pub mod receipt;
pub mod schema;

pub use dto::{
    AnswerCitationInput, AnswerContextChunk, AnswerReply, AnswerRequestCore, ContractError,
    HELP_ANSWER_REQUEST_SCHEMA, HELP_ANSWER_RESPONSE_SCHEMA, MAX_CITATIONS, MAX_CONTEXT_CHUNKS,
    MAX_QUERY_CHARS, request_digest,
};

pub use executor::{
    AnswerTask, CancelToken, ExecutorConfig, ExecutorStats, HelpAnswerExecutor, HelpAnswerProvider,
    ProviderError, ReplyValidator, ReplyVerdict, SubmitRefusal, SubmitRejection,
};

pub use receipt::{
    ExecutionOutcome, ExecutionReceipt, FailureReason, HELP_ANSWER_RECEIPT_SCHEMA, ReceiptInputs,
    build_receipt,
};

#[cfg(test)]
mod dto_tests;
#[cfg(test)]
mod executor_tests;
#[cfg(test)]
mod parity_tests;
