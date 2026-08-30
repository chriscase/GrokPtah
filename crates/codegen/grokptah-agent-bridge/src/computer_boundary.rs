//! The strict boundary between untrusted model output and a Computer Run
//! proposal.
//!
//! Everything a provider returns is untrusted bytes. This module is the only
//! place those bytes become a typed proposal, and it is deliberately a
//! *narrowing* layer: it can reject, never widen. After it accepts,
//! [`crate::computer_use::ComputerPolicy`] still revalidates the action
//! against the live run at dispatch time and remains the single physical-action
//! authority. Nothing here dispatches, and nothing here can make the kernel
//! accept something it would otherwise refuse.
//!
//! ## Host-minted, private challenge
//!
//! A [`ProposalTicket`] is minted by the host for exactly one observation of
//! one run at one control epoch. It carries a random challenge that the host
//! puts in the prompt and the model must echo back verbatim.
//!
//! The ticket implements neither `Serialize` nor `Deserialize`, and the
//! challenge field is private with no accessor that returns it for
//! serialization. A ticket therefore cannot be forged from wire bytes,
//! replayed from a durable record, or leaked through a projection — it can
//! only be produced by [`ProposalTicket::mint`] inside this process.
//!
//! This challenge is **not** an authentication principal or an auth
//! generation. Host identity, credential generations, and durable record
//! authentication belong to the canonical authority spine (G1–G4) and are not
//! reimplemented here; the ticket binds to `run_id`, `observation_id`,
//! `sequence`, and `control_epoch`, which already exist on `main`.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::computer_use::{
    ActionClass, AdaptiveProfile, AdaptiveRecord, ComputerAction, ComputerErrorCode,
    ComputerObservation,
};

/// Hard ceiling on the raw provider body this boundary will even look at.
pub const MAX_PROPOSAL_BYTES: usize = 64 * 1024;
/// Hard ceiling on the model-authored summary.
pub const MAX_SUMMARY_BYTES: usize = 512;
/// Consecutive repeats tolerated before a run is stopped for no progress.
pub const MAX_STATIONARY_STRIKES: u32 = 2;

/// Why the boundary refused a proposal. A closed enum: a rejection never
/// carries provider text, observed content, or a filesystem path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryRejection {
    /// Not a single JSON object. Prose and fenced blocks land here: this
    /// boundary performs no recovery, extraction, or repair.
    NotJson,
    /// A complete value followed by more bytes.
    TrailingContent,
    /// The same key appeared twice. `serde_json` would silently take the last
    /// one, which lets a crafted body show one value to a reviewer and another
    /// to the parser.
    DuplicateKey,
    /// A field the closed schema does not define.
    UnknownField,
    /// A field of the wrong JSON type, including a string where a number is
    /// required. No coercion is attempted.
    WrongType,
    TooLarge,
    ProposalIdMismatch,
    ChallengeMismatch,
    ObservationMismatch,
    SequenceMismatch,
    TicketExpired,
    /// The run's control epoch moved after the ticket was minted — a pause,
    /// takeover, stop, or recovery happened. The proposal is dead.
    LeaseLost,
    /// A real action, but one only a local operator may perform.
    OperatorOnlyAction,
    UnknownAction,
    UnknownElement,
    InvalidSummary,
    CompletionCarriedAction,
    /// The model asserted the objective was met without an exact host
    /// verification of the postcondition.
    CompletionNotHostVerified,
    /// The same action against the same observation, again.
    Stationary,
    BudgetExhausted,
}

impl BoundaryRejection {
    /// Map to the kernel's closed error vocabulary so a caller can surface one
    /// error type. The boundary never invents a kernel code that implies more
    /// authority than it observed.
    pub fn error_code(self) -> ComputerErrorCode {
        match self {
            Self::NotJson
            | Self::TrailingContent
            | Self::DuplicateKey
            | Self::UnknownField
            | Self::WrongType
            | Self::TooLarge
            | Self::InvalidSummary
            | Self::CompletionCarriedAction
            | Self::UnknownAction => ComputerErrorCode::InvalidRequest,
            Self::ProposalIdMismatch | Self::ChallengeMismatch => ComputerErrorCode::Unauthorized,
            Self::ObservationMismatch | Self::SequenceMismatch | Self::UnknownElement => {
                ComputerErrorCode::StaleObservation
            }
            Self::TicketExpired | Self::LeaseLost => ComputerErrorCode::Conflict,
            Self::OperatorOnlyAction => ComputerErrorCode::ForbiddenAction,
            Self::CompletionNotHostVerified => ComputerErrorCode::UncertainOutcome,
            Self::Stationary | Self::BudgetExhausted => ComputerErrorCode::LimitReached,
        }
    }
}

