//! The sealed model-output boundary (#457).
//!
//! Untrusted model output reaches a Computer Run through exactly one function,
//! [`accept_model_proposal`]. It returns an [`AcceptedModelProposal`]: an
//! unforgeable capability that is the *only* value any application seam will
//! act on. The capability has private fields, no public constructor, no
//! `Deserialize`, and is not `Clone`, so outside this module it can be obtained
//! only by passing raw bytes through the normalizer against a complete, live
//! context — and it can be spent only once.
//!
//! The seal is not a secret. It carries no key or MAC and nothing derived from
//! one; unforgeability comes from Rust's module privacy, and freshness comes
//! from re-validating every bound identity against the live run at application
//! time. That is deliberate: a durable secret would have to be stored, rotated,
//! and kept out of projections, and it would still not prove the run had not
//! moved on.
//!
//! What the seal binds, and what is therefore re-checked when it is spent:
//! run ID, owner session, run version, authority (control) epoch, observation
//! ID and sequence, the accepted action's fingerprint, the grant's action
//! classes, the backend's advertised capabilities, the observation's
//! per-element advertised actions, element enablement and sensitivity, the
//! normalized proposal fingerprint used for duplicate rejection, and — for a
//! completion — the host-issued [`CompletionProof`] that must still verify
//! the exact current frame.

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::computer_use::{
    action_fingerprint, ActionClass, CompletionProof, ComputerAction, ComputerCapabilities,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun,
    ComputerRunState, ComputerUseLimits, SemanticAction,
};

use super::ComputerAgentProposal;

/// Wire version for the sealed capability. A seal minted by a build with a
/// different notion of what the seal binds is refused rather than trusted.
pub const PROPOSAL_SEAL_VERSION: u32 = 1;

/// How long an accepted proposal may sit before it is spent. This is a
/// backstop only: the full identity re-check at application time is what makes
/// a stale seal unusable. The bound keeps a forgotten capability from lingering
/// in a queue as an apparently live authority.
const MAX_SEAL_AGE_SECS: i64 = 120;

const MAX_SUMMARY_BYTES: usize = 512;
const MAX_RAW_PROPOSAL_BYTES: usize = 64 * 1024;

/// Untrusted model output, exactly as the provider returned it.
///
/// This type is intentionally public and deserializable: it is *data*, not
/// authority. Nothing can be staged or completed from it without passing it
/// through [`accept_model_proposal`] against a live context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawModelProposal {
    arguments: String,
}

impl RawModelProposal {
    pub fn new(arguments: impl Into<String>) -> Self {
        Self {
            arguments: arguments.into(),
        }
    }

    pub fn arguments(&self) -> &str {
        &self.arguments
    }
}

/// The complete live context one proposal is normalized against.
///
/// Built from a run the caller has already proven it owns. Every field is read
/// from the live record, never from the model, and every field is re-checked
/// when the resulting capability is spent.
#[derive(Debug, Clone)]
pub struct ModelProposalContext {
    run_id: String,
    owner_session_id: Uuid,
    run_version: u64,
    control_epoch: u64,
    observation: ComputerObservation,
    grant_classes: BTreeSet<ActionClass>,
    capabilities: ComputerCapabilities,
    limits: ComputerUseLimits,
    completion_evidence: Option<CompletionProof>,
}

impl ModelProposalContext {
    /// Derive the context from a live run, or refuse.
    ///
    /// A run that is not `Ready`, has no grant, or has no current observation
    /// cannot produce a context at all, so those states can never reach the
    /// normalizer with a partially populated view.
    pub fn from_run(
        run: &ComputerRun,
        owner_session_id: Uuid,
        capabilities: ComputerCapabilities,
    ) -> ComputerResult<Self> {
        if run.owner_session_id != owner_session_id {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer run is not available to this session",
            ));
        }
        if run.state != ComputerRunState::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer run is not ready for a model proposal",
            ));
        }
        let grant = run.grant.as_ref().ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::Unauthorized, "computer run has no grant")
        })?;
        if grant.revoked_at.is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use grant was revoked",
            ));
        }
        let observation = run.current_observation.clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run has no current observation",
            )
        })?;
        let completion_evidence = run
            .last_receipt
            .as_ref()
            .and_then(|receipt| CompletionProof::capture(receipt, &observation, run.control_epoch));
        Ok(Self {
            run_id: run.run_id.clone(),
            owner_session_id,
            run_version: run.version,
            control_epoch: run.control_epoch,
            observation,
            grant_classes: grant.action_classes.clone(),
            capabilities,
            limits: run.limits,
            completion_evidence,
        })
    }

    pub fn observation(&self) -> &ComputerObservation {
        &self.observation
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn run_version(&self) -> u64 {
        self.run_version
    }
}

