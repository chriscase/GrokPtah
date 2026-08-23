//! Authoritative Computer Run projection shared by the desktop GUI and any
//! coordinator surface (#271).
//!
//! The durable [`ComputerRun`] record carries observation detail that is safe
//! for the local operator cockpit but must never leave the host: semantic
//! element labels and values can contain arbitrary document text, and evidence
//! asset identifiers are capability tokens for the backend byte store.
//! Backend-chosen action summaries and error messages are the same class of
//! payload: a native adapter can put observed text in those strings.
//!
//! This module derives a **redaction-safe** serialized view from that record.
//! The GUI embeds the same projection a coordinator receives, so the two
//! surfaces cannot disagree about run state, control disposition, control
//! epoch, progress, or the durable event range. Anything a coordinator is not
//! allowed to observe is absent from the type itself rather than filtered at
//! the transport boundary. Which runs a surface may list is a separate
//! authorization question: the local cockpit is session-scoped;
//! coordinator reads take a [`super::reads::ComputerReadBinding`].

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::types::{
    ActionClass, ComputerCapabilities, ComputerControlDisposition, ComputerError,
    ComputerErrorCode, ComputerRun, ComputerRunState, ComputerUseLimits, GrantIssuer,
    SemanticAction, Sensitivity,
};

/// Hard ceiling on one event page regardless of the requested limit.
pub const MAX_EVENT_PAGE: usize = 500;
/// Default event page size when a caller does not request one.
pub const DEFAULT_EVENT_PAGE: usize = 100;

/// Non-sensitive target identity. `display_name` is validated as an
/// application label, never a document or window title, and `window_id` is an
/// adapter-issued opaque handle rather than an OS pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTargetSummary {
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub display_name: String,
    pub sensitivity: Sensitivity,
}

/// Bounded description of the authority currently attached to a run. The grant
/// identifier is included so an operator and a coordinator can correlate the
/// same durable authorization; no target-scoped secret is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionGrantSummary {
    pub grant_id: String,
    pub action_classes: BTreeSet<ActionClass>,
    pub issued_by: GrantIssuer,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub uses_remaining: Option<u32>,
    pub revoked: bool,
    pub expired: bool,
}

/// Safe observation metadata. Element roles, labels, values, geometry, and the
/// evidence asset token are deliberately absent: a coordinator may learn that
/// an observation exists and how large it is, never what it contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationSummary {
    pub observation_id: String,
    pub sequence: u64,
    pub captured_at: DateTime<Utc>,
    pub element_count: u32,
    pub elements_truncated: bool,
    pub sensitivity: Sensitivity,
    pub has_screenshot: bool,
    pub screenshot_redacted: Option<bool>,
    /// True once the observation is older than the run's staleness bound, at
    /// which point the policy layer refuses to act on it.
    pub stale: bool,
    pub surface_id: String,
    pub frame_epoch: u64,
}

/// Safe last-action result. Backend-chosen summary text is deliberately
/// absent: it is an unbounded string a native adapter can fill with observed
/// document or accessibility content. A coordinator may learn that an action
/// completed and whether its expected postcondition held, never what the
/// backend wrote about the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutcomeSummary {
    pub expected_postcondition_met: Option<bool>,
}

/// Safe last-error view. The code is a closed enum; the backend-chosen
/// message is absent for the same reason as [`ActionOutcomeSummary`]:
/// [`ComputerAuditEntry`] already projects `error_code` without a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerErrorSummary {
    pub code: ComputerErrorCode,
}

/// Bounded progress counters against the run's own limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunProgress {
    pub action_count: u32,
    pub max_actions: u32,
    pub evidence_bytes: u64,
    pub max_evidence_bytes: u64,
    pub elapsed_millis: Option<i64>,
    pub max_duration_secs: u64,
    pub duration_exceeded: bool,
}

/// Durable sequence range currently retained for a run's event journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunEventRange {
    pub start_seq: u64,
    pub end_seq: u64,
}

/// The authoritative serialized Computer Run view.
///
/// Produced by [`project_run_at`] and consumed identically by the desktop
/// cockpit and any coordinator surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunProjection {
    pub run_id: String,
    pub owner_session_id: Uuid,
    pub parent_run_id: Option<String>,
    pub campaign_id: Option<String>,
    pub target: ComputerTargetSummary,
    pub state: ComputerRunState,
    pub control_disposition: ComputerControlDisposition,
    pub control_epoch: u64,
    pub version: u64,
    /// True while the backend is mid-observation or mid-action. This is the
    /// signal an "agent is using the computer" indicator must render.
    pub agent_active: bool,
    pub terminal: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub progress: ComputerRunProgress,
    pub grant: Option<ActionGrantSummary>,
    pub observation: Option<ObservationSummary>,
    pub last_outcome: Option<ActionOutcomeSummary>,
    pub last_error: Option<ComputerErrorSummary>,
    pub event_range: Option<ComputerRunEventRange>,
    pub capability_tier: super::types::ComputerCapabilityTier,
    pub surface_id: String,
    pub surface_incarnation: String,
    pub authority_epoch: u64,
    pub initiating_principal_kind: String,
}