/// A host-minted, single-observation proposal ticket.
///
/// Deliberately not `Serialize`/`Deserialize`, and its `Debug` is hand-written
/// so the challenge cannot reach a log through `{:?}`: see the module docs.
#[derive(Clone)]
pub struct ProposalTicket {
    proposal_id: String,
    run_id: String,
    observation_id: String,
    sequence: u64,
    control_epoch: u64,
    profile: AdaptiveProfile,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    /// Private. Never serialized, never projected, never logged.
    challenge: String,
}

/// Hand-written so the private challenge can never reach a log or a panic
/// message through `{:?}`. Every other field is already projectable.
impl std::fmt::Debug for ProposalTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposalTicket")
            .field("proposal_id", &self.proposal_id)
            .field("run_id", &self.run_id)
            .field("observation_id", &self.observation_id)
            .field("sequence", &self.sequence)
            .field("control_epoch", &self.control_epoch)
            .field("profile", &self.profile)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("challenge", &"<redacted>")
            .finish()
    }
}

impl ProposalTicket {
    /// Mint a ticket bound to exactly one observation of one run.
    ///
    /// `challenge_bytes` is supplied by the caller so tests are deterministic;
    /// production callers pass fresh random bytes.
    pub fn mint(
        run_id: &str,
        observation: &ComputerObservation,
        control_epoch: u64,
        profile: AdaptiveProfile,
        now: DateTime<Utc>,
        ttl: Duration,
        challenge_bytes: [u8; 32],
    ) -> Self {
        Self {
            proposal_id: Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
            control_epoch,
            profile,
            issued_at: now,
            expires_at: now + ttl,
            challenge: hex_encode(&challenge_bytes),
        }
    }

    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn control_epoch(&self) -> u64 {
        self.control_epoch
    }
    pub fn profile(&self) -> AdaptiveProfile {
        self.profile
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// The challenge, for inclusion in the outbound model prompt only.
    ///
    /// This is the single accessor. It is not `Display`, not `Serialize`, and
    /// not reachable from any projection.
    pub fn challenge_for_prompt(&self) -> &str {
        &self.challenge
    }

    fn challenge_digest(&self) -> String {
        sha256_hex(self.challenge.as_bytes())
    }
}

/// Host-recorded evidence of what was actually admitted for launch.
///
/// Minted by the host after acceptance, never by the model. Private to the
/// host: [`LaunchEvidenceProjection`] is what any surface may see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchEvidence {
    pub proposal_id: String,
    pub run_id: String,
    pub observation_id: String,
    pub sequence: u64,
    pub control_epoch: u64,
    pub profile: AdaptiveProfile,
    pub action_class: ActionClass,
    /// Digest of the canonical accepted action.
    pub action_digest: String,
    /// Proves the accepted proposal answered this exact ticket without
    /// retaining the challenge itself.
    pub challenge_digest: String,
    pub accepted_at: DateTime<Utc>,
}

/// Redaction-safe launch evidence.
///
/// Excludes `challenge_digest` and `action_digest`: both are digests over
/// live host-minted or model-authored material, and a digest of a live secret
/// is still an oracle for it. A surface may learn that a launch was admitted
/// and of what class, never what was typed or what the challenge was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchEvidenceProjection {
    pub proposal_id: String,
    pub sequence: u64,
    pub control_epoch: u64,
    pub profile: AdaptiveProfile,
    pub action_class: ActionClass,
    pub accepted_at: DateTime<Utc>,
}