/// What a sealed proposal authorizes. Neither variant is constructible outside
/// this module.
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptedIntent {
    /// Stage exactly this action for explicit operator approval. Staging is
    /// never dispatch: the kernel revalidates at `act` time and the operator
    /// still has to approve.
    Action {
        action: ComputerAction,
        action_fingerprint: String,
    },
    /// Terminate the run as complete, on exactly this host-issued evidence.
    Complete { evidence: CompletionProof },
}

/// An unforgeable, single-use authority to apply one normalized model proposal.
///
/// Deliberately not `Clone`, not `Deserialize`, and with no public
/// constructor. Application seams take it by value, so a capability cannot be
/// spent twice even inside this process.
#[derive(Debug)]
pub struct AcceptedModelProposal {
    seal_version: u32,
    nonce: String,
    issued_at: DateTime<Utc>,
    run_id: String,
    owner_session_id: Uuid,
    run_version: u64,
    control_epoch: u64,
    observation_id: String,
    observation_sequence: u64,
    proposal_fingerprint: String,
    summary: String,
    intent: AcceptedIntent,
}

impl AcceptedModelProposal {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn owner_session_id(&self) -> Uuid {
        self.owner_session_id
    }

    pub fn run_version(&self) -> u64 {
        self.run_version
    }

    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }

    pub fn observation_sequence(&self) -> u64 {
        self.observation_sequence
    }

    pub fn control_epoch(&self) -> u64 {
        self.control_epoch
    }

    /// Stable, secret-free, content-free identity for duplicate rejection.
    pub fn proposal_fingerprint(&self) -> &str {
        &self.proposal_fingerprint
    }

    /// One-use identity. Recording spent nonces defends the capability against
    /// replay even if a future refactor makes it cloneable.
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn intent(&self) -> &AcceptedIntent {
        &self.intent
    }

    /// Authority-free projection for the cockpit and telemetry.
    pub fn view(&self) -> ComputerAgentProposal {
        match &self.intent {
            AcceptedIntent::Action { action, .. } => ComputerAgentProposal::Action {
                observation_id: self.observation_id.clone(),
                action: action.clone(),
                summary: self.summary.clone(),
            },
            AcceptedIntent::Complete { .. } => ComputerAgentProposal::Complete {
                observation_id: self.observation_id.clone(),
                summary: self.summary.clone(),
            },
        }
    }

    /// Re-check every bound identity against the live run, then yield the
    /// intent. Callers must hold whatever lock makes this atomic with the
    /// mutation that follows.
    ///
    /// This is the second half of the one validation path: normalization proves
    /// the proposal was well formed against a context, and this proves the run
    /// still *is* that context.
    pub fn authorize_against(
        &self,
        run: &ComputerRun,
        owner_session_id: Uuid,
    ) -> ComputerResult<()> {
        if self.seal_version != PROPOSAL_SEAL_VERSION {
            return Err(unsealed("accepted proposal seal version is not current"));
        }
        let age = Utc::now().signed_duration_since(self.issued_at);
        if age < Duration::zero() || age > Duration::seconds(MAX_SEAL_AGE_SECS) {
            return Err(unsealed("accepted proposal expired before it was applied"));
        }
        if self.owner_session_id != owner_session_id
            || run.owner_session_id != owner_session_id
            || self.run_id != run.run_id
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "accepted proposal does not belong to this run and session",
            ));
        }
        if self.run_version != run.version || self.control_epoch != run.control_epoch {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "the computer run changed after the proposal was accepted",
            ));
        }
        if run.state != ComputerRunState::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "computer run is not ready to apply a proposal",
            ));
        }
        let observation = run.current_observation.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run has no current observation",
            )
        })?;
        if observation.observation_id != self.observation_id
            || observation.sequence != self.observation_sequence
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "the observation changed after the proposal was accepted",
            ));
        }
        let grant = run.grant.as_ref().ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::Unauthorized, "computer run has no grant")
        })?;
        if grant.revoked_at.is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use grant was revoked",
            ));
        }
        match &self.intent {
            AcceptedIntent::Action {
                action,
                action_fingerprint: fingerprint,
            } => {
                if &action_fingerprint(&run.run_id, action) != fingerprint {
                    return Err(unsealed("accepted action does not match its fingerprint"));
                }
                if !grant.action_classes.contains(&action.class()) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "action class is outside the grant",
                    ));
                }
                // Re-run the model-only element checks against the live frame,
                // not against the frame captured at normalization time.
                check_action_against_observation(action, observation)
            }
            AcceptedIntent::Complete { evidence } => {
                let receipt = run.last_receipt.as_ref().ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::UnverifiedCompletion,
                        "computer run has no action receipt for this frame",
                    )
                })?;
                if receipt.receipt_id != evidence.receipt_id
                    || receipt.action_fingerprint != evidence.action_fingerprint
                    || !evidence.frame.matches(observation)
                    || evidence.control_epoch != run.control_epoch
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::UnverifiedCompletion,
                        "completion evidence no longer matches the live run",
                    ));
                }
                Ok(())
            }
        }
    }

    /// Spend the capability, yielding the intent it authorizes.
    pub fn into_intent(self) -> AcceptedIntent {
        self.intent
    }
}

