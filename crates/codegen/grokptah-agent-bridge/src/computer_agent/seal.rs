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
    action_fingerprint, objective_digest, ActionClass, CompletionProof, ComputerAction,
    ComputerCapabilities, ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult,
    ComputerRun, ComputerRunState, ComputerTaskSpec, ComputerUseLimits, ComputerUseService,
    SemanticAction,
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

/// The provider route and authority generations one proposal was minted under.
///
/// Every field is folded into the seal's authority digest, so a proposal minted
/// under one route, capability generation, profile, principal, or lease cannot
/// be applied under another: the digest stops matching and application stops.
///
/// Four slots are `None` today because the authorities that would fill them do
/// not exist on this branch. They are typed and digested now rather than bolted
/// on later, so landing those issues tightens this fence in place instead of
/// introducing a second one:
///
/// - `capability_generation` — provider capability generation, #458.
/// - `adaptive_profile` — adaptive profile selection, #435.
/// - `principal_generation` — host-issued principal/auth generation, #477.
/// - `lease_id` — lease/agent binding for the run.
///
/// `route_fingerprint` and `model` are caller-attested until #458 lands: they
/// describe the route the caller used, and the seal proves only that it did not
/// change between minting and application. That is the exact residual #458
/// closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    route_fingerprint: String,
    model: String,
    capability_generation: Option<String>,
    adaptive_profile: Option<String>,
    principal_generation: Option<String>,
    lease_id: Option<String>,
}

impl RouteBinding {
    /// Bind the provider route a proposal is being requested over.
    pub fn new(route_fingerprint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            route_fingerprint: route_fingerprint.into(),
            model: model.into(),
            capability_generation: None,
            adaptive_profile: None,
            principal_generation: None,
            lease_id: None,
        }
    }

    /// Provider capability generation (#458). Absent until that lands.
    pub fn with_capability_generation(mut self, generation: impl Into<String>) -> Self {
        self.capability_generation = Some(generation.into());
        self
    }

    /// Adaptive profile selection (#435). Absent until that lands.
    pub fn with_adaptive_profile(mut self, profile: impl Into<String>) -> Self {
        self.adaptive_profile = Some(profile.into());
        self
    }

    /// Host-issued principal/auth generation (#477). Absent until that lands.
    pub fn with_principal_generation(mut self, generation: impl Into<String>) -> Self {
        self.principal_generation = Some(generation.into());
        self
    }

    /// Lease/agent binding for the run. Absent until that lands.
    pub fn with_lease(mut self, lease_id: impl Into<String>) -> Self {
        self.lease_id = Some(lease_id.into());
        self
    }

    fn digest_into(&self, hasher: &mut Sha256) {
        // A missing generation is digested as a distinct marker, never as an
        // empty string, so "unbound" cannot collide with a real value of "".
        const UNBOUND: &str = "\u{1}unbound";
        for part in [
            self.route_fingerprint.as_str(),
            self.model.as_str(),
            self.capability_generation.as_deref().unwrap_or(UNBOUND),
            self.adaptive_profile.as_deref().unwrap_or(UNBOUND),
            self.principal_generation.as_deref().unwrap_or(UNBOUND),
            self.lease_id.as_deref().unwrap_or(UNBOUND),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
    }
}