impl LaunchEvidence {
    pub fn project(&self) -> LaunchEvidenceProjection {
        LaunchEvidenceProjection {
            proposal_id: self.proposal_id.clone(),
            sequence: self.sequence,
            control_epoch: self.control_epoch,
            profile: self.profile,
            action_class: self.action_class,
            accepted_at: self.accepted_at,
        }
    }
}

/// An accepted proposal. The action still has to survive the kernel.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedProposal {
    pub action: ComputerAction,
    pub summary: String,
    pub evidence: LaunchEvidence,
}

/// The host's own verification that a completion claim is true.
///
/// Constructed only by the host after it re-observed the target; the model
/// cannot supply one.
#[derive(Debug, Clone, Copy)]
pub struct CompletionVerification {
    postcondition_met: bool,
}

impl CompletionVerification {
    /// Record an exact host observation of the postcondition.
    pub fn observed(postcondition_met: bool) -> Self {
        Self { postcondition_met }
    }
    pub fn postcondition_met(self) -> bool {
        self.postcondition_met
    }
}

/// What the boundary produced.
#[derive(Debug, Clone, PartialEq)]
pub enum BoundaryOutcome {
    Act(Box<AcceptedProposal>),
    /// The model claimed completion and the host verified the postcondition.
    Complete {
        summary: String,
    },
}

// ---------------------------------------------------------------------------
// Closed wire grammar
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawProposal {
    proposal_id: String,
    challenge: String,
    observation_id: String,
    sequence: u64,
    decision: RawDecision,
    #[serde(default)]
    action: Option<RawAction>,
    summary: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RawDecision {
    Act,
    Complete,
}

/// The closed action grammar a model may propose.
///
/// `pointer_click` and `key_chord` are deliberately absent: they are real
/// kernel actions, but operator-only at this boundary. A body naming one is
/// rejected as [`BoundaryRejection::OperatorOnlyAction`] rather than as an
/// unknown action, so the refusal is legible.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "snake_case")]
enum RawAction {
    ActivateTarget,
    Invoke {
        element_id: String,
    },
    SetValue {
        element_id: String,
        text: String,
    },
    Select {
        element_id: String,
    },
    Scroll {
        #[serde(default)]
        element_id: Option<String>,
        delta_x: i32,
        delta_y: i32,
    },
    Wait {
        millis: u64,
    },
}

impl RawAction {
    fn into_action(self) -> ComputerAction {
        match self {
            Self::ActivateTarget => ComputerAction::ActivateTarget,
            Self::Invoke { element_id } => ComputerAction::Invoke { element_id },
            Self::SetValue { element_id, text } => ComputerAction::SetValue { element_id, text },
            Self::Select { element_id } => ComputerAction::Select { element_id },
            Self::Scroll {
                element_id,
                delta_x,
                delta_y,
            } => ComputerAction::Scroll {
                element_id,
                delta_x,
                delta_y,
            },
            Self::Wait { millis } => ComputerAction::Wait { millis },
        }
    }
}

/// Action type names that exist in the kernel but are operator-only here.
const OPERATOR_ONLY_ACTIONS: [&str; 2] = ["pointer_click", "key_chord"];

// ---------------------------------------------------------------------------
// Duplicate-key-rejecting JSON
// ---------------------------------------------------------------------------

/// A `serde_json::Value` that refuses duplicate object keys at any depth.
///
/// `serde_json` accepts duplicates and keeps the last, so `{"a":1,"a":2}`
/// parses. That lets one body read one way to a human and another to the
/// parser, which is exactly the fabrication this boundary exists to stop.
struct StrictValue(serde_json::Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = StrictValue;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with no duplicate object keys")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(serde_json::Value::Null))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(StrictValue(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(StrictValue(serde_json::Value::Array(items)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                let mut seen = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(DUPLICATE_KEY_MARKER));
                    }
                    let StrictValue(value) = map.next_value()?;
                    object.insert(key, value);
                }
                Ok(StrictValue(serde_json::Value::Object(object)))
            }
        }

        deserializer.deserialize_any(V)
    }
}

const DUPLICATE_KEY_MARKER: &str = "grokptah-duplicate-object-key";

// ---------------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------------