/// Exact opaque handles an owning local operator needs to reconcile one
/// uncertain physical dispatch. This projection is emitted only by the local
/// cockpit path; it carries no target content, evidence token, or outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerUncertainSurfaceLease {
    pub lease_id: String,
    pub expected_revision: u64,
    pub surface_id: String,
    pub incarnation: String,
}

/// Local-operator view of one Agent Run's position in the host-owned physical
/// input queue. This deliberately omits lease IDs, revisions, dispatch IDs,
/// conflict-domain IDs, frame epochs, and WorkAttempt IDs: none of those
/// capability/fence handles are required to explain ownership to a person.
///
/// The desktop may show this after its session-ownership gate. Coordinator
/// reads do not receive it because queue depth and another Agent's identity
/// would otherwise become a cross-workspace activity oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerSurfaceCoordination {
    pub state: ComputerSurfaceCoordinationState,
    /// One-based position among currently live queued waiters. Absent after a
    /// grant, during dispatch, and for an uncertain physical outcome.
    pub queue_position: Option<u32>,
    pub queue_depth: u32,
    pub owns_surface: bool,
    pub blocked_by_uncertain_outcome: bool,
    /// Current capacity owner, if the domain has a granted/dispatching lease.
    pub active: Option<ComputerSurfaceOccupant>,
    pub expires_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerSurfaceCoordinationState {
    Queued,
    Granted,
    Dispatching,
    Uncertain,
}

/// Non-secret local-operator identity for the Agent currently holding a
/// physical input conflict domain. WorkAttempt and authority handles remain
/// absent; the stable Agent, Work, and Run IDs are sufficient for diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerSurfaceOccupant {
    pub agent_id: String,
    pub work_id: String,
    pub run_id: String,
}

/// One bounded, cursor-addressed page of a run's durable event journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunEventPage {
    pub run_id: String,
    pub entries: Vec<super::types::ComputerAuditEntry>,
    /// Sequence to pass as the next `after_seq`. `None` means the caller has
    /// reached the end of the currently retained journal.
    pub next_cursor: Option<u64>,
    /// True when the requested cursor is older than the retained window, so
    /// entries between the cursor and `range.start_seq` are permanently gone.
    /// A gap is never silently presented as a complete stream.
    pub cursor_expired: bool,
    /// The range still retained at the time the page was produced.
    pub range: Option<ComputerRunEventRange>,
}

/// Local-operator ledger occupancy. Host-wide `stored_runs` / `active_runs`
/// are for the desktop cockpit only and must never be serialized on a
/// coordinator surface after a workspace gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunCapacity {
    pub max_run_records: u32,
    pub stored_runs: u32,
    pub active_runs: u32,
    pub session_runs: u32,
    pub session_active_runs: u32,
}

/// Coordinator-facing capacity. Counts are scoped to the
/// [`super::reads::ComputerReadBinding`]; live host-wide occupancy is
/// absent so the tool cannot be used as a cross-scope activity oracle.
/// `max_run_records` is the constant ledger bound, not a live counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerScopeCapacity {
    pub max_run_records: u32,
    pub bound_runs: u32,
    pub bound_active_runs: u32,
}

/// Public backend flags for the local cockpit. Typed capability proofs stay
/// host-internal and must never ride a Tauri snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerBackendPublicView {
    pub backend_id: String,
    pub observe: bool,
    pub semantic_actions: bool,
    pub text_entry: bool,
    pub key_chords: bool,
    pub pointer_fallback: bool,
    /// Host-global foreground conflict-domain capacity. Stage 1 always
    /// reports 1. This is not N-surface isolation and is not run-record
    /// occupancy.
    pub foreground_conflict_capacity: u32,
}

impl ComputerBackendPublicView {
    pub fn from_capabilities(capabilities: &ComputerCapabilities) -> Self {
        Self {
            backend_id: capabilities.backend_id.clone(),
            observe: capabilities.observe,
            semantic_actions: capabilities.semantic_actions,
            text_entry: capabilities.text_entry,
            key_chords: capabilities.key_chords,
            pointer_fallback: capabilities.pointer_fallback,
            foreground_conflict_capacity: crate::computer_use::FOREGROUND_CONFLICT_DOMAIN_CAPACITY,
        }
    }
}

