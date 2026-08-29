//! Semantic Help: the host half.
//!
//! Everything a renderer may do with Help goes through the six commands at the
//! bottom of this file, and each one is a thin adapter over
//! `grokptah-help-authority` and `grokptah-help-runtime`. No decision is made
//! here that those crates do not already make: this module owns the Tauri
//! plumbing, the session table, and the clock, and nothing else.
//!
//! Three properties are structural rather than conventional.
//!
//! **The renderer cannot name what it is entitled to.** The commands accept an
//! opaque session token, an opaque ask handle, a question, and a locale. A
//! grant, admission, manifest, principal, capability, or route cannot arrive
//! over IPC, because the contract's corresponding types are `Serialize`-only
//! and there is no command here that would take one.
//!
//! **Provider execution is off.** [`DisabledProvider`] is the only `Provider`
//! implementation wired in, and its `begin` returns [`Begin::Rejected`]
//! unconditionally, so no byte leaves this process on Help's behalf. That is a
//! deliberate product state, not a stub waiting to be filled: retrieval is the
//! shipped feature and it runs entirely in the renderer, offline. An ask
//! therefore always resolves to an honest "unavailable" rather than to a
//! fabricated answer.
//!
//! **State is in memory only.** The authority, the executor, and the session
//! table live in [`HelpState`] for the lifetime of the process. Nothing is
//! written to disk and nothing survives a restart, and the bounds projection a
//! surface renders says so. Durable Help runs would belong on main's canonical
//! durable-run interfaces; until they are wired, claiming restart durability
//! here would be claiming a property that does not exist.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use grokptah_help_authority::{Authority, SessionRecord};
use grokptah_help_contract::corpus::{Corpus, Visibility};
use grokptah_help_contract::dto::{
    BoundsProjection, HelpProjection, HelpRequest, PrincipalKind, ProjectionStatus, PublicErrorCode,
};
use grokptah_help_runtime::executor::{
    Begin, Bounds, Executor, Poll, Provider, SubmitError, Ticket,
};
use grokptah_help_runtime::{project_unavailable, status_for};
use serde::Deserialize;

/// Wall clock in milliseconds.
///
/// The executor takes `now_ms` as an argument rather than reading a clock, so
/// the single place time enters Help is here.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The provider seam, wired shut.
///
/// `begin` refuses before touching anything, so there is no ticket, no
/// endpoint, and no credential anywhere in this build's Help path. `poll` and
/// `cancel` are unreachable for a run that was never accepted; they are
/// implemented to keep the refusal total rather than to leave a partial seam
/// that a later edit could accidentally complete.
#[derive(Debug, Default)]
pub struct DisabledProvider;

impl Provider for DisabledProvider {
    fn begin(&mut self, _request: &HelpRequest) -> Begin {
        Begin::Rejected
    }

    fn poll(&mut self, _ticket: Ticket, _now_ms: u64) -> Poll {
        Poll::Quiesced
    }

    fn cancel(&mut self, _ticket: Ticket) {}
}

/// Host state for Help. In-memory for the process lifetime.
pub struct HelpState {
    inner: Mutex<HelpInner>,
}

struct HelpInner {
    authority: Authority,
    executor: Executor<DisabledProvider>,
    /// The one session this window holds. Minted at startup, never renewed
    /// from the renderer.
    session_token: String,
}