/// Turn raw provider bytes into an accepted proposal, or refuse.
///
/// The caller must have already incremented its attempt counter: a refusal
/// here costs exactly what an acceptance costs.
///
/// `host_completion` is the host's own postcondition observation. It is
/// `None` unless the host actually re-observed the target, and a model
/// completion claim without one is refused.
pub fn accept_model_proposal(
    ticket: &ProposalTicket,
    raw: &[u8],
    observation: &ComputerObservation,
    live_control_epoch: u64,
    record: &AdaptiveRecord,
    host_completion: Option<CompletionVerification>,
    now: DateTime<Utc>,
) -> Result<BoundaryOutcome, BoundaryRejection> {
    // Budget and lease are checked before the body is even parsed: an
    // exhausted or fenced run must not pay to look at untrusted bytes.
    if record.budget_exhausted() {
        return Err(BoundaryRejection::BudgetExhausted);
    }
    if live_control_epoch != ticket.control_epoch {
        return Err(BoundaryRejection::LeaseLost);
    }
    if now >= ticket.expires_at || now < ticket.issued_at {
        return Err(BoundaryRejection::TicketExpired);
    }
    if raw.len() > MAX_PROPOSAL_BYTES {
        return Err(BoundaryRejection::TooLarge);
    }

    let text = std::str::from_utf8(raw).map_err(|_| BoundaryRejection::NotJson)?;
    let parsed = parse_strict(text)?;

    // Identity: the model must answer this exact ticket.
    if !constant_time_eq(parsed.proposal_id.as_bytes(), ticket.proposal_id.as_bytes()) {
        return Err(BoundaryRejection::ProposalIdMismatch);
    }
    if !constant_time_eq(parsed.challenge.as_bytes(), ticket.challenge.as_bytes()) {
        return Err(BoundaryRejection::ChallengeMismatch);
    }

    // Freshness: the ticket, the live observation, and the echo must agree.
    if parsed.observation_id != ticket.observation_id
        || observation.observation_id != ticket.observation_id
    {
        return Err(BoundaryRejection::ObservationMismatch);
    }
    if parsed.sequence != ticket.sequence || observation.sequence != ticket.sequence {
        return Err(BoundaryRejection::SequenceMismatch);
    }

    let summary = parsed.summary.trim().to_string();
    if summary.is_empty() || summary.len() > MAX_SUMMARY_BYTES || summary.contains('\0') {
        return Err(BoundaryRejection::InvalidSummary);
    }

    if parsed.decision == RawDecision::Complete {
        if parsed.action.is_some() {
            return Err(BoundaryRejection::CompletionCarriedAction);
        }
        // A model asserting success is not evidence of success.
        let verification = host_completion.ok_or(BoundaryRejection::CompletionNotHostVerified)?;
        if !verification.postcondition_met() {
            return Err(BoundaryRejection::CompletionNotHostVerified);
        }
        return Ok(BoundaryOutcome::Complete { summary });
    }

    let raw_action = parsed.action.ok_or(BoundaryRejection::UnknownAction)?;
    let action = raw_action.into_action();

    // Every referenced element must exist in the exact current observation.
    if let Some(element_id) = action.referenced_element() {
        if observation.element(element_id).is_none() {
            return Err(BoundaryRejection::UnknownElement);
        }
    }

    let action_digest = action_digest(&action);
    if is_stationary(record, &action_digest) {
        return Err(BoundaryRejection::Stationary);
    }

    Ok(BoundaryOutcome::Act(Box::new(AcceptedProposal {
        action: action.clone(),
        summary,
        evidence: LaunchEvidence {
            proposal_id: ticket.proposal_id.clone(),
            run_id: ticket.run_id.clone(),
            observation_id: ticket.observation_id.clone(),
            sequence: ticket.sequence,
            control_epoch: ticket.control_epoch,
            profile: ticket.profile,
            action_class: action.class(),
            action_digest,
            challenge_digest: ticket.challenge_digest(),
            accepted_at: now,
        },
    })))
}

