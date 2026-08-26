//! Tauri commands for Semantic Help.
//!
//! # What this file deliberately does not do
//!
//! An earlier design exposed `help_authorize(request, served)`: the caller
//! passed in the index it wanted checked. Nothing here takes a manifest, a
//! grant, an admission, a route, a principal, or a capability from the
//! renderer, and nothing could — those types are `Serialize`-only in
//! `grokptah-help-contract`, so a Tauri command cannot accept one. The
//! renderer's whole vocabulary is [`HelpAsk`], [`HelpFollow`],
//! [`HelpCancelRequest`], and an opaque session token the host issued.
//!
//! # No provider is configured in this build
//!
//! [`DesktopProvider`] is `Unconfigured`, and its `begin` returns
//! [`Begin::Rejected`]. That is not a stub standing in for a real provider: it
//! is the honest state of a build that has no qualified provider route, and
//! it means every ask abstains after zero bytes leave the process. Offline
//! retrieval — which is what the Help Center actually renders — does not
//! depend on it. Wiring a real provider is a separate change that has to bring
//! its own qualification evidence.

use std::sync::Mutex;

use grokptah_help_authority::{Authority, SessionRecord};
use grokptah_help_contract::corpus::{Corpus, Visibility};
use grokptah_help_contract::dto::{
    BoundsProjection, HelpAsk, HelpCancelRequest, HelpFollow, HelpProjection, PrincipalKind,
    ProjectionStatus, PublicErrorCode,
};
use grokptah_help_runtime::executor::{
    Begin, Bounds, Executor, Poll, Provider, RunState, SubmitError, Ticket,
};
use grokptah_help_runtime::{project, project_unavailable, status_for, validate};

/// The desktop's provider seam.
///
/// Held as an enum rather than a boxed trait so the "no provider" case is a
/// state the type system knows about, not a null that some later branch might
/// forget to check.
pub enum DesktopProvider {
    /// No qualified provider route. Nothing is sent.
    Unconfigured,
}

impl Provider for DesktopProvider {
    fn begin(&mut self, _request: &grokptah_help_contract::dto::HelpRequest) -> Begin {
        match self {
            // `Rejected` is what keeps `SendCertainty::NotSent` truthful here.
            Self::Unconfigured => Begin::Rejected,
        }
    }

    fn poll(&mut self, _ticket: Ticket, _now_ms: u64) -> Poll {
        Poll::Failed
    }

    fn cancel(&mut self, _ticket: Ticket) {}
}

/// Help's host state: one authority and one executor, both owned here.
pub struct HelpState {
    authority: Mutex<Authority>,
    executor: Mutex<Executor<DesktopProvider>>,
}

/// Milliseconds since the Unix epoch.
///
/// The executor takes time as a parameter rather than reading a clock, so it
/// stays deterministic under test; the clock is read once, here, at the edge.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

impl HelpState {
    /// Build Help's state from the corpus this binary was compiled against.
    ///
    /// # Panics
    /// Panics if the embedded corpus fails its own digests. A desktop that
    /// started anyway would be serving content whose provenance it cannot
    /// establish, which is worse than not starting.
    #[must_use]
    pub fn new() -> Self {
        let corpus = grokptah_help_contract::embedded_corpus()
            .expect("the embedded Help corpus matches its digests");
        let mut authority =
            Authority::new(corpus).expect("the embedded Help corpus is self-consistent");

        // The desktop runs as a single local principal. It is registered here,
        // by the host, from what the host knows — never from anything the
        // renderer sent.
        authority.register_session(SessionRecord {
            token: LOCAL_SESSION_TOKEN.to_string(),
            session_id: "help-local".to_string(),
            principal_id: "help-local-user".to_string(),
            tenant_id: "local".to_string(),
            kind: PrincipalKind::Member,
            capabilities: std::collections::BTreeSet::new(),
            visibility_ceiling: Visibility::Public,
        });

        Self {
            authority: Mutex::new(authority),
            executor: Mutex::new(Executor::new(Bounds::default(), DesktopProvider::Unconfigured)),
        }
    }

    /// The opaque token the renderer is handed for the local session.
    #[must_use]
    pub fn local_session_token() -> &'static str {
        LOCAL_SESSION_TOKEN
    }
}

impl Default for HelpState {
    fn default() -> Self {
        Self::new()
    }
}

/// The desktop's single local session token.
///
/// Opaque to the renderer: it names a row in the host's session table and
/// carries no principal, tenant, capability, or ceiling of its own. Editing it
/// resolves to nothing, which is a denial, not a promotion.
const LOCAL_SESSION_TOKEN: &str = "help-local-session";