/// The one strict normalizer. Untrusted bytes in, sealed capability or typed
/// refusal out.
pub fn accept_model_proposal(
    context: &ModelProposalContext,
    raw: &RawModelProposal,
) -> ComputerResult<AcceptedModelProposal> {
    let arguments = parse_strict(raw.arguments())?;
    if arguments.observation_id != context.observation.observation_id {
        return Err(ComputerError::new(
            ComputerErrorCode::StaleObservation,
            "model proposal is bound to a stale observation",
        ));
    }
    let summary = validate_summary(&arguments.summary)?;

    let intent = if arguments.action_type == "complete" {
        if arguments.element_id.is_some()
            || arguments.text.is_some()
            || arguments.delta_x.is_some()
            || arguments.delta_y.is_some()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "completion proposal carries action arguments",
            ));
        }
        // Fail closed: a completion is only ever as good as the host-issued
        // receipt the run currently holds for the current frame (#456). A
        // model asserting success buys nothing.
        let evidence = context.completion_evidence.clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::UnverifiedCompletion,
                "no host-issued action receipt verifies the current observation",
            )
        })?;
        AcceptedIntent::Complete { evidence }
    } else {
        let action = normalize_action(&arguments)?;
        action.validate(&context.limits)?;
        if !model_proposable(&action) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "action kind is operator-only and cannot be model-proposed",
            ));
        }
        if !context.grant_classes.contains(&action.class()) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "action class is outside the run's grant",
            ));
        }
        if !backend_advertises(&context.capabilities, action.class()) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "the backend does not advertise this action class",
            ));
        }
        check_action_against_observation(&action, &context.observation)?;
        let fingerprint = action_fingerprint(&context.run_id, &action);
        AcceptedIntent::Action {
            action,
            action_fingerprint: fingerprint,
        }
    };

    Ok(AcceptedModelProposal {
        seal_version: PROPOSAL_SEAL_VERSION,
        nonce: format!("seal-{}", Uuid::new_v4()),
        issued_at: Utc::now(),
        run_id: context.run_id.clone(),
        owner_session_id: context.owner_session_id,
        run_version: context.run_version,
        control_epoch: context.control_epoch,
        observation_id: context.observation.observation_id.clone(),
        observation_sequence: context.observation.sequence,
        proposal_fingerprint: proposal_fingerprint(context, &intent),
        summary,
        intent,
    })
}