/// Digest over every authority a proposal depends on, recomputed from the live
/// run whenever a seal is spent.
///
/// One digest rather than a list of comparisons is deliberate: adding an
/// authority means adding it here, and every existing seal is invalidated by
/// construction. There is no way to add a binding and forget to check it.
fn authority_digest(
    run: &ComputerRun,
    owner_session_id: Uuid,
    observation: &ComputerObservation,
    capabilities: &ComputerCapabilities,
    route: &RouteBinding,
    objective_digest: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.computer.authority.v1");
    hasher.update([0]);
    hasher.update(run.run_id.as_bytes());
    hasher.update([0]);
    hasher.update(owner_session_id.as_bytes());
    hasher.update(run.version.to_be_bytes());
    hasher.update(run.control_epoch.to_be_bytes());
    hasher.update(format!("{:?}", run.state).as_bytes());
    hasher.update(format!("{:?}", run.control_disposition).as_bytes());
    hasher.update([0]);

    // Exact grant identity and generation.
    match run.grant.as_ref() {
        Some(grant) => {
            hasher.update(b"grant");
            hasher.update(grant.grant_id.as_bytes());
            hasher.update([0]);
            hasher.update(grant.issued_at.timestamp_millis().to_be_bytes());
            hasher.update(grant.expires_at.timestamp_millis().to_be_bytes());
            hasher.update(grant.uses_remaining.unwrap_or(u32::MAX).to_be_bytes());
            hasher.update(u8::from(grant.revoked_at.is_some()).to_be_bytes());
            for class in &grant.action_classes {
                hasher.update(format!("{class:?}").as_bytes());
                hasher.update([0]);
            }
        }
        None => hasher.update(b"grant\x01none"),
    }
    hasher.update([0]);

    // Target identity and generation.
    hasher.update(run.target.app_id.as_bytes());
    hasher.update([0]);
    hasher.update(run.target.window_id.as_bytes());
    hasher.update([0]);
    hasher.update(run.target.generation.to_be_bytes());
    hasher.update(format!("{:?}", run.target.sensitivity).as_bytes());
    hasher.update([0]);

    // Exact frame.
    hasher.update(observation.observation_id.as_bytes());
    hasher.update([0]);
    hasher.update(observation.sequence.to_be_bytes());
    hasher.update(observation.captured_at.timestamp_millis().to_be_bytes());

    // Effective policy: the limits that decide what may be proposed at all.
    hasher.update(b"policy");
    match serde_json::to_vec(&run.limits) {
        Ok(bytes) => hasher.update(&bytes),
        Err(_) => hasher.update(b"\x01unserializable"),
    }
    hasher.update([0]);

    // Operator objective and its success predicate.
    hasher.update(b"objective");
    hasher.update(objective_digest.as_bytes());
    hasher.update([0]);
    match run.task_spec.as_ref() {
        Some(spec) => hasher.update(spec.digest().as_bytes()),
        None => hasher.update(b"\x01none"),
    }
    hasher.update([0]);

    // Backend capability surface available today (#458 supplies the provider
    // generation through `route`).
    hasher.update(b"capabilities");
    match serde_json::to_vec(capabilities) {
        Ok(bytes) => hasher.update(&bytes),
        Err(_) => hasher.update(b"\x01unserializable"),
    }
    hasher.update([0]);

    route.digest_into(&mut hasher);
    format!("{:x}", hasher.finalize())
}

/// Which model turn a proposal belongs to.
///
/// Identifiers only. Everything these name is looked up from host-owned state
/// by [`accept_model_output`]; nothing here is trusted as content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTurn<'a> {
    pub run_id: &'a str,
    pub expected_version: u64,
    pub observation_id: &'a str,
    /// The exact objective text the model was given. It must be the one the
    /// operator authored, or the turn is refused.
    pub objective: &'a str,
}

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
/// Every field is read from host-owned state — the durable run record and the
/// backend's own capability report — never from a caller-supplied value. That
/// is why the constructor is private and the only way in is
/// [`accept_model_output`], which takes identifiers and looks the state up
/// itself. A caller that could hand in a `ComputerRun` could hand in a
/// fabricated one, and the seal would be exactly as trustworthy as the lie.
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
    task_spec: Option<ComputerTaskSpec>,
    objective_digest: String,
    route: RouteBinding,
    authority_digest: String,
    completion_proof: Option<CompletionProof>,
}