/// Allowlisted local observation element. Geometry, sensitivity, focus, and
/// evidence handles are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalElement {
    pub element_id: String,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub enabled: bool,
    pub actions: BTreeSet<SemanticAction>,
}

/// Local-only observation detail needed to render an approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalObservation {
    pub observation_id: String,
    pub sequence: u64,
    pub captured_at: DateTime<Utc>,
    pub elements: Vec<ComputerLocalElement>,
}

/// Allowlisted grant projection for the local cockpit. Principal, surface,
/// incarnation, and epoch handles stay internal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalGrant {
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub action_classes: BTreeSet<ActionClass>,
}

/// Allowlisted durable audit row. Backend-chosen messages are absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalAuditEntry {
    pub sequence: u64,
    pub at: DateTime<Utc>,
    pub operation: String,
    pub disposition: String,
    pub action_class: Option<ActionClass>,
    pub observation_id: Option<String>,
    pub error_code: Option<ComputerErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalTarget {
    pub app_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalLimits {
    pub max_actions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalError {
    pub code: ComputerErrorCode,
}

/// Purpose-built local approval/observation DTO. This is the only run detail
/// the desktop snapshot may serialize alongside the coordinator projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerLocalApproval {
    pub run_id: String,
    pub state: ComputerRunState,
    pub version: u64,
    pub action_count: u32,
    pub limits: ComputerLocalLimits,
    pub control_disposition: ComputerControlDisposition,
    pub target: ComputerLocalTarget,
    pub observation: Option<ComputerLocalObservation>,
    pub grant: Option<ComputerLocalGrant>,
    pub audit: Vec<ComputerLocalAuditEntry>,
    pub last_error: Option<ComputerLocalError>,
}

impl ComputerLocalApproval {
    pub fn from_run(run: &ComputerRun) -> Self {
        Self {
            run_id: run.run_id.clone(),
            state: run.state,
            version: run.version,
            action_count: run.action_count,
            limits: ComputerLocalLimits {
                max_actions: run.limits.max_actions,
            },
            control_disposition: run.control_disposition,
            target: ComputerLocalTarget {
                app_id: run.target.app_id.clone(),
                display_name: run.target.display_name.clone(),
            },
            observation: run.current_observation.as_ref().map(|observation| {
                ComputerLocalObservation {
                    observation_id: observation.observation_id.clone(),
                    sequence: observation.sequence,
                    captured_at: observation.captured_at,
                    elements: observation
                        .elements
                        .iter()
                        .map(|element| ComputerLocalElement {
                            element_id: element.element_id.clone(),
                            role: element.role.clone(),
                            label: element.label.clone(),
                            value: element.value.clone(),
                            enabled: element.enabled,
                            actions: element.actions.clone(),
                        })
                        .collect(),
                }
            }),
            grant: run.grant.as_ref().map(|grant| ComputerLocalGrant {
                expires_at: grant.expires_at,
                revoked_at: grant.revoked_at,
                action_classes: grant.action_classes.clone(),
            }),
            audit: run
                .audit
                .iter()
                .map(|entry| ComputerLocalAuditEntry {
                    sequence: entry.sequence,
                    at: entry.at,
                    operation: entry.operation.clone(),
                    disposition: entry.disposition.clone(),
                    action_class: entry.action_class,
                    observation_id: entry.observation_id.clone(),
                    error_code: entry.error_code,
                })
                .collect(),
            last_error: run
                .last_error
                .as_ref()
                .map(|error| ComputerLocalError { code: error.code }),
        }
    }
}

/// Ownership failure. Unknown runs and cross-session runs deliberately produce
/// the identical error so an unauthorized caller cannot probe run existence.
pub(super) fn not_available() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::Unauthorized,
        "computer run is not available to this session",
    )
}