/// Run-, frame-, and intent-scoped identity used to reject a repeated proposal.
/// Hashed, so no observed content reaches a duplicate registry or a projection.
fn proposal_fingerprint(context: &ModelProposalContext, intent: &AcceptedIntent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.computer.proposal.v1");
    hasher.update([0]);
    hasher.update(context.run_id.as_bytes());
    hasher.update([0]);
    hasher.update(context.observation.observation_id.as_bytes());
    hasher.update([0]);
    hasher.update(context.observation.sequence.to_be_bytes());
    hasher.update([0]);
    match intent {
        AcceptedIntent::Action {
            action_fingerprint, ..
        } => {
            hasher.update(b"action");
            hasher.update([0]);
            hasher.update(action_fingerprint.as_bytes());
        }
        AcceptedIntent::Complete { evidence } => {
            hasher.update(b"complete");
            hasher.update([0]);
            hasher.update(evidence.receipt_id.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Action kinds a model may propose at all. Pointer, key-chord, and wait
/// remain operator-only regardless of grant or backend capability, so a
/// coordinate- or keystroke-level escape cannot be reached from model output.
fn model_proposable(action: &ComputerAction) -> bool {
    matches!(
        action,
        ComputerAction::ActivateTarget
            | ComputerAction::Invoke { .. }
            | ComputerAction::SetValue { .. }
            | ComputerAction::Select { .. }
            | ComputerAction::Scroll { .. }
    )
}

fn backend_advertises(capabilities: &ComputerCapabilities, class: ActionClass) -> bool {
    match class {
        ActionClass::Semantic => capabilities.semantic_actions,
        ActionClass::TextEntry => capabilities.text_entry,
        ActionClass::KeyChord => capabilities.key_chords,
        ActionClass::PointerFallback => capabilities.pointer_fallback,
    }
}

fn check_action_against_observation(
    action: &ComputerAction,
    observation: &ComputerObservation,
) -> ComputerResult<()> {
    if observation.sensitivity.is_hard_denied() {
        return Err(ComputerError::new(
            ComputerErrorCode::SensitiveSurface,
            "the observed surface is hard denied",
        ));
    }
    let Some(element_id) = action.referenced_element() else {
        return Ok(());
    };
    let element = observation.element(element_id).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::StaleObservation,
            "model selected an element that is not in the observation",
        )
    })?;
    if element.sensitivity.is_hard_denied() {
        return Err(ComputerError::new(
            ComputerErrorCode::SensitiveSurface,
            "model selected a sensitive element",
        ));
    }
    if !element.enabled {
        return Err(ComputerError::new(
            ComputerErrorCode::ForbiddenAction,
            "model selected a disabled element",
        ));
    }
    let required = match action {
        ComputerAction::Invoke { .. } => Some(SemanticAction::Invoke),
        ComputerAction::SetValue { .. } => Some(SemanticAction::SetValue),
        ComputerAction::Select { .. } => Some(SemanticAction::Select),
        ComputerAction::Scroll { .. } => Some(SemanticAction::Scroll),
        _ => None,
    };
    if let Some(required) = required {
        if !element.actions.contains(&required) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "model selected an action the observation does not advertise",
            ));
        }
    }
    Ok(())
}

fn normalize_action(arguments: &StrictProposalArguments) -> ComputerResult<ComputerAction> {
    let only_element = arguments.element_id.is_some()
        && arguments.text.is_none()
        && arguments.delta_x.is_none()
        && arguments.delta_y.is_none();
    let bare = arguments.element_id.is_none()
        && arguments.text.is_none()
        && arguments.delta_x.is_none()
        && arguments.delta_y.is_none();
    let action = match arguments.action_type.as_str() {
        "activate_target" if bare => ComputerAction::ActivateTarget,
        "invoke" if only_element => ComputerAction::Invoke {
            element_id: arguments.element_id.clone().expect("checked element"),
        },
        "select" if only_element => ComputerAction::Select {
            element_id: arguments.element_id.clone().expect("checked element"),
        },
        "set_value"
            if arguments.element_id.is_some()
                && arguments.text.is_some()
                && arguments.delta_x.is_none()
                && arguments.delta_y.is_none() =>
        {
            ComputerAction::SetValue {
                element_id: arguments.element_id.clone().expect("checked element"),
                text: arguments.text.clone().expect("checked text"),
            }
        }
        "scroll"
            if arguments.element_id.is_some()
                && arguments.text.is_none()
                && arguments.delta_x.is_some()
                && arguments.delta_y.is_some() =>
        {
            ComputerAction::Scroll {
                element_id: arguments.element_id.clone(),
                delta_x: arguments.delta_x.expect("checked delta"),
                delta_y: arguments.delta_y.expect("checked delta"),
            }
        }
        _ => {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "model proposed an unsupported or incoherent action",
            ))
        }
    };
    Ok(action)
}

fn validate_summary(summary: &str) -> ComputerResult<String> {
    if summary.trim().is_empty()
        || summary.len() > MAX_SUMMARY_BYTES
        || summary
            .chars()
            .any(|c| c == '\0' || c.is_control() && c != '\n')
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "model proposal summary is empty, oversized, or contains control characters",
        ));
    }
    Ok(summary.trim().to_string())
}

fn unsealed(message: &str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::UnsealedProposal, message)
}

