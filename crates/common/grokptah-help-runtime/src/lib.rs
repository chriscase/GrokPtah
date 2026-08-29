//! Host-supervised Semantic Help execution and validation.
//!
//! * [`executor`] — the one bounded executor: fixed concurrency, queue, and
//!   deadline; exactly one provider request per ask; capacity held until the
//!   provider is observed to quiesce.
//! * [`validate`] — the host-side check that turns a raw reply into claims
//!   bound to exact corpus bytes, or into an abstention.
//! * [`redact`] — what is removed before any reply text reaches a renderer.
//!
//! [`project`] is the only way an answer becomes something a renderer sees. It
//! drops every digest, span offset, chunk id, principal, and reason code, and
//! replaces each span with the exact bytes it named. A renderer receives text
//! and a place to look it up — never the means to check its own authority.

pub mod executor;
pub mod redact;
pub mod validate;

#[cfg(test)]
mod tests;

use grokptah_help_contract::corpus::Corpus;
use grokptah_help_contract::dto::{
    CitationProjection, ClaimProjection, HelpProjection, ProjectionStatus, PublicErrorCode,
    ValidatedAnswer,
};

use crate::executor::RunState;

/// Turn a validated answer into the renderer's view.
///
/// Spans become quoted text here, resolved from the corpus rather than carried
/// from the reply, so what is shown is the corpus's bytes and not the model's
/// recollection of them. A span that no longer resolves drops its citation,
/// and a claim left with no citation drops entirely: the projection cannot
/// show a claim it can no longer support.
#[must_use]
pub fn project(
    handle: &str,
    answer: &ValidatedAnswer,
    corpus: &Corpus,
    status: ProjectionStatus,
) -> HelpProjection {
    let mut claims: Vec<ClaimProjection> = Vec::new();
    for claim in &answer.claims {
        let mut citations: Vec<CitationProjection> = Vec::new();
        for span in &claim.spans {
            let Some(quote) = validate::quote_for(span, corpus) else {
                continue;
            };
            let Some(source) = corpus.source(&span.source_id) else {
                continue;
            };
            citations.push(CitationProjection {
                source_id: source.id.clone(),
                path: source.path.clone(),
                heading: source.heading.clone(),
                quote: quote.to_string(),
            });
        }
        if citations.is_empty() {
            continue;
        }
        claims.push(ClaimProjection {
            ordinal: claims.len(),
            text: claim.text.clone(),
            citations,
        });
    }
    let status = if claims.is_empty() && status == ProjectionStatus::Answered {
        ProjectionStatus::Abstained
    } else {
        status
    };
    HelpProjection {
        handle: handle.to_string(),
        status,
        claims,
        error: None,
        message: None,
    }
}

/// The renderer's view of a run that produced nothing to show.
#[must_use]
pub fn project_unavailable(handle: &str, code: PublicErrorCode) -> HelpProjection {
    HelpProjection {
        handle: handle.to_string(),
        status: ProjectionStatus::Unavailable,
        claims: Vec::new(),
        error: Some(code),
        message: Some(code.message().to_string()),
    }
}

/// Map a run state to the status a renderer may see.
#[must_use]
pub const fn status_for(state: RunState) -> ProjectionStatus {
    match state {
        RunState::Queued => ProjectionStatus::Queued,
        RunState::Running | RunState::Draining => ProjectionStatus::Running,
        RunState::Answered => ProjectionStatus::Answered,
        RunState::Abstained => ProjectionStatus::Abstained,
        RunState::Denied | RunState::Cancelled | RunState::Abandoned | RunState::TimedOut => {
            ProjectionStatus::Unavailable
        }
    }
}