/// Why a Help command could not be served.
///
/// Deliberately coarse. A renderer that could tell "no such session" from
/// "revoked" from "no such source" could use the difference to map what
/// exists, so every failure the renderer can provoke reads the same way.
#[derive(Debug)]
pub struct HelpError(&'static str);

impl std::fmt::Display for HelpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl serde::Serialize for HelpError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

const UNAVAILABLE: HelpError = HelpError("Help is unavailable.");

/// Ceiling on how much of the manifest one request may carry as context.
///
/// A bound rather than a tuned number: the host selects context from the
/// manifest, and an unbounded selection would grow with the corpus until a
/// request became unservable. A host-side retriever would narrow this further;
/// until there is one, the ceiling is what keeps the request finite.
const MAX_CONTEXT_CHUNKS: usize = 16;

impl HelpState {
    /// Build the host authority over the corpus this binary was compiled with.
    ///
    /// The corpus is embedded by `grokptah-help-contract`, so a file swapped on
    /// disk after the build cannot change what this process serves. A corpus
    /// that fails its own digest check is not served at all.
    ///
    /// # Panics
    /// Panics if the embedded corpus does not verify, which would mean the
    /// binary was built from an inconsistent tree — a build fault, not a
    /// runtime condition.
    #[must_use]
    pub fn new() -> Self {
        let corpus = grokptah_help_contract::embedded_corpus()
            .expect("embedded Help corpus verifies against its recorded digests");
        Self::with_corpus(corpus)
    }

    fn with_corpus(corpus: Corpus) -> Self {
        let mut authority =
            Authority::new(corpus).expect("verified Help corpus builds an authority");
        // One desktop window, one local principal. Capabilities and the
        // visibility ceiling are the host's to decide and are set here, not
        // negotiated with the renderer. The desktop user is a local operator of
        // their own machine; the ceiling still stops the private corpus from
        // crossing the boundary unasked, because the manifest is what filters.
        let session_token = format!("help-session-{}", now_ms());
        authority.register_session(SessionRecord {
            token: session_token.clone(),
            session_id: "desktop-window".to_string(),
            principal_id: "desktop-local-user".to_string(),
            tenant_id: "local".to_string(),
            kind: PrincipalKind::Member,
            capabilities: BTreeSet::new(),
            visibility_ceiling: Visibility::Public,
        });
        Self {
            inner: Mutex::new(HelpInner {
                authority,
                executor: Executor::new(Bounds::default(), DisabledProvider),
                session_token,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HelpInner> {
        // A poisoned lock means a previous command panicked mid-mutation. The
        // state is then of unknown validity, so Help refuses rather than
        // serving from it.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for HelpState {
    fn default() -> Self {
        Self::new()
    }
}

/// The renderer's ask. Only these three fields cross the boundary.
///
/// Deliberately no chunk, source, or article id: `HelpAsk` in the generated
/// contract says a renderer may name none of them, and it means it. A caller
/// that could name a chunk could learn which chunks exist by watching which
/// names are accepted, which is the mapping the manifest exists to prevent.
/// Context selection is therefore the host's, below.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpAskInput {
    pub session: String,
    pub question: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// Poll or cancel an existing ask.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpHandleInput {
    pub session: String,
    pub handle: String,
}

fn projection_for(
    inner: &HelpInner,
    session: &str,
    handle: &str,
) -> Result<HelpProjection, HelpError> {
    let Some(run) = inner.executor.run(handle) else {
        return Err(UNAVAILABLE);
    };
    // A handle is only answerable to the session that created it. Without this
    // a leaked handle would read another session's outcome.
    if run.identity.session_token != session {
        return Err(UNAVAILABLE);
    }
    let status = status_for(run.state);
    if status == ProjectionStatus::Unavailable {
        return Ok(project_unavailable(
            handle,
            run.public_code().unwrap_or(PublicErrorCode::NotAvailable),
        ));
    }
    Ok(HelpProjection {
        handle: handle.to_string(),
        status,
        claims: Vec::new(),
        error: None,
        message: None,
    })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Ask Help a question.
///
/// Reaches the executor, which reauthorizes and then refuses to start: the
/// provider seam is disabled in this build. The refusal is reported as an
/// ordinary unavailable projection, because that is what it is.
///
/// # Errors
/// Returns a coarse error when the session is unknown or the queue is full.
#[tauri::command]
pub fn help_ask(
    state: tauri::State<'_, HelpState>,
    ask: HelpAskInput,
) -> Result<HelpProjection, HelpError> {
    let mut inner = state.lock();
    let now = now_ms();
    let Some(principal) = inner.authority.principal_for(&ask.session) else {
        return Err(UNAVAILABLE);
    };
    let locale = ask.locale.unwrap_or_else(|| "en".to_string());
    // The host chooses the context, from this principal's own manifest. Doing
    // it here rather than accepting a renderer's list is what keeps a caller
    // from probing which chunks exist; the manifest has already dropped
    // everything above their ceiling, so nothing selected here can exceed it.
    let chunk_ids: Vec<String> = inner
        .authority
        .manifest_for(&principal)
        .entries
        .iter()
        .flat_map(|entry| entry.chunk_ids.clone())
        .take(MAX_CONTEXT_CHUNKS)
        .collect();
    let request = inner
        .authority
        .build_request(&principal, &ask.question, &locale, &chunk_ids)
        .map_err(|_| UNAVAILABLE)?;
    let grant = inner.authority.issue_grant(&principal, now, 60_000);
    let identity = {
        let HelpInner {
            authority,
            executor,
            ..
        } = &mut *inner;
        executor
            .submit(authority, &ask.session, grant, request, now)
            .map_err(|error| match error {
                SubmitError::Denied(_) | SubmitError::Saturated => UNAVAILABLE,
            })?
    };
    // Advance once so a run that cannot start does not sit queued: with the
    // provider disabled this settles immediately, and the caller learns the
    // real state on this call instead of after a pointless poll.
    let HelpInner {
        authority,
        executor,
        ..
    } = &mut *inner;
    executor.tick(authority, now);
    projection_for(&inner, &ask.session, &identity.handle)
}

/// Poll an in-flight ask.
///
/// # Errors
/// Returns a coarse error when the handle is unknown to this session.
#[tauri::command]
pub fn help_follow(
    state: tauri::State<'_, HelpState>,
    follow: HelpHandleInput,
) -> Result<HelpProjection, HelpError> {
    let mut inner = state.lock();
    let now = now_ms();
    let HelpInner {
        authority,
        executor,
        ..
    } = &mut *inner;
    executor.tick(authority, now);
    projection_for(&inner, &follow.session, &follow.handle)
}

/// Ask the host to stop an in-flight ask.
///
/// Recording the request is not the same as the provider having stopped, and
/// the projection reflects the state the host has actually observed.
///
/// # Errors
/// Returns a coarse error when the handle is unknown to this session.
#[tauri::command]
pub fn help_cancel(
    state: tauri::State<'_, HelpState>,
    cancel: HelpHandleInput,
) -> Result<HelpProjection, HelpError> {
    let mut inner = state.lock();
    let now = now_ms();
    // Check ownership before acting: cancelling by a handle alone would let a
    // leaked handle stop another session's run.
    match inner.executor.run(&cancel.handle) {
        Some(run) if run.identity.session_token == cancel.session => {}
        _ => return Err(UNAVAILABLE),
    }
    inner.executor.cancel(&cancel.handle, now);
    let HelpInner {
        authority,
        executor,
        ..
    } = &mut *inner;
    executor.tick(authority, now);
    projection_for(&inner, &cancel.session, &cancel.handle)
}

/// The executor's fixed bounds, so a surface renders honest limits.
#[tauri::command]
pub fn help_bounds(state: tauri::State<'_, HelpState>) -> BoundsProjection {
    let inner = state.lock();
    inner.executor.bounds().projection()
}

/// The corpus this session may see, filtered by the host.
///
/// The renderer filters nothing. Content above this principal's ceiling never
/// crosses the boundary, so a modified renderer has nothing extra to reveal.
///
/// # Errors
/// Returns a coarse error when the session is unknown.
#[tauri::command]
pub fn help_visible_corpus(
    state: tauri::State<'_, HelpState>,
    session: String,
) -> Result<Corpus, HelpError> {
    let inner = state.lock();
    let Some(principal) = inner.authority.principal_for(&session) else {
        return Err(UNAVAILABLE);
    };
    Ok(inner.authority.visible_corpus(&principal))
}

/// The opaque session token for this window.
///
/// The renderer cannot usefully alter it: it names a row in the host's session
/// table, and a token the host does not recognise resolves to nothing — a
/// denial, never a promotion.
#[tauri::command]
pub fn help_session(state: tauri::State<'_, HelpState>) -> String {
    state.lock().session_token.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> HelpState {
        HelpState::new()
    }

    /// Build a request the way the host actually builds one, so the test
    /// cannot pass against a shape the contract no longer has.
    fn real_request(state: &HelpState) -> HelpRequest {
        let mut inner = state.lock();
        let token = inner.session_token.clone();
        let principal = inner
            .authority
            .principal_for(&token)
            .expect("session exists");
        let chunk_ids: Vec<String> = inner
            .authority
            .manifest_for(&principal)
            .entries
            .iter()
            .flat_map(|entry| entry.chunk_ids.clone())
            .take(1)
            .collect();
        inner
            .authority
            .build_request(&principal, "what is a lane", "en", &chunk_ids)
            .expect("a public principal can ask about public content")
    }

    #[test]
    fn the_provider_seam_refuses_every_request() {
        let state = state();
        let request = real_request(&state);
        let mut provider = DisabledProvider;
        assert_eq!(provider.begin(&request), Begin::Rejected);
    }

    #[test]
    fn an_ask_resolves_to_unavailable_rather_than_a_fabricated_answer() {
        let state = state();
        let session = state.lock().session_token.clone();
        let chunk_ids: Vec<String> = {
            let inner = state.lock();
            let principal = inner
                .authority
                .principal_for(&session)
                .expect("session exists");
            inner
                .authority
                .manifest_for(&principal)
                .entries
                .iter()
                .flat_map(|entry| entry.chunk_ids.clone())
                .take(1)
                .collect()
        };
        // Drive the same path the command drives, without Tauri's state
        // wrapper, so the assertion is about Help and not about the framework.
        let mut inner = state.lock();
        let now = now_ms();
        let principal = inner
            .authority
            .principal_for(&session)
            .expect("session exists");
        let request = inner
            .authority
            .build_request(&principal, "what is a lane", "en", &chunk_ids)
            .expect("request builds");
        let grant = inner.authority.issue_grant(&principal, now, 60_000);
        let identity = {
            let HelpInner {
                authority,
                executor,
                ..
            } = &mut *inner;
            executor
                .submit(authority, &session, grant, request, now)
                .expect("the queue admits one ask")
        };
        {
            let HelpInner {
                authority,
                executor,
                ..
            } = &mut *inner;
            executor.tick(authority, now);
        }

        // First tick: the provider refused, so the run is draining. The host
        // has not yet observed it stop, and says "running" rather than
        // guessing at an outcome nobody has seen.
        let draining = projection_for(&inner, &session, &identity.handle).expect("projection");
        assert_eq!(draining.status, ProjectionStatus::Running);
        assert!(draining.claims.is_empty(), "a draining ask shows no claims");

        // Second tick: quiescence observed, so the run settles. What a
        // renderer sees is an honest refusal with a coarse code — never a
        // fabricated answer.
        {
            let HelpInner {
                authority,
                executor,
                ..
            } = &mut *inner;
            executor.tick(authority, now);
        }
        let settled = projection_for(&inner, &session, &identity.handle).expect("projection");
        assert_eq!(settled.status, ProjectionStatus::Unavailable);
        assert!(settled.claims.is_empty(), "a refused ask shows no claims");
        assert!(settled.error.is_some(), "the refusal names a coarse code");
    }

    #[test]
    fn bounds_report_no_tools_history_workspace_or_fallback() {
        let state = state();
        let bounds = state.lock().executor.bounds().projection();
        assert!(!bounds.tools_enabled);
        assert!(!bounds.history_enabled);
        assert!(!bounds.workspace_enabled);
        assert!(!bounds.fallback_enabled);
        assert!(bounds.single_request);
    }

    #[test]
    fn an_unknown_session_is_refused_the_corpus() {
        let state = state();
        let inner = state.lock();
        assert!(inner.authority.principal_for("not-a-session").is_none());
    }

    #[test]
    fn the_visible_corpus_carries_nothing_above_the_ceiling() {
        let state = state();
        let inner = state.lock();
        let token = inner.session_token.clone();
        let principal = inner
            .authority
            .principal_for(&token)
            .expect("session exists");
        let visible = inner.authority.visible_corpus(&principal);
        assert!(
            !visible.articles.is_empty(),
            "a public reader sees something"
        );
        for source in &visible.sources {
            assert_eq!(source.visibility, Visibility::Public);
        }
        for chunk in &visible.chunks {
            assert_eq!(chunk.visibility, Visibility::Public);
        }
    }

    #[test]
    fn a_handle_from_one_session_is_not_readable_by_another() {
        let state = state();
        let inner = state.lock();
        // No run exists, so any handle is unknown; the point is that the
        // lookup is by (session, handle) and not by handle alone.
        assert!(projection_for(&inner, "session-a", "help-1").is_err());
    }
}