/// Parse the closed schema with no recovery of any kind.
fn parse_strict(text: &str) -> Result<RawProposal, BoundaryRejection> {
    // Reject duplicate keys first, at every depth.
    let mut de = serde_json::Deserializer::from_str(text);
    let StrictValue(value) = match StrictValue::deserialize(&mut de) {
        Ok(value) => value,
        Err(error) if error.to_string().contains(DUPLICATE_KEY_MARKER) => {
            return Err(BoundaryRejection::DuplicateKey)
        }
        Err(_) => return Err(BoundaryRejection::NotJson),
    };
    // A complete value followed by anything else is not one proposal.
    de.end().map_err(|_| BoundaryRejection::TrailingContent)?;

    let serde_json::Value::Object(object) = &value else {
        return Err(BoundaryRejection::NotJson);
    };
    // Name operator-only actions before the closed grammar calls them unknown.
    if let Some(serde_json::Value::Object(action)) = object.get("action") {
        if let Some(serde_json::Value::String(kind)) = action.get("type") {
            if OPERATOR_ONLY_ACTIONS.contains(&kind.as_str()) {
                return Err(BoundaryRejection::OperatorOnlyAction);
            }
        }
    }

    serde_json::from_value::<RawProposal>(value).map_err(|error| classify(&error.to_string()))
}

fn classify(message: &str) -> BoundaryRejection {
    if message.contains("unknown field") {
        BoundaryRejection::UnknownField
    } else if message.contains("unknown variant") {
        BoundaryRejection::UnknownAction
    } else if message.contains("invalid type") || message.contains("invalid value") {
        BoundaryRejection::WrongType
    } else if message.contains("missing field") {
        BoundaryRejection::UnknownField
    } else {
        BoundaryRejection::NotJson
    }
}

/// A proposal repeating the last accepted action against an unchanged
/// observation makes no progress.
fn is_stationary(record: &AdaptiveRecord, action_digest: &str) -> bool {
    record
        .last_action_digest
        .as_deref()
        .is_some_and(|last| last == action_digest)
        && record.stationary_strikes >= MAX_STATIONARY_STRIKES
}

/// Record the outcome of one boundary turn on the durable adaptive record.
///
/// Called by the host for both acceptance and refusal so the ledger stays
/// balanced: `attempts == accepted + rejected` always.
pub fn note_turn(record: &mut AdaptiveRecord, outcome: Result<&AcceptedProposal, ()>) {
    record.spend.attempts = record.spend.attempts.saturating_add(1);
    match outcome {
        Ok(accepted) => {
            record.spend.accepted = record.spend.accepted.saturating_add(1);
            let digest = accepted.evidence.action_digest.clone();
            if record.last_action_digest.as_deref() == Some(digest.as_str()) {
                record.stationary_strikes = record.stationary_strikes.saturating_add(1);
            } else {
                record.stationary_strikes = 0;
            }
            record.last_action_digest = Some(digest);
        }
        Err(()) => {
            record.spend.rejected = record.spend.rejected.saturating_add(1);
        }
    }
}

fn action_digest(action: &ComputerAction) -> String {
    let canonical = serde_json::to_vec(action).unwrap_or_default();
    sha256_hex(&canonical)
}

