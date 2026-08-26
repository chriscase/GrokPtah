//! Public projections of the durable admission ledger.
//!
//! One shape, four consumers: the SDK, the MCP control plane, the web broker,
//! and the desktop. Each of those previously reached into whichever durable
//! record was nearest, which is how a private field ends up on a wire nobody
//! audited. Everything they are allowed to see is defined here, once.
//!
//! The rule this module enforces is that a projection is a *narrowing*, never
//! a re-serialization. Durable records hold the prompt, the credential
//! fingerprints, and the raw provider detail; a projection holds an identity,
//! a state, and a bounded preview. New fields on a durable record therefore do
//! not appear on any wire until someone adds them here on purpose.
//!
//! The schemas in `docs/schemas/` are the cross-language contract, and
//! `schema_contract_matches_projection` below fails if the Rust type, the JSON
//! Schema, and the TypeScript declaration ever disagree.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::admission::{
    AttemptLease, AttemptLeaseState, ProviderSendRecord, ProviderSendState, TeardownUncertain,
};
use super::types::{RunRecord, RunState};

/// Contract version of every projection in this module. Bumped whenever a
/// field is added, removed, or changes meaning.
pub const PROJECTION_VERSION: u32 = 2;

/// What one admitted unit of work looks like to anything outside the bridge.
///
/// Deliberately absent: the prompt, the sealed authorization fingerprints, the
/// attempt owner, the send identity, and every provider detail string. Those
/// are execution material or internal identity; a consumer that needs to
/// display progress does not need any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionProjection {
    pub projection_version: u32,
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub state: RunState,
    /// The immutable execution specification key. Safe to publish: it is a
    /// digest, and it lets a consumer tell "same work" from "similar work".
    pub spec_key: Option<String>,
    /// One-based position in the bounded admission queue, while queued.
    pub queue_position: Option<usize>,
    /// Monotonic attempt number, once an attempt exists.
    pub attempt: Option<u64>,
    pub attempt_state: Option<AttemptProjectionState>,
    /// What is durably known about whether the work reached the provider.
    pub provider_send_state: Option<ProviderSendProjectionState>,
    /// Seconds since this attempt's lease was last heartbeat, when it holds
    /// one. A consumer watching a long turn needs to distinguish "working" from
    /// "the holder stopped reporting".
    pub heartbeat_age_seconds: Option<i64>,
    /// Seconds until this attempt's lease expires, negative once it has.
    pub lease_expires_in_seconds: Option<i64>,
    /// The concrete provider route this work is bound to, as a fingerprint.
    /// Publishing the fingerprint lets a consumer notice a route change
    /// without ever seeing an endpoint or a credential.
    pub route_revision: Option<String>,
    /// True when a previous teardown could not be established. The run's
    /// capacity and lease are fenced, and no new attempt is authorized.
    pub teardown_uncertain: bool,
    /// Bounded, redacted explanation of that uncertainty, when it applies.
    pub teardown_detail: Option<String>,
    /// Whether a new attempt for this work is currently permitted.
    ///
    /// False whenever the outcome of previous work is unknown — the case where
    /// retrying risks doing it twice.
    pub retry_eligible: bool,
    /// Whether this run currently occupies an admission slot.
    pub capacity_fenced: bool,
    /// Remaining wall-clock budget in milliseconds, from the sealed bounds.
    pub remaining_duration_ms: Option<u64>,
    /// Round budget from the sealed bounds.
    pub max_rounds: u32,
    /// Bounded, redacted preview. Never the execution input.
    pub prompt_preview: String,
    pub terminal_result: Option<String>,
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Public form of an attempt lease's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptProjectionState {
    Held,
    Released,
    /// Held, but past its durable heartbeat deadline. Distinguished from
    /// `held` because it is the state a reconciler will act on.
    Expired,
}

/// Public form of provider-send evidence. Mirrors the durable state exactly:
/// consumers must be able to tell "not sent" from "unknown", because only one
/// of them is safe to retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendProjectionState {
    KnownNotSent,
    Sending,
    Uncertain,
    Sent,
}