impl ModelProposalContext {
    /// Derive the context from host-owned state, or refuse.
    ///
    /// Private on purpose: see the type docs. A run that is not `Ready`, has no
    /// grant, has no current observation, has no operator-authored objective, or
    /// whose objective does not govern the text the model was given cannot
    /// produce a context at all, so none of those states reach the normalizer
    /// with a partially populated view.
    fn from_host_state(
        run: &ComputerRun,
        owner_session_id: Uuid,
        capabilities: ComputerCapabilities,
        objective: &str,
        route: RouteBinding,
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
        // The frame the model reasons over must still be fresh by the run's own
        // policy, not merely current: a stale frame is a stale premise.
        let age = Utc::now().signed_duration_since(observation.captured_at);
        if age < Duration::zero()
            || age > Duration::milliseconds(run.limits.max_observation_age_millis as i64)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "current observation is too old or from the future to reason over",
            ));
        }
        // A run with no authored objective has no definition of success, so a
        // model turn against it can never be completed. Refuse up front rather
        // than discovering it at the completion arm.
        let spec = run.task_spec.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::UnverifiedCompletion,
                "computer run has no operator-authored objective",
            )
        })?;
        if !spec.governs(objective) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "the objective given to the model is not the one the operator authored",
            ));
        }
        let digest = objective_digest(objective);
        let completion_proof = run
            .last_receipt
            .as_ref()
            .and_then(|receipt| CompletionProof::capture(receipt, &observation, run.control_epoch));
        let authority_digest = authority_digest(
            run,
            owner_session_id,
            &observation,
            &capabilities,
            &route,
            &digest,
        );
        Ok(Self {
            run_id: run.run_id.clone(),
            owner_session_id,
            run_version: run.version,
            control_epoch: run.control_epoch,
            observation,
            grant_classes: grant.action_classes.clone(),
            capabilities,
            limits: run.limits,
            task_spec: run.task_spec.clone(),
            objective_digest: digest,
            route,
            authority_digest,
            completion_proof,
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

    /// Does the operator's objective already hold on this frame?
    ///
    /// Reported so a caller can tell the model that further action is
    /// unnecessary. It is advisory only — the authority is
    /// `ComputerUseService::complete_verified`, which decides against the frame
    /// live at application time.
    pub fn objective_satisfied(&self) -> bool {
        self.task_spec
            .as_ref()
            .is_some_and(|spec| spec.evaluate(&self.observation).is_ok())
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
    /// Digest over every authority this proposal depends on, recomputed from
    /// the live run when the capability is spent.
    authority_digest: String,
    objective_digest: String,
    route: RouteBinding,
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

    /// The objective digest this proposal was minted for.
    pub fn objective_digest(&self) -> &str {
        &self.objective_digest
    }

    /// Re-check every bound identity against the live run, then yield the
    /// intent. Callers must hold whatever lock makes this atomic with the
    /// mutation that follows.
    ///
    /// `capabilities` and `route` come from the host at application time, not
    /// from the seal, so a capability minted under one backend surface or
    /// provider route cannot be spent under another.
    pub fn authorize_against(
        &self,
        run: &ComputerRun,
        owner_session_id: Uuid,
        capabilities: &ComputerCapabilities,
        route: &RouteBinding,
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
        // One comparison covering every bound authority: run identity and
        // revision, control epoch, state and disposition, exact grant identity
        // and generation, target identity and generation, exact frame and its
        // capture time, effective policy limits, the operator objective and its
        // predicate, the backend capability surface, and the provider route
        // with its capability/profile/principal/lease generations. Any change
        // to any of them stops the application here.
        if self.route != *route {
            return Err(unsealed(
                "accepted proposal was minted under a different provider route",
            ));
        }
        let live = authority_digest(
            run,
            owner_session_id,
            observation,
            capabilities,
            route,
            &self.objective_digest,
        );
        if live != self.authority_digest {
            return Err(unsealed(
                "the authority the proposal was accepted under has changed",
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

/// Turn untrusted model output into a sealed capability, against host-owned
/// state only.
///
/// This is the single public entry to the boundary. It takes identifiers and
/// looks every authority-relevant value up itself — the run from the durable
/// ledger, the capabilities from the backend — so no caller can substitute a
/// fabricated run, grant, observation, receipt, or objective. The strict
/// normalizer below is private and unreachable except through here.
///
/// ```compile_fail
/// # use grokptah_agent_bridge::ModelProposalContext;
/// // There is no public constructor: a caller cannot assemble a context from
/// // values it controls, which is what would make the seal forgeable.
/// let context = ModelProposalContext::from_host_state();
/// ```
pub fn accept_model_output(
    service: &ComputerUseService,
    owner_session_id: Uuid,
    turn: &ModelTurn<'_>,
    route: RouteBinding,
    raw: &RawModelProposal,
) -> ComputerResult<AcceptedModelProposal> {
    let ModelTurn {
        run_id,
        expected_version,
        observation_id,
        objective,
    } = *turn;
    let run = service
        .get_run(run_id)?
        .filter(|run| run.owner_session_id == owner_session_id)
        .ok_or_else(|| {
            // Unknown and cross-session runs answer identically so this cannot
            // be used as an existence oracle.
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer run is not available to this session",
            )
        })?;
    if run.version != expected_version {
        return Err(ComputerError::new(
            ComputerErrorCode::Conflict,
            "the computer run changed before the model proposal was normalized",
        ));
    }
    if run
        .current_observation
        .as_ref()
        .map(|observation| observation.observation_id.as_str())
        != Some(observation_id)
    {
        return Err(ComputerError::new(
            ComputerErrorCode::StaleObservation,
            "the observation changed before the model proposal was normalized",
        ));
    }
    let context = ModelProposalContext::from_host_state(
        &run,
        owner_session_id,
        service.capabilities(),
        objective,
        route,
    )?;
    accept_model_proposal(&context, raw)
}

/// The one strict normalizer. Untrusted bytes in, sealed capability or typed
/// refusal out. Private: reachable only through [`accept_model_output`], which
/// guarantees the context came from host-owned state.
fn accept_model_proposal(
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
        let evidence = context.completion_proof.clone().ok_or_else(|| {
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
        authority_digest: context.authority_digest.clone(),
        objective_digest: context.objective_digest.clone(),
        route: context.route.clone(),
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
    // The summary is untrusted text that reaches an operator's approval prompt
    // and the proposal view, so it goes through the same public privacy needles
    // the durable journal uses rather than a second, weaker set.
    let redacted = crate::event_bus::redact_display_text(summary.trim(), MAX_SUMMARY_BYTES);
    if redacted.trim().is_empty() {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "model proposal summary was entirely redacted",
        ));
    }
    Ok(redacted)
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
        ActionGrant, ComputerTarget, ElementLocator, GrantIssuer, ObservationGeometry,
        SemanticElement, Sensitivity, TaskPredicate,
    };

    const OBJECTIVE: &str = "Enter Ada Lovelace in the visible Name field";

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
                element_id: "ephemeral-name".into(),
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

    fn spec() -> ComputerTaskSpec {
        ComputerTaskSpec::new(
            OBJECTIVE,
            TaskPredicate::ElementValueEquals {
                locator: ElementLocator::new("text_field", Some("Name".into())),
                value: "Ada Lovelace".into(),
            },
            4,
        )
        .expect("spec")
    }

    fn ready_run(owner: Uuid) -> ComputerRun {
        let mut run =
            ComputerRun::new(owner, None, target(), ComputerUseLimits::default()).expect("run");
        let now = Utc::now();
        run.state = ComputerRunState::Ready;
        run.task_spec = Some(spec());
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

    fn route() -> RouteBinding {
        RouteBinding::new("route-a", "fixture-model")
    }

    fn set_value_raw() -> RawModelProposal {
        RawModelProposal::new(
            serde_json::json!({
                "observation_id": "observation-1",
                "action_type": "set_value",
                "element_id": "ephemeral-name",
                "text": "Ada Lovelace",
                "summary": "Enter the visible name"
            })
            .to_string(),
        )
    }

    fn context(owner: Uuid, run: &ComputerRun) -> ModelProposalContext {
        ModelProposalContext::from_host_state(run, owner, capabilities(), OBJECTIVE, route())
            .expect("context")
    }

    fn accepted(owner: Uuid, run: &ComputerRun) -> AcceptedModelProposal {
        accept_model_proposal(&context(owner, run), &set_value_raw()).expect("seal")
    }

    #[test]
    fn a_fresh_seal_authorizes_its_own_run() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let sealed = accepted(owner, &run);
        assert_eq!(sealed.run_id(), run.run_id);
        assert_eq!(sealed.observation_sequence(), 1);
        sealed
            .authorize_against(&run, owner, &capabilities(), &route())
            .expect("a fresh seal is live");
    }

    /// The seal is bounded in time, on its own terms, before any run
    /// comparison.
    #[test]
    fn an_aged_seal_is_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        sealed.issued_at = Utc::now() - Duration::seconds(MAX_SEAL_AGE_SECS + 1);
        assert_eq!(
            sealed
                .authorize_against(&run, owner, &capabilities(), &route())
                .expect_err("expired")
                .code,
            ComputerErrorCode::UnsealedProposal
        );

        sealed.issued_at = Utc::now() + Duration::seconds(60);
        assert_eq!(
            sealed
                .authorize_against(&run, owner, &capabilities(), &route())
                .expect_err("future-dated")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// The seal is versioned: a capability minted under a different notion of
    /// what the seal binds is refused, never reinterpreted.
    #[test]
    fn a_seal_from_another_version_is_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        sealed.seal_version = PROPOSAL_SEAL_VERSION + 1;
        assert_eq!(
            sealed
                .authorize_against(&run, owner, &capabilities(), &route())
                .expect_err("version mismatch")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// The action inside a seal cannot be swapped: the fingerprint is
    /// re-derived from the action itself at application time.
    #[test]
    fn a_tampered_action_no_longer_matches_its_fingerprint() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let mut sealed = accepted(owner, &run);
        let fingerprint = match &sealed.intent {
            AcceptedIntent::Action {
                action_fingerprint, ..
            } => action_fingerprint.clone(),
            AcceptedIntent::Complete { .. } => unreachable!(),
        };
        sealed.intent = AcceptedIntent::Action {
            action: ComputerAction::SetValue {
                element_id: "ephemeral-name".into(),
                text: "Grace Hopper".into(),
            },
            action_fingerprint: fingerprint,
        };
        assert_eq!(
            sealed
                .authorize_against(&run, owner, &capabilities(), &route())
                .expect_err("fingerprint mismatch")
                .code,
            ComputerErrorCode::UnsealedProposal
        );
    }

    /// Every authority folded into the digest actually moves it. A binding that
    /// does not change the digest is a binding that is not checked.
    #[test]
    fn the_authority_digest_covers_every_binding() {
        let owner = Uuid::new_v4();
        let base = ready_run(owner);
        let observation = base.current_observation.clone().expect("observation");
        let digest = |run: &ComputerRun, caps: &ComputerCapabilities, route: &RouteBinding| {
            authority_digest(
                run,
                owner,
                run.current_observation.as_ref().unwrap_or(&observation),
                caps,
                route,
                &objective_digest(OBJECTIVE),
            )
        };
        let reference = digest(&base, &capabilities(), &route());

        let mut version = base.clone();
        version.version += 1;
        let mut epoch = base.clone();
        epoch.control_epoch += 1;
        let mut grant_id = base.clone();
        if let Some(grant) = grant_id.grant.as_mut() {
            grant.grant_id = Uuid::new_v4().to_string();
        }
        let mut grant_uses = base.clone();
        if let Some(grant) = grant_uses.grant.as_mut() {
            grant.uses_remaining = Some(1);
        }
        let mut revoked = base.clone();
        if let Some(grant) = revoked.grant.as_mut() {
            grant.revoked_at = Some(Utc::now());
        }
        let mut generation = base.clone();
        generation.target.generation += 1;
        let mut limits = base.clone();
        limits.limits.max_actions += 1;
        let mut no_spec = base.clone();
        no_spec.task_spec = None;
        let mut other_spec = base.clone();
        other_spec.task_spec = Some(
            ComputerTaskSpec::new(
                OBJECTIVE,
                TaskPredicate::ElementEnabled {
                    locator: ElementLocator::new("text_field", Some("Name".into())),
                },
                4,
            )
            .expect("spec"),
        );
        let mut frame = base.clone();
        if let Some(current) = frame.current_observation.as_mut() {
            current.sequence += 1;
        }

        for (label, run) in [
            ("run version", version),
            ("control epoch", epoch),
            ("grant id", grant_id),
            ("grant uses", grant_uses),
            ("grant revocation", revoked),
            ("target generation", generation),
            ("policy limits", limits),
            ("absent task spec", no_spec),
            ("different predicate", other_spec),
            ("frame sequence", frame),
        ] {
            assert_ne!(
                digest(&run, &capabilities(), &route()),
                reference,
                "{label} must move the authority digest"
            );
        }

        assert_ne!(
            digest(
                &base,
                &ComputerCapabilities {
                    text_entry: false,
                    ..capabilities()
                },
                &route(),
            ),
            reference,
            "capability surface must move the authority digest"
        );

        for (label, route) in [
            (
                "route fingerprint",
                RouteBinding::new("route-b", "fixture-model"),
            ),
            ("model", RouteBinding::new("route-a", "other-model")),
            (
                "capability generation",
                route().with_capability_generation("g1"),
            ),
            (
                "adaptive profile",
                route().with_adaptive_profile("balanced"),
            ),
            (
                "principal generation",
                route().with_principal_generation("p1"),
            ),
            ("lease", route().with_lease("lease-1")),
        ] {
            assert_ne!(
                digest(&base, &capabilities(), &route),
                reference,
                "{label} must move the authority digest"
            );
        }

        assert_ne!(
            authority_digest(
                &base,
                Uuid::new_v4(),
                &observation,
                &capabilities(),
                &route(),
                &objective_digest(OBJECTIVE),
            ),
            reference,
            "owner must move the authority digest"
        );
        assert_ne!(
            authority_digest(
                &base,
                owner,
                &observation,
                &capabilities(),
                &route(),
                &objective_digest("a different objective"),
            ),
            reference,
            "objective must move the authority digest"
        );
    }

    /// The seal carries no secret and no observed content.
    #[test]
    fn seal_identities_are_content_free() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let sealed = accepted(owner, &run);
        for identity in [
            sealed.proposal_fingerprint(),
            sealed.objective_digest(),
            sealed.nonce(),
        ] {
            assert!(!identity.contains("Ada"));
            assert!(!identity.contains("Lovelace"));
        }
        assert_eq!(sealed.proposal_fingerprint().len(), 64);
        assert!(sealed
            .proposal_fingerprint()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
    }

    /// A run whose objective the operator never authored cannot even produce a
    /// context, so no model turn against it is possible.
    #[test]
    fn a_run_without_an_objective_has_no_context() {
        let owner = Uuid::new_v4();
        let mut run = ready_run(owner);
        run.task_spec = None;
        assert_eq!(
            ModelProposalContext::from_host_state(&run, owner, capabilities(), OBJECTIVE, route())
                .expect_err("no objective")
                .code,
            ComputerErrorCode::UnverifiedCompletion
        );
    }

    /// A frame older than the run's own staleness policy is not a premise a
    /// model may reason over.
    #[test]
    fn an_over_age_frame_cannot_produce_a_context() {
        let owner = Uuid::new_v4();
        let mut run = ready_run(owner);
        if let Some(current) = run.current_observation.as_mut() {
            current.captured_at = Utc::now()
                - Duration::milliseconds(run.limits.max_observation_age_millis as i64 + 1_000);
        }
        assert_eq!(
            ModelProposalContext::from_host_state(&run, owner, capabilities(), OBJECTIVE, route())
                .expect_err("stale frame")
                .code,
            ComputerErrorCode::StaleObservation
        );
    }

    /// A grant that does not carry the action's class refuses the proposal even
    /// though the frame and element are valid.
    #[test]
    fn a_grant_without_the_action_class_refuses_the_proposal() {
        let owner = Uuid::new_v4();
        let mut run = ready_run(owner);
        if let Some(grant) = run.grant.as_mut() {
            grant.action_classes = BTreeSet::from([ActionClass::Semantic]);
        }
        assert_eq!(
            accept_model_proposal(&context(owner, &run), &set_value_raw())
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
        let context = ModelProposalContext::from_host_state(
            &run,
            owner,
            ComputerCapabilities {
                text_entry: false,
                ..capabilities()
            },
            OBJECTIVE,
            route(),
        )
        .expect("context");
        assert_eq!(
            accept_model_proposal(&context, &set_value_raw())
                .expect_err("backend does not advertise text entry")
                .code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    /// Control characters and empty summaries reach an operator's approval
    /// prompt, so they are refused rather than rendered.
    #[test]
    fn hostile_summaries_are_refused() {
        let owner = Uuid::new_v4();
        let run = ready_run(owner);
        let context = context(owner, &run);
        for summary in ["", "   ", "ok\u{0}injected", "ok\u{1b}[2Jcleared"] {
            let raw = RawModelProposal::new(
                serde_json::json!({
                    "observation_id": "observation-1",
                    "action_type": "set_value",
                    "element_id": "ephemeral-name",
                    "text": "Ada Lovelace",
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