/// Ask Help a question.
///
/// Every refusal collapses to the same coarse projection. Distinguishing
/// "revoked" from "no such source" in the reply would let a caller map what
/// exists by asking about it.
#[tauri::command]
pub fn help_ask(state: tauri::State<'_, HelpState>, ask: HelpAsk) -> HelpProjection {
    let now = now_ms();
    let mut authority = state.authority.lock().expect("help authority lock");
    let mut executor = state.executor.lock().expect("help executor lock");

    let Some(principal) = authority.principal_for(&ask.session) else {
        return project_unavailable("", PublicErrorCode::NotAvailable);
    };

    // Retrieval is the host's: the renderer never names a chunk. Here it is
    // the whole visible corpus, bounded by the executor's context budget; the
    // authority filters it again when the request is built.
    let visible = authority.visible_corpus(&principal);
    let chunk_ids: Vec<String> =
        visible.chunks.iter().take(MAX_CONTEXT_CHUNKS).map(|chunk| chunk.id.clone()).collect();

    let locale = ask.locale.clone().unwrap_or_else(|| "en".to_string());
    let grant = authority.issue_grant(&principal, now, GRANT_TTL_MS);
    let Ok(request) = authority.build_request(&principal, &ask.question, &locale, &chunk_ids)
    else {
        return project_unavailable("", PublicErrorCode::NotAvailable);
    };

    match executor.submit(&mut authority, &ask.session, grant, request, now) {
        Ok(identity) => {
            executor.tick(&authority, now);
            projection_for(&executor, &authority, &identity.handle, now)
        }
        Err(SubmitError::Saturated) => project_unavailable("", PublicErrorCode::Busy),
        Err(SubmitError::Denied(_)) => project_unavailable("", PublicErrorCode::NotAvailable),
    }
}

/// Poll an in-flight ask.
#[tauri::command]
pub fn help_follow(state: tauri::State<'_, HelpState>, follow: HelpFollow) -> HelpProjection {
    let now = now_ms();
    let authority = state.authority.lock().expect("help authority lock");
    let mut executor = state.executor.lock().expect("help executor lock");
    if !owns_handle(&executor, &follow.session, &follow.handle) {
        return project_unavailable(&follow.handle, PublicErrorCode::NotAvailable);
    }
    executor.tick(&authority, now);
    projection_for(&executor, &authority, &follow.handle, now)
}

/// Ask the host to stop an in-flight ask.
///
/// Returning here means the request was recorded. The run becomes `Cancelled`
/// only once the host observes the provider quiesce; until then it is still
/// draining and the projection says `running`.
#[tauri::command]
pub fn help_cancel(
    state: tauri::State<'_, HelpState>,
    cancel: HelpCancelRequest,
) -> HelpProjection {
    let now = now_ms();
    let authority = state.authority.lock().expect("help authority lock");
    let mut executor = state.executor.lock().expect("help executor lock");
    if !owns_handle(&executor, &cancel.session, &cancel.handle) {
        return project_unavailable(&cancel.handle, PublicErrorCode::NotAvailable);
    }
    executor.cancel(&cancel.handle, now);
    executor.tick(&authority, now);
    projection_for(&executor, &authority, &cancel.handle, now)
}

/// The executor's fixed bounds.
#[tauri::command]
pub fn help_bounds(state: tauri::State<'_, HelpState>) -> BoundsProjection {
    let executor = state.executor.lock().expect("help executor lock");
    executor.bounds().projection()
}

/// The corpus this session is entitled to, filtered by the host.
#[tauri::command]
pub fn help_visible_corpus(state: tauri::State<'_, HelpState>, session: String) -> Corpus {
    let authority = state.authority.lock().expect("help authority lock");
    authority.principal_for(&session).map_or_else(
        // An unknown session sees an empty corpus, not an error naming what it
        // failed to prove.
        || empty_corpus(),
        |principal| authority.visible_corpus(&principal),
    )
}

/// The opaque local session token, handed to the renderer at startup.
#[tauri::command]
pub fn help_session() -> String {
    HelpState::local_session_token().to_string()
}

/// How many corpus chunks may accompany one question.
const MAX_CONTEXT_CHUNKS: usize = 24;
/// How long a Help grant stays valid.
const GRANT_TTL_MS: u64 = 120_000;

fn empty_corpus() -> Corpus {
    let mut corpus = grokptah_help_contract::build_corpus();
    corpus.articles.clear();
    corpus.chunks.clear();
    corpus.sources.clear();
    corpus
}

/// Whether `handle` belongs to `session`.
///
/// A handle is opaque, but opaque is not the same as unguessable. Ownership is
/// checked so one session cannot follow or cancel another's ask by presenting
/// a handle it happened to learn.
fn owns_handle(executor: &Executor<DesktopProvider>, session: &str, handle: &str) -> bool {
    executor.run(handle).is_some_and(|run| run.identity.session_token == session)
}

/// Build the renderer's view of a run.
fn projection_for(
    executor: &Executor<DesktopProvider>,
    authority: &Authority,
    handle: &str,
    _now_ms: u64,
) -> HelpProjection {
    let Some(run) = executor.run(handle) else {
        return project_unavailable(handle, PublicErrorCode::NotAvailable);
    };
    let status = status_for(run.state);
    if let Some(reply) = run.reply.as_deref() {
        // Validation happens here, in the host, against the corpus this
        // process was built with — not against whatever the renderer holds.
        return match validate::validate(reply, &run.request, authority.corpus()) {
            Ok(validation) => project(handle, &validation.answer, authority.corpus(), status),
            Err(_) => project_unavailable(handle, PublicErrorCode::NotAvailable),
        };
    }
    match run.state {
        RunState::Queued => HelpProjection {
            handle: handle.to_string(),
            status: ProjectionStatus::Queued,
            claims: Vec::new(),
            error: None,
            message: None,
        },
        RunState::Running | RunState::Draining => HelpProjection {
            handle: handle.to_string(),
            status: ProjectionStatus::Running,
            claims: Vec::new(),
            error: None,
            message: None,
        },
        _ => project_unavailable(
            handle,
            run.public_code().unwrap_or(PublicErrorCode::NotAvailable),
        ),
    }
}