fn sha256_hex(bytes: &[u8]) -> String {
    // A single SHA-256, hex encoded. Never `sha256_hex(&finalize())`, which
    // would be a digest of a digest and would silently disagree with any
    // other implementation of "the sha256 of these bytes".
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::{
        project_run_at, AdaptiveProfileProjection, ComputerRun, ComputerStore, ComputerTarget,
        ObservationGeometry, PackagedQualification, QualificationVerdict, SemanticAction,
        SemanticElement, Sensitivity,
    };
    use uuid::Uuid;

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn observation() -> ComputerObservation {
        ComputerObservation {
            observation_id: "obs-1".into(),
            sequence: 11,
            target: target(),
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 2.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "field".into(),
                role: "text_field".into(),
                label: Some("Patient name".into()),
                value: Some("Ada Lovelace".into()),
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

    /// A run whose adaptive record is mid-flight, as it would be when a
    /// process dies between turns.
    fn run_with_adaptive() -> ComputerRun {
        let mut run = ComputerRun::new(Uuid::new_v4(), None, target(), Default::default()).unwrap();
        let mut record = AdaptiveRecord::open(AdaptiveProfile::Economy, "efficient", Utc::now());
        record.spend.attempts = 4;
        record.spend.accepted = 3;
        record.spend.rejected = 1;
        record.stationary_strikes = 2;
        record.last_action_digest = Some("a".repeat(64));
        run.adaptive = Some(record);
        run
    }

    #[test]
    fn adaptive_truth_survives_a_crash_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_with_adaptive();
        let run_id = run.run_id.clone();

        {
            let store = ComputerStore::open(dir.path()).unwrap();
            store.save_run(&run).unwrap();
            // Process dies here; the store is dropped without a clean shutdown.
        }

        let store = ComputerStore::open(dir.path()).unwrap();
        let recovered = store.load_run(&run_id).unwrap().expect("run survived");
        let record = recovered.adaptive.expect("adaptive record survived");

        assert_eq!(record.profile, AdaptiveProfile::Economy);
        assert_eq!(
            record.ingested_as, "efficient",
            "ingest spelling is retained"
        );
        assert_eq!(record.spend.attempts, 4);
        assert!(record.spend.is_balanced());
        assert_eq!(
            record.stationary_strikes, 2,
            "a restart must not reset a no-progress loop"
        );
        assert_eq!(
            record.last_action_digest.as_deref(),
            Some("a".repeat(64).as_str())
        );
    }

    #[test]
    fn a_pre_restart_proposal_is_stranded_by_the_recovered_epoch() {
        let observation = observation();
        let ticket = ProposalTicket::mint(
            "run-1",
            &observation,
            7,
            AdaptiveProfile::Balanced,
            Utc::now(),
            Duration::seconds(30),
            [1u8; 32],
        );
        // Recovery advances the control epoch, so an in-flight response that
        // was minted before the restart can never be applied after it.
        let rejection = accept_model_proposal(
            &ticket,
            b"{}",
            &observation,
            8,
            &AdaptiveRecord::open(AdaptiveProfile::Balanced, "balanced", Utc::now()),
            None,
            Utc::now(),
        )
        .unwrap_err();
        assert_eq!(rejection, BoundaryRejection::LeaseLost);
    }

    #[test]
    fn a_legacy_run_record_recovers_with_no_adaptive_authority() {
        let dir = tempfile::tempdir().unwrap();
        let mut run = ComputerRun::new(Uuid::new_v4(), None, target(), Default::default()).unwrap();
        run.adaptive = None;
        let run_id = run.run_id.clone();

        let store = ComputerStore::open(dir.path()).unwrap();
        store.save_run(&run).unwrap();

        // Strip the field entirely, as a record written before it existed.
        let mut value = serde_json::to_value(&run).unwrap();
        value.as_object_mut().unwrap().remove("adaptive");
        assert!(value.get("adaptive").is_none());
        let legacy: ComputerRun = serde_json::from_value(value).unwrap();

        assert!(
            legacy.adaptive.is_none(),
            "absent means no adaptive authority, not no constraints"
        );
        // And a reload of the saved record agrees.
        let reloaded = store.load_run(&run_id).unwrap().unwrap();
        assert!(reloaded.adaptive.is_none());
    }

    #[test]
    fn the_public_projection_carries_no_secret_label_value_or_path() {
        let mut run = run_with_adaptive();
        run.current_observation = Some(observation());
        let projection = project_run_at(&run, Utc::now());

        let adaptive = projection.adaptive.clone().expect("adaptive is projected");
        assert_eq!(adaptive.profile, AdaptiveProfile::Economy);
        assert!(adaptive.ingested_alias, "an alias ingest is visible");
        assert_eq!(adaptive.stationary_strikes, 2);

        let rendered = serde_json::to_string(&projection).unwrap();
        for leak in [
            "Patient name",  // element label
            "Ada Lovelace",  // element value
            "efficient",     // the raw ingest spelling is not a wire value
            &"a".repeat(64), // the last-action digest
            "obs-1",         // the internal observation id
        ] {
            assert!(
                !rendered.contains(leak),
                "projection leaked {leak}: {rendered}"
            );
        }
        // The canonical profile name is present; the alias never is.
        assert!(rendered.contains("\"economy\""));
        // No filesystem path can appear in a projection at all.
        assert!(!rendered.contains("/home/"));
        assert!(!rendered.contains("\\\\"));
    }

    #[test]
    fn a_projection_of_a_record_without_adaptive_state_is_absent_not_defaulted() {
        let run = ComputerRun::new(Uuid::new_v4(), None, target(), Default::default()).unwrap();
        let projection = project_run_at(&run, Utc::now());
        assert!(projection.adaptive.is_none());
        let rendered = serde_json::to_string(&projection).unwrap();
        assert!(rendered.contains("\"adaptive\":null"));
    }

    #[test]
    fn a_simulator_run_cannot_be_projected_as_a_packaged_pass() {
        // The simulator is the only backend this container can run. Whatever
        // it did, the packaged/VM verdict is unavailable with named reasons.
        let qualification = PackagedQualification::from_simulator();
        assert_eq!(qualification.verdict, QualificationVerdict::Unavailable);
        assert!(!qualification.is_qualified());

        // And there is no constructor that produces anything else: the only
        // public builders are `unavailable` and `from_simulator`.
        let rendered = serde_json::to_string(&qualification).unwrap();
        assert!(!rendered.contains("\"pass\""));
        assert!(!rendered.contains("\"partial\""));
    }

    #[test]
    fn launch_evidence_projects_without_its_digests() {
        let observation = observation();
        let ticket = ProposalTicket::mint(
            "run-1",
            &observation,
            5,
            AdaptiveProfile::HighAssurance,
            Utc::now(),
            Duration::seconds(30),
            [3u8; 32],
        );
        let body = format!(
            r#"{{"proposalId":"{}","challenge":"{}","observationId":"{}","sequence":{},"decision":"act","action":{{"type":"set_value","element_id":"field","text":"Ada"}},"summary":"fill"}}"#,
            ticket.proposal_id(),
            ticket.challenge_for_prompt(),
            ticket.observation_id(),
            ticket.sequence(),
        );
        let record = AdaptiveRecord::open(AdaptiveProfile::HighAssurance, "frontier", Utc::now());
        let BoundaryOutcome::Act(accepted) = accept_model_proposal(
            &ticket,
            body.as_bytes(),
            &observation,
            5,
            &record,
            None,
            Utc::now(),
        )
        .unwrap() else {
            panic!("expected an action");
        };

        // Host-side evidence retains the binding proof...
        assert_eq!(accepted.evidence.challenge_digest.len(), 64);
        assert_eq!(accepted.evidence.action_digest.len(), 64);
        // ...and the projection drops both.
        let rendered = serde_json::to_string(&accepted.evidence.project()).unwrap();
        assert!(!rendered.contains(&accepted.evidence.challenge_digest));
        assert!(!rendered.contains(&accepted.evidence.action_digest));
        assert!(!rendered.contains("Ada"));
        assert!(rendered.contains("\"high_assurance\""));
    }

    #[test]
    fn the_challenge_digest_is_a_single_sha256_not_a_digest_of_a_digest() {
        // A `sha256_hex(&hasher.finalize())` shape would silently disagree
        // with every other implementation of "the sha256 of these bytes".
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(b"grokptah");
            hex_encode(&hasher.finalize())
        };
        assert_eq!(sha256_hex(b"grokptah"), expected);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_challenge_never_reaches_a_debug_rendering() {
        let observation = observation();
        let ticket = ProposalTicket::mint(
            "run-1",
            &observation,
            1,
            AdaptiveProfile::Economy,
            Utc::now(),
            Duration::seconds(30),
            [42u8; 32],
        );
        let rendered = format!("{ticket:?}");
        assert!(
            !rendered.contains(ticket.challenge_for_prompt()),
            "Debug leaked the challenge: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // The rest of the ticket stays legible for diagnostics.
        assert!(rendered.contains(ticket.proposal_id()));
    }

    #[test]
    fn adaptive_projection_matches_the_record_it_came_from() {
        let record = AdaptiveRecord::open(AdaptiveProfile::Balanced, "balanced", Utc::now());
        let projection = AdaptiveProfileProjection::of(&record);
        assert_eq!(projection.profile, AdaptiveProfile::Balanced);
        assert!(!projection.ingested_alias);
        assert_eq!(projection.budget, AdaptiveProfile::Balanced.budget());
        assert!(!projection.budget_exhausted);
    }
}