/// Derive the authoritative projection at an explicit instant.
///
/// `now` is a parameter rather than an ambient clock read so the GUI and a
/// coordinator provably produce byte-identical output for one record, and so
/// staleness assertions are deterministic under test.
pub fn project_run_at(run: &ComputerRun, now: DateTime<Utc>) -> ComputerRunProjection {
    ComputerRunProjection {
        run_id: run.run_id.clone(),
        owner_session_id: run.owner_session_id,
        parent_run_id: run.parent_run_id.clone(),
        campaign_id: run.campaign_id.clone(),
        target: ComputerTargetSummary {
            app_id: run.target.app_id.clone(),
            window_id: run.target.window_id.clone(),
            generation: run.target.generation,
            display_name: run.target.display_name.clone(),
            sensitivity: run.target.sensitivity,
        },
        state: run.state,
        control_disposition: run.control_disposition,
        control_epoch: run.control_epoch,
        version: run.version,
        agent_active: matches!(
            run.state,
            ComputerRunState::Observing | ComputerRunState::Acting
        ),
        terminal: run.state.is_terminal(),
        created_at: run.created_at,
        updated_at: run.updated_at,
        started_at: run.started_at,
        ended_at: run.ended_at,
        progress: progress(run, now),
        grant: run.grant.as_ref().map(|grant| ActionGrantSummary {
            grant_id: grant.grant_id.clone(),
            action_classes: grant.action_classes.clone(),
            issued_by: grant.issued_by,
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
            uses_remaining: grant.uses_remaining,
            revoked: grant.revoked_at.is_some(),
            expired: grant.expires_at <= now,
        }),
        observation: run
            .current_observation
            .as_ref()
            .map(|observation| ObservationSummary {
                observation_id: projection_observation_id(&run.run_id, &observation.observation_id),
                sequence: observation.sequence,
                captured_at: observation.captured_at,
                element_count: observation.elements.len() as u32,
                elements_truncated: observation.elements_truncated,
                sensitivity: observation.sensitivity,
                has_screenshot: observation.screenshot.is_some(),
                screenshot_redacted: observation
                    .screenshot
                    .as_ref()
                    .map(|evidence| evidence.redacted),
                stale: observation_is_stale(observation.captured_at, &run.limits, now),
                surface_id: observation.authority.surface.surface_id.clone(),
                frame_epoch: observation.authority.frame_epoch,
            }),
        last_outcome: run
            .last_outcome
            .as_ref()
            .map(|outcome| ActionOutcomeSummary {
                expected_postcondition_met: outcome.expected_postcondition_met,
            }),
        last_error: run
            .last_error
            .as_ref()
            .map(|error| ComputerErrorSummary { code: error.code }),
        event_range: event_range(run),
        capability_tier: run.capability_proof.tier(),
        surface_id: run.surface.surface_id.clone(),
        surface_incarnation: run.surface.incarnation.clone(),
        authority_epoch: run.authority_epoch,
        initiating_principal_kind: run
            .initiating_principal
            .as_ref()
            .map(|principal| principal.public_kind().to_string())
            .unwrap_or_else(|| "unproven".to_string()),
    }
}

/// Durable records do not carry provenance for their observation IDs. Project
/// every ID through a run-scoped deterministic surrogate so even a legacy
/// backend that chose a UUID-shaped value cannot encode observed content in a
/// GUI/MCP projection. Local action paths retain the internal ID.
fn projection_observation_id(run_id: &str, observation_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update([0]);
    hasher.update(observation_id.as_bytes());
    format!("observation-ref-{:x}", hasher.finalize())
}

fn progress(run: &ComputerRun, now: DateTime<Utc>) -> ComputerRunProgress {
    ComputerRunProgress {
        action_count: run.action_count,
        max_actions: run.limits.max_actions,
        evidence_bytes: run.evidence_bytes,
        max_evidence_bytes: run.limits.max_evidence_bytes,
        elapsed_millis: run.started_at.map(|started| {
            run.ended_at
                .unwrap_or(now)
                .signed_duration_since(started)
                .num_milliseconds()
        }),
        max_duration_secs: run.limits.max_duration_secs,
        duration_exceeded: run.duration_exceeded(now),
    }
}

fn observation_is_stale(
    captured_at: DateTime<Utc>,
    limits: &ComputerUseLimits,
    now: DateTime<Utc>,
) -> bool {
    now.signed_duration_since(captured_at)
        > Duration::milliseconds(limits.max_observation_age_millis as i64)
}

pub(super) fn event_range(run: &ComputerRun) -> Option<ComputerRunEventRange> {
    let start_seq = run.audit.first()?.sequence;
    let end_seq = run.audit.last()?.sequence;
    Some(ComputerRunEventRange { start_seq, end_seq })
}