impl From<ProviderSendState> for ProviderSendProjectionState {
    fn from(value: ProviderSendState) -> Self {
        match value {
            ProviderSendState::KnownNotSent => Self::KnownNotSent,
            ProviderSendState::Sending => Self::Sending,
            ProviderSendState::Uncertain => Self::Uncertain,
            ProviderSendState::Sent => Self::Sent,
        }
    }
}

/// Narrow the durable records into the one public shape.
pub fn project_admission(
    run: &RunRecord,
    lease: Option<&AttemptLease>,
    send: Option<&ProviderSendRecord>,
    uncertainty: Option<&TeardownUncertain>,
    route_revision: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> AdmissionProjection {
    let attempt_state = lease.map(|lease| match lease.state {
        AttemptLeaseState::Released => AttemptProjectionState::Released,
        AttemptLeaseState::Held if lease.is_expired(now) => AttemptProjectionState::Expired,
        AttemptLeaseState::Held => AttemptProjectionState::Held,
    });
    let heartbeat_age_seconds =
        lease.map(|lease| now.signed_duration_since(lease.heartbeat_at).num_seconds());
    let lease_expires_in_seconds = lease.map(|lease| {
        let deadline = lease.heartbeat_at
            + chrono::Duration::milliseconds(lease.lease_ttl_ms.min(i64::MAX as u64) as i64);
        deadline.signed_duration_since(now).num_seconds()
    });
    // Retry is permitted only when nothing about previous work is unknown.
    // Two independent things can make it unknown: an unresolved teardown, and
    // a provider send whose outcome was never observed.
    let retry_eligible = uncertainty.is_none()
        && send
            .map(|send| send.state.permits_new_attempt() || send.state.permits_completion())
            .unwrap_or(true)
        && !matches!(run.state, RunState::Starting | RunState::Running);
    let remaining_duration_ms = run.state.is_terminal().then_some(0).or_else(|| {
        let elapsed = now.signed_duration_since(run.updated_at).num_milliseconds();
        Some(
            run.bounds
                .max_duration_ms
                .saturating_sub(elapsed.max(0) as u64),
        )
    });
    AdmissionProjection {
        projection_version: PROJECTION_VERSION,
        run_id: run.run_id.clone(),
        session_id: run.session_id,
        workspace: run.workspace.clone(),
        state: run.state,
        spec_key: run.spec_key.clone(),
        queue_position: run.queue_position,
        attempt: lease.map(|lease| lease.attempt),
        attempt_state,
        provider_send_state: send.map(|send| send.state.into()),
        heartbeat_age_seconds,
        lease_expires_in_seconds,
        route_revision: route_revision.map(str::to_string),
        teardown_uncertain: uncertainty.is_some(),
        teardown_detail: uncertainty.map(|record| record.reason.clone()),
        retry_eligible,
        capacity_fenced: run.state.is_dispatched() || uncertainty.is_some(),
        remaining_duration_ms,
        max_rounds: run.bounds.max_rounds,
        prompt_preview: run.prompt_preview.clone(),
        terminal_result: run.terminal_result.clone(),
        error_code: run.error_code.clone(),
        created_at: run.created_at.to_rfc3339(),
        updated_at: run.updated_at.to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{RunAggregates, RunBounds};

    fn schema_dir() -> std::path::PathBuf {
        // The bridge is its own workspace root, four levels below the repo.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/schemas")
    }

    fn sample() -> AdmissionProjection {
        project_admission(&sample_run(), None, None, None, None, chrono::Utc::now())
    }

    fn sample_run() -> RunRecord {
        RunRecord {
            run_id: "run-1".into(),
            session_id: Uuid::nil(),
            workspace: "/tmp/project".into(),
            request_id: "req-1".into(),
            client_id: Some("mcp".into()),
            state: RunState::Queued,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: Some(2),
            spec_key: Some("a".repeat(64)),
            bounds: RunBounds::default(),
            prompt_preview: "fix the failing test".into(),
            start_seq: None,
            end_seq: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    /// The Rust type, the JSON Schema, and the TypeScript declaration must
    /// name exactly the same fields. Drift between them is how one consumer
    /// starts reading a field another never publishes.
    #[test]
    fn schema_contract_matches_projection() {
        let value = serde_json::to_value(sample()).unwrap();
        let mut rust_fields: Vec<String> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::from)
            .collect();
        rust_fields.sort();

        let schema_text =
            std::fs::read_to_string(schema_dir().join("durable-admission.schema.json"))
                .expect("durable-admission.schema.json must exist");
        let schema: serde_json::Value = serde_json::from_str(&schema_text).unwrap();
        let projection = &schema["$defs"]["AdmissionProjection"];
        let mut schema_fields: Vec<String> = projection["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::from)
            .collect();
        schema_fields.sort();
        assert_eq!(
            rust_fields, schema_fields,
            "the JSON Schema and the Rust projection describe different fields"
        );
        assert_eq!(
            projection["additionalProperties"],
            serde_json::Value::Bool(false),
            "the schema must be strict, like the Rust type"
        );

        let dts = std::fs::read_to_string(schema_dir().join("durable-admission.d.ts"))
            .expect("durable-admission.d.ts must exist");
        let body = dts
            .split("export interface AdmissionProjection {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("AdmissionProjection interface must exist");
        let mut ts_fields: Vec<String> = body
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                // Skip blank lines and every form of comment, including the
                // single-line `/** ... */` doc comments the declarations use.
                if line.is_empty()
                    || line.starts_with("//")
                    || line.starts_with('*')
                    || line.starts_with("/*")
                {
                    return None;
                }
                let name = line.split(':').next()?.trim().trim_end_matches('?');
                (!name.is_empty()).then(|| name.to_string())
            })
            .collect();
        ts_fields.sort();
        assert_eq!(
            rust_fields, ts_fields,
            "the TypeScript declaration and the Rust projection describe different fields"
        );
    }

    /// A projection must never carry execution material or credentials, in any
    /// field name or in any value.
    #[test]
    fn projection_never_carries_private_material() {
        let value = serde_json::to_value(sample()).unwrap();
        let encoded = serde_json::to_string(&value).unwrap().to_lowercase();
        for forbidden in [
            "\"prompt\"",
            "\"credential",
            "\"token\"",
            "\"bearer",
            "\"principalrevision\"",
            "\"policyrevision\"",
            "\"ownerid\"",
            "\"sendid\"",
            "\"attemptid\"",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "projection exposed {forbidden}: {encoded}"
            );
        }
        // The bounded preview is the one prompt-derived field, and it is
        // published under a name that says so.
        assert!(encoded.contains("\"promptpreview\""));

        // `routeRevision` *is* published, deliberately, so a consumer can see
        // that the provider route changed. It must be a fingerprint and never
        // the route itself: no endpoint, no host, no credential.
        let with_route = project_admission(
            &sample_run(),
            None,
            None,
            None,
            Some(&"a".repeat(64)),
            chrono::Utc::now(),
        );
        let route = with_route.route_revision.clone().unwrap();
        assert_eq!(route.len(), 64);
        assert!(route.chars().all(|c| c.is_ascii_hexdigit()));
        let encoded = serde_json::to_string(&with_route).unwrap();
        for leak in ["http://", "https://", "bearer", "api-key", "sk-"] {
            assert!(
                !encoded.to_lowercase().contains(leak),
                "the route fingerprint must not carry {leak}"
            );
        }
    }

    /// Every provider-send state survives the projection distinctly. Merging
    /// `known_not_sent` into `uncertain` would let a consumer retry work that
    /// may already have run.
    #[test]
    fn provider_send_states_project_one_to_one() {
        let pairs = [
            (ProviderSendState::KnownNotSent, "known_not_sent"),
            (ProviderSendState::Sending, "sending"),
            (ProviderSendState::Uncertain, "uncertain"),
            (ProviderSendState::Sent, "sent"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (state, expected) in pairs {
            let projected: ProviderSendProjectionState = state.into();
            let encoded = serde_json::to_string(&projected).unwrap();
            assert_eq!(encoded, format!("\"{expected}\""));
            assert!(seen.insert(encoded), "two states projected the same way");
        }
    }

    #[test]
    fn projection_rejects_unknown_fields() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("prompt".into(), serde_json::json!("leak"));
        assert!(serde_json::from_value::<AdmissionProjection>(value).is_err());
    }
}