/// Model tool arguments, parsed under a deliberately unforgiving reader.
///
/// `serde_json` silently keeps the *last* value for a repeated key, which lets
/// one payload mean different things to a validator and to an applier. The
/// manual `Deserialize` below rejects any repeated key outright, rejects
/// unknown keys, and the parse helper rejects trailing content, so there is
/// exactly one reading of any accepted payload.
#[derive(Debug)]
struct StrictProposalArguments {
    observation_id: String,
    action_type: String,
    element_id: Option<String>,
    text: Option<String>,
    delta_x: Option<i32>,
    delta_y: Option<i32>,
    summary: String,
}

fn parse_strict(raw: &str) -> ComputerResult<StrictProposalArguments> {
    if raw.len() > MAX_RAW_PROPOSAL_BYTES {
        return Err(ComputerError::new(
            ComputerErrorCode::LimitReached,
            "model proposal payload exceeds the accepted size bound",
        ));
    }
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let parsed = StrictProposalArguments::deserialize(&mut deserializer).map_err(|error| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("model returned malformed proposal arguments: {error}"),
        )
    })?;
    deserializer.end().map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "model proposal payload has trailing content",
        )
    })?;
    Ok(parsed)
}

impl<'de> Deserialize<'de> for StrictProposalArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictVisitor;

        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictProposalArguments;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a computer proposal object with unique, known keys")
            }

            fn visit_map<M>(self, mut map: M) -> Result<StrictProposalArguments, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut seen: BTreeSet<String> = BTreeSet::new();
                let mut observation_id = None;
                let mut action_type = None;
                let mut element_id = None;
                let mut text = None;
                let mut delta_x = None;
                let mut delta_y = None;
                let mut summary = None;
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate key `{key}`")));
                    }
                    match key.as_str() {
                        "observation_id" => observation_id = Some(map.next_value::<String>()?),
                        "action_type" => action_type = Some(map.next_value::<String>()?),
                        "element_id" => element_id = Some(map.next_value::<String>()?),
                        "text" => text = Some(map.next_value::<String>()?),
                        "delta_x" => delta_x = Some(map.next_value::<i32>()?),
                        "delta_y" => delta_y = Some(map.next_value::<i32>()?),
                        "summary" => summary = Some(map.next_value::<String>()?),
                        other => {
                            return Err(de::Error::custom(format!("unknown key `{other}`")));
                        }
                    }
                }
                Ok(StrictProposalArguments {
                    observation_id: observation_id
                        .ok_or_else(|| de::Error::missing_field("observation_id"))?,
                    action_type: action_type
                        .ok_or_else(|| de::Error::missing_field("action_type"))?,
                    element_id,
                    text,
                    delta_x,
                    delta_y,
                    summary: summary.ok_or_else(|| de::Error::missing_field("summary"))?,
                })
            }
        }

        deserializer.deserialize_map(StrictVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::{
        ActionGrant, ComputerTarget, GrantIssuer, ObservationGeometry, SemanticElement, Sensitivity,
    };

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.example.demo".into(),
            window_id: "window-1".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn observation() -> ComputerObservation {
        ComputerObservation {
            observation_id: "observation-1".into(),
            sequence: 1,
            target: target(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "name".into(),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    fn ready_run(owner: Uuid) -> ComputerRun {
        let mut run =
            ComputerRun::new(owner, None, target(), ComputerUseLimits::default()).expect("run");
        let now = Utc::now();
        run.state = ComputerRunState::Ready;
        run.grant = Some(ActionGrant {
            grant_id: Uuid::new_v4().to_string(),
            run_id: run.run_id.clone(),
            target: target(),
            action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            issued_by: GrantIssuer::LocalUser,
            issued_at: now,
            expires_at: now + Duration::minutes(5),
            uses_remaining: Some(4),
            revoked_at: None,
        });
        run.current_observation = Some(observation());
        run
    }

    fn capabilities() -> ComputerCapabilities {
        ComputerCapabilities {
            backend_id: "seal_unit_fixture".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: true,
            pointer_fallback: true,
        }
    }

    fn set_value_raw() -> RawModelProposal {
        RawModelProposal::new(
            serde_json::json!({
                "observation_id": "observation-1",
                "action_type": "set_value",
                "element_id": "name",
                "text": "Ada Lovelace",
                "summary": "Enter the visible name"
            })
            .to_string(),
        )
    }

    fn accepted(owner: Uuid, run: &ComputerRun) -> AcceptedModelProposal {
        let context = ModelProposalContext::from_run(run, owner, capabilities()).expect("context");
        accept_model_proposal(&context, &set_value_raw()).expect("seal")
    }

    #[test]
    fn a_fresh_seal_authorizes_its_own_run() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let sealed = accepted(owner, &run);
        assert_eq!(sealed.run_id(), run.run_id);
        assert_eq!(sealed.observation_sequence(), 1);
        sealed
            .authorize_against(&run, owner)
            .expect("a fresh seal is live");
    }

    /// The seal is bounded in time. A capability that sat in a queue is refused
    /// on its own terms, before any run comparison.
    #[test]
    fn an_aged_seal_is_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        sealed.issued_at = Utc::now() - Duration::seconds(MAX_SEAL_AGE_SECS + 1);
        let error = sealed
            .authorize_against(&run, owner)
            .expect_err("an expired seal cannot apply");
        assert_eq!(error.code, ComputerErrorCode::UnsealedProposal);

        // A seal stamped in the future is equally refused.
        sealed.issued_at = Utc::now() + Duration::seconds(60);
        assert_eq!(
            sealed
                .authorize_against(&run, owner)
                .expect_err("a future-dated seal cannot apply")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// The seal is versioned. A capability minted under a different notion of
    /// what the seal binds is refused rather than reinterpreted.
    #[test]
    fn a_seal_from_another_version_is_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        sealed.seal_version = PROPOSAL_SEAL_VERSION + 1;
        assert_eq!(
            sealed
                .authorize_against(&run, owner)
                .expect_err("version mismatch")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// The action inside a seal cannot be swapped for another: the fingerprint
    /// is re-derived from the action itself at application time.
    #[test]
    fn a_tampered_action_no_longer_matches_its_fingerprint() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        sealed.intent = AcceptedIntent::Action {
            action: ComputerAction::SetValue {
                element_id: "name".into(),
                text: "Grace Hopper".into(),
            },
            action_fingerprint: match &sealed.intent {
                AcceptedIntent::Action {
                    action_fingerprint, ..
                } => action_fingerprint.clone(),
                AcceptedIntent::Complete { .. } => unreachable!(),
            },
        };
        assert_eq!(
            sealed
                .authorize_against(&run, owner)
                .expect_err("fingerprint mismatch")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// The seal carries no secret and no observed content: everything durable
    /// about it is an identity or a hash.
    #[test]
    fn seal_identities_are_content_free() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let sealed = accepted(owner, &run);
        assert_eq!(sealed.proposal_fingerprint().len(), 64);
        assert!(sealed
            .proposal_fingerprint()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert!(!sealed.proposal_fingerprint().contains("Ada"));
        assert!(!sealed.nonce().contains("Ada"));
    }

    /// A grant that does not carry the action's class refuses the proposal even
    /// though the frame and element are perfectly valid.
    #[test]
    fn a_grant_without_the_action_class_refuses_the_proposal() {
        let owner = Uuid::new_v4();
        let mut run = ready_run(owner);
        if let Some(grant) = run.grant.as_mut() {
            grant.action_classes = BTreeSet::from([ActionClass::Semantic]);
        }
        let context = ModelProposalContext::from_run(&run, owner, capabilities()).expect("context");
        assert_eq!(
            accept_model_proposal(&context, &set_value_raw())
                .expect_err("text entry is outside the grant")
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    /// A backend that does not advertise the class refuses it too, so a grant
    /// alone is never sufficient.
    #[test]
    fn a_backend_without_the_capability_refuses_the_proposal() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let context = ModelProposalContext::from_run(
            &run,
            owner,
            ComputerCapabilities {
                text_entry: false,
                ..capabilities()
            },
        )
        .expect("context");
        assert_eq!(
            accept_model_proposal(&context, &set_value_raw())
                .expect_err("backend does not advertise text entry")
                .code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    /// Control characters and empty summaries are model-authored text reaching
    /// an operator's screen. They are bounded and scrubbed at the boundary.
    #[test]
    fn hostile_summaries_are_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let context = ModelProposalContext::from_run(&run, owner, capabilities()).expect("context");
        for summary in ["", "   ", "ok\u{0}injected", "ok\u{1b}[2Jcleared"] {
            let raw = RawModelProposal::new(
                serde_json::json!({
                    "observation_id": "observation-1",
                    "action_type": "set_value",
                    "element_id": "name",
                    "text": "Ada",
                    "summary": summary
                })
                .to_string(),
            );
            assert_eq!(
                accept_model_proposal(&context, &raw)
                    .expect_err("hostile summary")
                    .code,
                ComputerErrorCode::InvalidRequest,
                "summary {summary:?} was accepted"
            );
        }
    }
}