/// Build one bounded event page from the durable journal.
///
/// The journal is a bounded ring: once it reaches its retention limit the
/// oldest entries are dropped and their sequences never reappear. A caller
/// resuming from a cursor below the retained window therefore has a real gap,
/// which is reported as `cursor_expired` instead of being papered over by
/// returning whatever happens to still be present.
pub(super) fn project_events(
    run: &ComputerRun,
    after_seq: Option<u64>,
    limit: usize,
) -> ComputerRunEventPage {
    let limit = limit.clamp(1, MAX_EVENT_PAGE);
    let range = event_range(run);

    // A cursor is expired when it points strictly below the oldest retained
    // sequence. `after_seq == start_seq - 1` is exact continuity, not a gap.
    let cursor_expired = match (after_seq, range) {
        // `after_seq` is caller-supplied, so saturate rather than overflow.
        (Some(after), Some(range)) => after.saturating_add(1) < range.start_seq,
        // With nothing retained there is nothing to prove continuity against,
        // so an unanchored cursor is treated as still valid and simply empty.
        _ => false,
    };
    if cursor_expired {
        return ComputerRunEventPage {
            run_id: run.run_id.clone(),
            entries: Vec::new(),
            next_cursor: None,
            cursor_expired: true,
            range,
        };
    }

    let mut entries: Vec<_> = run
        .audit
        .iter()
        .filter(|entry| after_seq.is_none_or(|after| entry.sequence > after))
        .cloned()
        .map(|mut entry| {
            entry.observation_id = entry
                .observation_id
                .as_deref()
                .map(|observation_id| projection_observation_id(&run.run_id, observation_id));
            entry
        })
        .collect();
    let more = entries.len() > limit;
    entries.truncate(limit);
    let next_cursor = if more {
        entries.last().map(|entry| entry.sequence)
    } else {
        None
    };

    ComputerRunEventPage {
        run_id: run.run_id.clone(),
        entries,
        next_cursor,
        cursor_expired: false,
        range,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::computer_use::types::{
        ActionGrant, ActionOutcome, ComputerObservation, ComputerTarget, EvidenceRef,
        ObservationGeometry, SemanticElement,
    };

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn run() -> ComputerRun {
        ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default()).unwrap()
    }

    fn observation_with_secrets(target: &ComputerTarget) -> ComputerObservation {
        ComputerObservation {
            observation_id: "observation-01234567-89ab-cdef-0123-456789abcdef".into(),
            sequence: 7,
            target: target.clone(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 2.0,
            },
            screenshot: Some(EvidenceRef {
                content_sha256: "a".repeat(64),
                media_type: "image/png".into(),
                byte_len: 1024,
                width: 800,
                height: 600,
                redacted: true,
                asset_id: "SECRET_ASSET_TOKEN".into(),
            }),
            elements: vec![SemanticElement {
                element_id: "field".into(),
                role: "text_field".into(),
                label: Some("PRIVATE_DOCUMENT_LABEL".into()),
                value: Some("PRIVATE_DOCUMENT_VALUE".into()),
                bounds: None,
                enabled: true,
                focused: true,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::new(),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
            authority: Default::default(),
        }
    }

    #[test]
    fn projection_never_serializes_observation_contents_or_evidence_tokens() {
        let mut run = run();
        run.current_observation = Some(observation_with_secrets(&run.target.clone()));
        let encoded = serde_json::to_string(&project_run_at(&run, Utc::now())).unwrap();

        assert!(!encoded.contains("PRIVATE_DOCUMENT_LABEL"));
        assert!(!encoded.contains("PRIVATE_DOCUMENT_VALUE"));
        assert!(!encoded.contains("observation-01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!encoded.contains("SECRET_ASSET_TOKEN"));
        assert!(!encoded.contains(&"a".repeat(64)));
        // The safe metadata a coordinator legitimately needs is still present.
        assert!(encoded.contains("observation-ref-"));
        assert!(encoded.contains("\"hasScreenshot\":true"));
        assert!(encoded.contains("\"elementCount\":1"));
    }

    #[test]
    fn last_outcome_and_last_error_are_summary_types_not_durable_passthrough() {
        let mut run = run();
        run.last_outcome = Some(ActionOutcome::bounded(
            "PRIVATE_DOCUMENT_TITLE leaked from AX",
            Some(true),
        ));
        run.last_error = Some(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "PRIVATE_ERROR_DETAIL from observed value",
        ));
        let projection = project_run_at(&run, Utc::now());
        let encoded = serde_json::to_value(&projection).unwrap();
        let wire = serde_json::to_string(&projection).unwrap();

        assert!(!wire.contains("PRIVATE_DOCUMENT_TITLE"));
        assert!(!wire.contains("PRIVATE_ERROR_DETAIL"));
        assert!(!wire.contains("leaked from AX"));
        assert!(!wire.contains("from observed value"));

        assert_eq!(
            projection
                .last_outcome
                .as_ref()
                .and_then(|outcome| outcome.expected_postcondition_met),
            Some(true)
        );
        let outcome_keys: BTreeSet<&str> = encoded["lastOutcome"]
            .as_object()
            .expect("lastOutcome is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            outcome_keys,
            BTreeSet::from(["expectedPostconditionMet"]),
            "adding a field to ActionOutcomeSummary must consciously widen this pin"
        );

        assert_eq!(
            projection.last_error.as_ref().map(|error| error.code),
            Some(ComputerErrorCode::BackendFailure)
        );
        let error_keys: BTreeSet<&str> = encoded["lastError"]
            .as_object()
            .expect("lastError is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            error_keys,
            BTreeSet::from(["code"]),
            "adding a field to ComputerErrorSummary must consciously widen this pin"
        );
    }

    #[test]
    fn agent_active_tracks_only_inflight_backend_work() {
        let mut run = run();
        run.transition(ComputerRunState::Ready).unwrap();
        assert!(!project_run_at(&run, Utc::now()).agent_active);
        run.transition(ComputerRunState::Observing).unwrap();
        assert!(project_run_at(&run, Utc::now()).agent_active);
        run.transition(ComputerRunState::Ready).unwrap();
        run.transition(ComputerRunState::Acting).unwrap();
        assert!(project_run_at(&run, Utc::now()).agent_active);
        run.transition(ComputerRunState::Paused).unwrap();
        assert!(!project_run_at(&run, Utc::now()).agent_active);
    }

    #[test]
    fn projection_is_deterministic_for_one_record_and_instant() {
        let mut run = run();
        run.record_audit("create_run", "accepted", None, None, None);
        let now = Utc::now();
        assert_eq!(project_run_at(&run, now), project_run_at(&run, now));
    }

    #[test]
    fn grant_expiry_and_revocation_are_reported_without_target_secrets() {
        let mut run = run();
        let now = Utc::now();
        run.grant = Some(ActionGrant::for_run(
            &run,
            BTreeSet::from([ActionClass::Semantic]),
            now - Duration::minutes(10),
            now - Duration::minutes(5),
            Some(2),
        ));
        run.grant.as_mut().unwrap().revoked_at = Some(now);
        let grant = project_run_at(&run, now).grant.unwrap();
        assert!(grant.expired);
        assert!(grant.revoked);
        assert_eq!(grant.uses_remaining, Some(2));
    }

    #[test]
    fn observation_staleness_uses_the_run_bound() {
        let mut run = run();
        let now = Utc::now();
        let mut observation = observation_with_secrets(&run.target.clone());
        observation.captured_at = now - Duration::milliseconds(1);
        run.current_observation = Some(observation.clone());
        assert!(!project_run_at(&run, now).observation.unwrap().stale);

        observation.captured_at =
            now - Duration::milliseconds(run.limits.max_observation_age_millis as i64 + 1);
        run.current_observation = Some(observation);
        assert!(project_run_at(&run, now).observation.unwrap().stale);
    }

    #[test]
    fn event_pages_are_bounded_monotonic_and_terminate() {
        let mut run = run();
        for index in 0..250 {
            run.record_audit(&format!("op-{index}"), "accepted", None, None, None);
        }
        let first = project_events(&run, None, 100);
        assert_eq!(first.entries.len(), 100);
        assert!(!first.cursor_expired);
        assert!(first
            .entries
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence));
        let cursor = first.next_cursor.expect("more entries remain");

        let second = project_events(&run, Some(cursor), 500);
        assert_eq!(second.entries.len(), 150);
        assert_eq!(second.next_cursor, None, "final page must not loop");
        assert!(second.entries[0].sequence > cursor);
    }

    #[test]
    fn requested_limit_is_clamped_to_the_hard_ceiling() {
        let mut run = run();
        for index in 0..(MAX_EVENT_PAGE + 25) {
            run.record_audit(&format!("op-{index}"), "accepted", None, None, None);
        }
        assert_eq!(
            project_events(&run, None, usize::MAX).entries.len(),
            MAX_EVENT_PAGE
        );
        assert_eq!(project_events(&run, None, 0).entries.len(), 1);
    }

    #[test]
    fn cursor_below_the_retained_window_is_reported_as_expired() {
        let mut run = run();
        for index in 0..10 {
            run.record_audit(&format!("op-{index}"), "accepted", None, None, None);
        }
        // Simulate ring-buffer eviction of the oldest entries.
        run.audit.drain(0..5);
        let range = event_range(&run).unwrap();
        assert_eq!(range.start_seq, 6);

        let expired = project_events(&run, Some(1), 100);
        assert!(expired.cursor_expired);
        assert!(expired.entries.is_empty());
        assert_eq!(expired.next_cursor, None);
        assert_eq!(expired.range, Some(range));

        // Exact continuity with the retained window is not a gap.
        let continuous = project_events(&run, Some(range.start_seq - 1), 100);
        assert!(!continuous.cursor_expired);
        assert_eq!(continuous.entries.len(), 5);
    }

    #[test]
    fn event_entries_serialize_only_the_pinned_audit_keys() {
        let mut run = run();
        run.current_observation = Some(observation_with_secrets(&run.target.clone()));
        run.record_audit(
            "create_run",
            "accepted",
            Some(ActionClass::Semantic),
            Some("observation-01234567-89ab-cdef-0123-456789abcdef".into()),
            Some(ComputerErrorCode::Interrupted),
        );
        let page = project_events(&run, None, 10);
        let wire = serde_json::to_string(&page).unwrap();
        assert!(!wire.contains("observation-01234567-89ab-cdef-0123-456789abcdef"));
        assert!(wire.contains("observation-ref-"));
        assert_eq!(
            page.entries[0].observation_id.as_deref(),
            project_run_at(&run, Utc::now())
                .observation
                .as_ref()
                .map(|observation| observation.observation_id.as_str()),
            "run projections and event pages must use the same opaque mapping"
        );
        let encoded = serde_json::to_value(&page).unwrap();
        let keys: BTreeSet<&str> = encoded["entries"][0]
            .as_object()
            .expect("entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        // Event pages are the other coordinator-visible payload, so the exact
        // serialized audit-entry key set is pinned the same way the
        // observation keys are: a future field addition must consciously
        // widen this list rather than silently widen what MCP observes.
        assert_eq!(
            keys,
            BTreeSet::from([
                "sequence",
                "at",
                "operation",
                "disposition",
                "actionClass",
                "observationId",
                "errorCode",
            ])
        );
    }

    #[test]
    fn empty_journal_yields_no_range_and_no_false_expiry() {
        let run = run();
        let page = project_events(&run, Some(42), 100);
        assert!(page.entries.is_empty());
        assert!(!page.cursor_expired);
        assert_eq!(page.range, None);
        assert_eq!(project_run_at(&run, Utc::now()).event_range, None);
    }

    #[test]
    fn public_projection_pins_isolation_keys_and_hides_native_and_principal_secrets() {
        let mut run = run();
        run.initiating_principal = Some(
            serde_json::from_value(serde_json::json!({
                "kind": "agent",
                "agentId": format!("agent-{}", Uuid::new_v4()),
                "specRevision": 3
            }))
            .unwrap(),
        );
        run.current_observation = Some(observation_with_secrets(&run.target.clone()));
        let projection = project_run_at(&run, Utc::now());
        let encoded = serde_json::to_value(&projection).unwrap();
        let keys: BTreeSet<&str> = encoded
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(keys.contains("capabilityTier"));
        assert!(keys.contains("surfaceId"));
        assert!(keys.contains("surfaceIncarnation"));
        assert!(keys.contains("authorityEpoch"));
        assert!(keys.contains("initiatingPrincipalKind"));
        assert_eq!(
            projection.initiating_principal_kind, "agent",
            "projection may expose principal kind only"
        );
        let wire = serde_json::to_string(&projection).unwrap();
        assert!(!wire.contains("agent-"));
        assert!(!wire.contains("specRevision"));
        assert!(!wire.contains("process_id"));
        assert!(!wire.contains("processId"));
        assert!(!wire.contains("pid"));
        assert!(!wire.contains("CGWindow"));
        assert!(!wire.contains("attestation"));
        assert!(!wire.contains("input-domain-"));
        assert!(!wire.contains("measurement-"));
        let observation_keys: BTreeSet<&str> = encoded["observation"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            observation_keys,
            BTreeSet::from([
                "observationId",
                "sequence",
                "capturedAt",
                "elementCount",
                "elementsTruncated",
                "sensitivity",
                "hasScreenshot",
                "screenshotRedacted",
                "stale",
                "surfaceId",
                "frameEpoch",
            ])
        );
        assert_eq!(
            keys,
            BTreeSet::from([
                "runId",
                "ownerSessionId",
                "parentRunId",
                "campaignId",
                "target",
                "state",
                "controlDisposition",
                "controlEpoch",
                "version",
                "agentActive",
                "terminal",
                "createdAt",
                "updatedAt",
                "startedAt",
                "endedAt",
                "progress",
                "grant",
                "observation",
                "lastOutcome",
                "lastError",
                "eventRange",
                "capabilityTier",
                "surfaceId",
                "surfaceIncarnation",
                "authorityEpoch",
                "initiatingPrincipalKind",
            ])
        );
    }

    fn json_key_paths(value: &serde_json::Value) -> BTreeSet<String> {
        fn walk(prefix: &str, value: &serde_json::Value, out: &mut BTreeSet<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        let path = if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        };
                        out.insert(path.clone());
                        walk(&path, child, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    if let Some(first) = items.first() {
                        let path = format!("{prefix}[]");
                        walk(&path, first, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = BTreeSet::new();
        walk("", value, &mut out);
        out
    }

    fn populated_run() -> ComputerRun {
        let mut run = ComputerRun::attested_foreground_for_test(
            Uuid::new_v4(),
            Some("/secret/workspace".into()),
            target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
        run.current_observation = Some(observation_with_secrets(&run.target.clone()));
        let now = Utc::now();
        run.grant = Some(ActionGrant::for_run(
            &run,
            BTreeSet::from([ActionClass::Semantic]),
            now,
            now + Duration::minutes(5),
            Some(1),
        ));
        run.last_outcome = Some(ActionOutcome::bounded("BACKEND_TEXT_SUMMARY", Some(true)));
        run.last_error = Some(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "BACKEND_ERROR_MESSAGE",
        ));
        run.record_audit(
            "act",
            "denied",
            Some(ActionClass::Semantic),
            Some("observation-01234567-89ab-cdef-0123-456789abcdef".into()),
            Some(ComputerErrorCode::BackendFailure),
        );
        run
    }

    #[test]
    fn local_approval_dto_is_allowlisted_and_rejects_unknown_fields() {
        let run = populated_run();
        let local = ComputerLocalApproval::from_run(&run);
        let encoded = serde_json::to_value(&local).unwrap();
        assert_eq!(
            json_key_paths(&encoded),
            BTreeSet::from([
                "runId".into(),
                "state".into(),
                "version".into(),
                "actionCount".into(),
                "limits".into(),
                "limits.maxActions".into(),
                "controlDisposition".into(),
                "target".into(),
                "target.appId".into(),
                "target.displayName".into(),
                "observation".into(),
                "observation.observationId".into(),
                "observation.sequence".into(),
                "observation.capturedAt".into(),
                "observation.elements".into(),
                "observation.elements[].elementId".into(),
                "observation.elements[].role".into(),
                "observation.elements[].label".into(),
                "observation.elements[].value".into(),
                "observation.elements[].enabled".into(),
                "observation.elements[].actions".into(),
                "grant".into(),
                "grant.expiresAt".into(),
                "grant.revokedAt".into(),
                "grant.actionClasses".into(),
                "audit".into(),
                "audit[].sequence".into(),
                "audit[].at".into(),
                "audit[].operation".into(),
                "audit[].disposition".into(),
                "audit[].actionClass".into(),
                "audit[].observationId".into(),
                "audit[].errorCode".into(),
                "lastError".into(),
                "lastError.code".into(),
            ])
        );

        let wire = serde_json::to_string(&encoded).unwrap();
        for forbidden in [
            "workspace",
            "initiatingPrincipal",
            "capabilityProof",
            "SECRET_ASSET_TOKEN",
            "BACKEND_TEXT_SUMMARY",
            "BACKEND_ERROR_MESSAGE",
            "grantId",
            "usesRemaining",
            "surfaceId",
            "incarnation",
            "authorityEpoch",
            "controlEpoch",
            "ownerSessionId",
            "screenshot",
            "evidence",
            "lastOutcome",
        ] {
            assert!(
                !wire.contains(forbidden),
                "{forbidden} must not appear in the local approval DTO"
            );
        }
        assert!(wire.contains("PRIVATE_DOCUMENT_LABEL"));
        assert_eq!(
            local.last_error.as_ref().unwrap().code,
            ComputerErrorCode::BackendFailure
        );

        let mut with_workspace = encoded.clone();
        with_workspace
            .as_object_mut()
            .unwrap()
            .insert("workspace".into(), serde_json::json!("/secret/workspace"));
        assert!(serde_json::from_value::<ComputerLocalApproval>(with_workspace).is_err());

        let mut with_nested = encoded;
        with_nested["observation"]
            .as_object_mut()
            .unwrap()
            .insert("screenshot".into(), serde_json::json!({"assetId": "x"}));
        assert!(serde_json::from_value::<ComputerLocalApproval>(with_nested).is_err());
    }

    #[test]
    fn backend_public_view_omits_proof_and_rejects_unknown_fields() {
        let capabilities = ComputerCapabilities::from_proof(
            crate::computer_use::ComputerCapabilityProof::ForegroundSemantic {
                backend_id: crate::computer_use::SIMULATOR_FOREGROUND_BACKEND_ID.into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
            },
        )
        .unwrap();
        let view = ComputerBackendPublicView::from_capabilities(&capabilities);
        let encoded = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json_key_paths(&encoded),
            BTreeSet::from([
                "backendId".into(),
                "observe".into(),
                "semanticActions".into(),
                "textEntry".into(),
                "keyChords".into(),
                "pointerFallback".into(),
                "foregroundConflictCapacity".into(),
            ])
        );
        assert_eq!(
            view.foreground_conflict_capacity,
            crate::computer_use::FOREGROUND_CONFLICT_DOMAIN_CAPACITY
        );
        assert_eq!(view.foreground_conflict_capacity, 1);
        let wire = serde_json::to_string(&encoded).unwrap();
        assert!(!wire.contains("proof"));
        assert!(!wire.contains("tier"));
        let mut extra = encoded;
        extra
            .as_object_mut()
            .unwrap()
            .insert("proof".into(), serde_json::json!({"kind": "unproven"}));
        assert!(serde_json::from_value::<ComputerBackendPublicView>(extra).is_err());
    }
}
