//! Transport-neutral durable routines and activation records (#306).
//!
//! A routine is a trigger definition. An activation is one attributable
//! attempt to make Work eligible through the existing workload API. Routines
//! never own execution, never resume an interrupted model invocation, and
//! never approve tools, promote code, or grant Computer Use.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{merge_bounds, AgentSpec, OrchError, OrchErrorCode, RunBounds};
use super::workload::{invalid, WorkItem, WorkPolicy};

pub const ROUTINE_SCHEMA_VERSION: u32 = 1;
pub const MAX_ROUTINE_NAME_BYTES: usize = 128;
pub const MAX_ROUTINE_PAYLOAD_BYTES: usize = 8 * 1024;
pub const MAX_ROUTINE_ACTOR_BYTES: usize = 256;
pub const MAX_ROUTINE_TIMEZONE_BYTES: usize = 64;
pub const MAX_ACTIVATION_HISTORY: usize = 128;
pub const MAX_CATCH_UP_OCCURRENCES: usize = 8;
pub const MAX_ROUTINE_INTERVAL_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
pub const MIN_ROUTINE_INTERVAL_MS: u64 = 1_000;
pub const DEFAULT_ROUTINE_MAX_IN_FLIGHT: u32 = 1;
pub const DEFAULT_CIRCUIT_FAILURES: u32 = 5;
pub const DEFAULT_BACKOFF_MS: u64 = 5_000;
pub const MAX_BACKOFF_MS: u64 = 60 * 60 * 1_000;

/// Injected clock so schedule, downtime, and backoff tests do not read the
/// wall clock. Production uses [`SystemClock`].
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone)]
pub struct FakeClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl FakeClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock() = now;
    }

    pub fn advance(&self, delta: Duration) {
        let mut now = self.now.lock();
        *now += delta;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineLifecycle {
    Enabled,
    Paused,
    Disabled,
}

impl RoutineLifecycle {
    pub fn allows_scheduled_fire(self) -> bool {
        matches!(self, Self::Enabled)
    }

    pub fn allows_manual_fire(self) -> bool {
        matches!(self, Self::Enabled | Self::Paused)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedRunPolicy {
    /// After downtime, discard every missed occurrence and wait for the next
    /// future slot. A single still-current slot is fired.
    #[default]
    Skip,
    /// After downtime, create exactly one activation for the latest missed
    /// slot, then wait for the next future slot.
    Coalesce,
    /// After downtime, create one activation per missed slot up to
    /// [`MAX_CATCH_UP_OCCURRENCES`], preferring the most recent slots.
    CatchUp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Skip this activation when the routine already has the configured number
    /// of non-terminal Work items. This is not a second queue.
    #[default]
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAdapterKind {
    Webhook,
    GitHub,
    Message,
}

impl ExternalAdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::GitHub => "github",
            Self::Message => "message",
        }
    }
}

/// Trigger kinds for this slice plus a reserved external boundary.
///
/// Webhook, GitHub, and message adapters must enter through
/// [`RoutineTrigger::External`] and the store ingest seam. They are stored
/// and validated but cannot create Work until a later adapter PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutineTrigger {
    Manual,
    OneShot {
        at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
    },
    Interval {
        every_ms: u64,
        timezone: String,
        anchor: DateTime<Utc>,
    },
    Calendar {
        timezone: String,
        hour: u8,
        minute: u8,
        /// Monday=0 … Sunday=6. `None` means every day.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weekday: Option<u8>,
    },
    External {
        adapter: ExternalAdapterKind,
    },
}

impl RoutineTrigger {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::OneShot { .. } => "one_shot",
            Self::Interval { .. } => "interval",
            Self::Calendar { .. } => "calendar",
            Self::External { .. } => "external",
        }
    }

    pub fn timezone_name(&self) -> &str {
        match self {
            Self::Interval { timezone, .. } | Self::Calendar { timezone, .. } => timezone,
            _ => "UTC",
        }
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        match self {
            Self::Manual | Self::External { .. } => Ok(()),
            Self::OneShot { at, expires_at } => {
                if expires_at.is_some_and(|expires| expires < *at) {
                    return Err(invalid("one-shot expires_at precedes at"));
                }
                Ok(())
            }
            Self::Interval {
                every_ms,
                timezone,
                anchor: _,
            } => {
                if *every_ms < MIN_ROUTINE_INTERVAL_MS || *every_ms > MAX_ROUTINE_INTERVAL_MS {
                    return Err(invalid(
                        "interval.every_ms must be between 1000 and 2592000000",
                    ));
                }
                parse_timezone(timezone).map(|_| ())
            }
            Self::Calendar {
                timezone,
                hour,
                minute,
                weekday,
            } => {
                if *hour > 23 || *minute > 59 {
                    return Err(invalid("calendar hour/minute is out of range"));
                }
                if weekday.is_some_and(|day| day > 6) {
                    return Err(invalid("calendar.weekday must be 0-6 (Monday-Sunday)"));
                }
                parse_timezone(timezone).map(|_| ())
            }
        }
    }

    pub fn initial_next_fire(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, OrchError> {
        match self {
            Self::Manual | Self::External { .. } => Ok(None),
            Self::OneShot { at, expires_at } => {
                if expires_at.is_some_and(|expires| expires < now) {
                    Ok(None)
                } else {
                    Ok(Some(*at))
                }
            }
            Self::Interval { .. } | Self::Calendar { .. } => {
                self.next_after(now - Duration::milliseconds(1))
            }
        }
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, OrchError> {
        match self {
            Self::Manual | Self::External { .. } => Ok(None),
            Self::OneShot { at, expires_at } => {
                if *at > after && !expires_at.is_some_and(|expires| expires < *at) {
                    Ok(Some(*at))
                } else {
                    Ok(None)
                }
            }
            Self::Interval {
                every_ms,
                timezone,
                anchor,
            } => {
                let _tz = parse_timezone(timezone)?;
                let step = Duration::milliseconds(*every_ms as i64);
                if step <= Duration::zero() {
                    return Err(invalid("interval step is invalid"));
                }
                if *anchor > after {
                    return Ok(Some(*anchor));
                }
                let elapsed = after - *anchor;
                let steps = elapsed.num_milliseconds() / step.num_milliseconds() + 1;
                Ok(Some(*anchor + step * steps as i32))
            }
            Self::Calendar {
                timezone,
                hour,
                minute,
                weekday,
            } => next_calendar_after(parse_timezone(timezone)?, *hour, *minute, *weekday, after),
        }
    }

    pub fn occurrences_through(
        &self,
        from_inclusive: DateTime<Utc>,
        through: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<DateTime<Utc>>, OrchError> {
        if through < from_inclusive || limit == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut cursor = from_inclusive;
        if let Some(first) = match self {
            Self::OneShot { at, .. } if *at >= from_inclusive && *at <= through => Some(*at),
            Self::OneShot { .. } => None,
            _ => {
                if self.next_after(from_inclusive - Duration::milliseconds(1))?
                    == Some(from_inclusive)
                {
                    Some(from_inclusive)
                } else {
                    self.next_after(from_inclusive - Duration::milliseconds(1))?
                        .filter(|instant| *instant >= from_inclusive)
                }
            }
        } {
            if first <= through {
                out.push(first);
                cursor = first;
            }
        }
        while out.len() < limit {
            let Some(next) = self.next_after(cursor)? else {
                break;
            };
            if next > through {
                break;
            }
            if next <= cursor {
                break;
            }
            out.push(next);
            cursor = next;
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineConcurrencyPolicy {
    pub max_in_flight: u32,
    pub overlap: OverlapPolicy,
}

impl Default for RoutineConcurrencyPolicy {
    fn default() -> Self {
        Self {
            max_in_flight: DEFAULT_ROUTINE_MAX_IN_FLIGHT,
            overlap: OverlapPolicy::Skip,
        }
    }
}

impl RoutineConcurrencyPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.max_in_flight == 0 || self.max_in_flight > 8 {
            return Err(invalid("concurrency.max_in_flight must be between 1 and 8"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRetryPolicy {
    pub backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub circuit_failures: u32,
}

impl Default for RoutineRetryPolicy {
    fn default() -> Self {
        Self {
            backoff_ms: DEFAULT_BACKOFF_MS,
            max_backoff_ms: MAX_BACKOFF_MS,
            circuit_failures: DEFAULT_CIRCUIT_FAILURES,
        }
    }
}

impl RoutineRetryPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.backoff_ms == 0 || self.backoff_ms > MAX_BACKOFF_MS {
            return Err(invalid("retry.backoff_ms is out of range"));
        }
        if self.max_backoff_ms < self.backoff_ms || self.max_backoff_ms > MAX_BACKOFF_MS {
            return Err(invalid("retry.max_backoff_ms is out of range"));
        }
        if self.circuit_failures == 0 || self.circuit_failures > 32 {
            return Err(invalid("retry.circuit_failures must be between 1 and 32"));
        }
        Ok(())
    }

    pub fn delay_after(&self, consecutive_failures: u32) -> Duration {
        let shift = consecutive_failures.saturating_sub(1).min(10);
        let scaled = self.backoff_ms.saturating_mul(1u64 << shift);
        Duration::milliseconds(scaled.min(self.max_backoff_ms) as i64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkTemplate {
    pub kind: String,
    pub objective: String,
    #[serde(default)]
    pub priority: i32,
    pub policy: WorkPolicy,
}

impl WorkTemplate {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.kind.is_empty() || self.kind.len() > 96 || self.kind.contains('\0') {
            return Err(invalid("work template kind is empty or exceeds its bound"));
        }
        if self.objective.is_empty()
            || self.objective.len() > 32 * 1024
            || self.objective.contains('\0')
        {
            return Err(invalid(
                "work template objective is empty or exceeds its bound",
            ));
        }
        self.policy.validate()
    }
}

/// Frozen policy applied to one activation. Captured so later Agent or
/// routine revisions cannot rewrite history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedActivationPolicy {
    pub routine_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_spec_revision: Option<u64>,
    pub run_bounds: RunBounds,
    pub work_policy: WorkPolicy,
    pub sandbox_profile: String,
    pub computer_use_allowed: bool,
    pub auto_approve_tools: bool,
}

impl CapturedActivationPolicy {
    pub fn capture(
        routine_revision: u64,
        agent_id: Option<&str>,
        spec: Option<&AgentSpec>,
        template: &WorkTemplate,
        server_ceiling: &RunBounds,
    ) -> Result<Self, OrchError> {
        let agent_bounds = spec
            .map(|spec| spec.default_run_bounds.clone())
            .unwrap_or_else(|| server_ceiling.clone());
        let narrowed_ceiling = RunBounds {
            max_prompt_bytes: agent_bounds
                .max_prompt_bytes
                .min(server_ceiling.max_prompt_bytes),
            max_rounds: agent_bounds.max_rounds.min(server_ceiling.max_rounds),
            max_duration_ms: agent_bounds
                .max_duration_ms
                .min(server_ceiling.max_duration_ms),
            max_total_tokens: match (
                agent_bounds.max_total_tokens,
                server_ceiling.max_total_tokens,
            ) {
                (Some(agent), Some(server)) => Some(agent.min(server)),
                (Some(agent), None) => Some(agent),
                (None, server) => server,
            },
        };
        let requested = serde_json::to_value(&template.policy.bounds)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let run_bounds = merge_bounds(&narrowed_ceiling, Some(&requested))?;
        let mut work_policy = template.policy.clone();
        work_policy.bounds = run_bounds.clone();
        work_policy.validate()?;
        let authority = spec.map(|spec| spec.authority.clone()).unwrap_or_default();
        Ok(Self {
            routine_revision,
            agent_id: agent_id.map(str::to_string),
            agent_spec_revision: spec.map(|spec| spec.revision),
            run_bounds,
            work_policy,
            sandbox_profile: authority.sandbox_profile,
            // Activation never grants Computer Use or automatic tool approval.
            computer_use_allowed: false,
            auto_approve_tools: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRecord {
    pub schema_version: u32,
    pub routine_id: String,
    pub name: String,
    pub agent_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub lifecycle: RoutineLifecycle,
    pub trigger: RoutineTrigger,
    pub work_template: WorkTemplate,
    pub missed_run_policy: MissedRunPolicy,
    pub concurrency: RoutineConcurrencyPolicy,
    pub retry: RoutineRetryPolicy,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fire_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub circuit_open: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RoutineRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: Uuid,
        workspace: impl Into<String>,
        trigger: RoutineTrigger,
        work_template: WorkTemplate,
        missed_run_policy: MissedRunPolicy,
        concurrency: RoutineConcurrencyPolicy,
        retry: RoutineRetryPolicy,
        created_by: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let routine = Self {
            schema_version: ROUTINE_SCHEMA_VERSION,
            routine_id: Uuid::new_v4().to_string(),
            name: name.into(),
            agent_id: agent_id.into(),
            session_id,
            workspace: workspace.into(),
            lifecycle: RoutineLifecycle::Enabled,
            trigger,
            work_template,
            missed_run_policy,
            concurrency,
            retry,
            revision: 1,
            next_fire_at: None,
            last_fire_at: None,
            consecutive_failures: 0,
            circuit_open: false,
            created_by: created_by.into(),
            created_at: now,
            updated_at: now,
        };
        let mut routine = routine;
        routine.next_fire_at = routine.trigger.initial_next_fire(now)?;
        routine.validate()?;
        Ok(routine)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != ROUTINE_SCHEMA_VERSION || self.revision == 0 {
            return Err(invalid("routine schema version or revision is invalid"));
        }
        validate_routine_id(&self.routine_id, "routine_id")?;
        validate_bounded(&self.name, MAX_ROUTINE_NAME_BYTES, "name")?;
        validate_routine_id(&self.agent_id, "agent_id")?;
        validate_bounded(&self.workspace, MAX_ROUTINE_ACTOR_BYTES * 4, "workspace")?;
        validate_bounded(&self.created_by, MAX_ROUTINE_ACTOR_BYTES, "created_by")?;
        self.trigger.validate()?;
        self.work_template.validate()?;
        self.concurrency.validate()?;
        self.retry.validate()?;
        Ok(())
    }

    pub fn bump_at(&mut self, now: DateTime<Utc>) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDisposition {
    CreatedWork,
    Deduplicated,
    SkippedPaused,
    SkippedDisabled,
    SkippedOverlap,
    SkippedMissed,
    SkippedExpired,
    Rejected,
    Backoff,
    CircuitOpen,
}

impl ActivationDisposition {
    pub fn created_work(self) -> bool {
        matches!(self, Self::CreatedWork)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationRecord {
    pub schema_version: u32,
    pub activation_id: String,
    pub routine_id: String,
    pub routine_revision: u64,
    pub trigger_kind: String,
    pub dedupe_key: String,
    pub scheduled_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub fired_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    pub disposition: ActivationDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub captured_policy: CapturedActivationPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub created_by: String,
}

impl ActivationRecord {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != ROUTINE_SCHEMA_VERSION {
            return Err(invalid("activation schema version is invalid"));
        }
        validate_routine_id(&self.activation_id, "activation_id")?;
        validate_routine_id(&self.routine_id, "activation.routine_id")?;
        validate_bounded(&self.dedupe_key, 512, "dedupe_key")?;
        validate_bounded(&self.trigger_kind, 32, "trigger_kind")?;
        validate_bounded(&self.created_by, MAX_ROUTINE_ACTOR_BYTES, "created_by")?;
        if let Some(work_id) = &self.work_id {
            validate_routine_id(work_id, "activation.work_id")?;
        }
        if let Some(error) = &self.error {
            validate_bounded(error, 4 * 1024, "activation.error")?;
        }
        if let Some(payload) = &self.payload {
            let encoded = serde_json::to_vec(payload).unwrap_or_default();
            if encoded.len() > MAX_ROUTINE_PAYLOAD_BYTES {
                return Err(invalid("activation payload exceeds its bound"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineSnapshot {
    pub routine: RoutineRecord,
    pub activations: Vec<ActivationRecord>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineFireReport {
    pub scanned_routines: usize,
    pub created_work: usize,
    pub skipped: usize,
    pub rejected: usize,
    pub deduplicated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    pub cause: ActivationCause,
    pub dedupe_key: String,
    pub scheduled_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub payload: Option<serde_json::Value>,
    pub created_by: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationCause {
    Manual,
    Scheduled,
    External,
}

impl ActivationCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::External => "external",
        }
    }
}

pub fn parse_timezone(name: &str) -> Result<Tz, OrchError> {
    validate_bounded(name, MAX_ROUTINE_TIMEZONE_BYTES, "timezone")?;
    name.parse::<Tz>()
        .map_err(|_| invalid(format!("unknown timezone {name}")))
}

pub fn occurrence_dedupe_key(routine_id: &str, scheduled_at: DateTime<Utc>) -> String {
    format!(
        "sched:{}:{}",
        routine_id,
        scheduled_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    )
}

pub fn manual_dedupe_key(routine_id: &str, request_id: &str) -> String {
    format!("manual:{routine_id}:{request_id}")
}

pub fn redact_activation_payload(value: &serde_json::Value) -> serde_json::Value {
    redact_value(value)
}

pub fn validate_activation_payload(
    payload: Option<&serde_json::Value>,
) -> Result<Option<serde_json::Value>, OrchError> {
    let Some(payload) = payload else {
        return Ok(None);
    };
    let encoded = serde_json::to_vec(payload)
        .map_err(|error| OrchError::new(OrchErrorCode::InvalidRequest, error.to_string()))?;
    if encoded.len() > MAX_ROUTINE_PAYLOAD_BYTES {
        return Err(invalid("activation payload exceeds 8192 bytes"));
    }
    Ok(Some(redact_activation_payload(payload)))
}

pub fn due_occurrences(
    routine: &RoutineRecord,
    now: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, OrchError> {
    let Some(due) = routine.next_fire_at else {
        return Ok(Vec::new());
    };
    if due > now {
        return Ok(Vec::new());
    }
    if let RoutineTrigger::OneShot { expires_at, .. } = routine.trigger {
        if expires_at.is_some_and(|expires| expires < now) {
            return Ok(Vec::new());
        }
        return Ok(vec![due]);
    }
    let missed = routine.trigger.occurrences_through(
        due,
        now,
        MAX_CATCH_UP_OCCURRENCES.saturating_add(1),
    )?;
    if missed.is_empty() {
        return Ok(vec![due]);
    }
    match routine.missed_run_policy {
        MissedRunPolicy::Skip => {
            if missed.len() == 1 {
                Ok(missed)
            } else {
                Ok(Vec::new())
            }
        }
        MissedRunPolicy::Coalesce => Ok(missed.last().copied().into_iter().collect()),
        MissedRunPolicy::CatchUp => {
            let start = missed.len().saturating_sub(MAX_CATCH_UP_OCCURRENCES);
            Ok(missed[start..].to_vec())
        }
    }
}

pub fn in_flight_count(items: &[WorkItem], routine_id: &str) -> usize {
    items
        .iter()
        .filter(|item| {
            item.source_routine_id.as_deref() == Some(routine_id) && !item.state.is_terminal()
        })
        .count()
}

pub fn decide_lifecycle_skip(
    routine: &RoutineRecord,
    cause: ActivationCause,
) -> Option<ActivationDisposition> {
    if routine.circuit_open {
        return Some(ActivationDisposition::CircuitOpen);
    }
    match (routine.lifecycle, cause) {
        (RoutineLifecycle::Disabled, _) => Some(ActivationDisposition::SkippedDisabled),
        (RoutineLifecycle::Paused, ActivationCause::Scheduled) => {
            Some(ActivationDisposition::SkippedPaused)
        }
        (RoutineLifecycle::Paused, ActivationCause::External) => {
            Some(ActivationDisposition::SkippedPaused)
        }
        (RoutineLifecycle::Paused, ActivationCause::Manual) => None,
        (RoutineLifecycle::Enabled, _) => None,
    }
}

fn next_calendar_after(
    tz: Tz,
    hour: u8,
    minute: u8,
    weekday: Option<u8>,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, OrchError> {
    let local_after = after.with_timezone(&tz);
    let start_date = local_after.date_naive();
    for offset in 0i64..400 {
        let Some(date) = start_date.checked_add_signed(Duration::days(offset)) else {
            break;
        };
        if let Some(wanted) = weekday {
            if weekday_monday0(date.weekday()) != wanted {
                continue;
            }
        }
        let Some(local) = resolve_local_wall(tz, date, hour, minute) else {
            continue;
        };
        let utc = local.with_timezone(&Utc);
        if utc > after {
            return Ok(Some(utc));
        }
    }
    Err(invalid("could not compute the next calendar fire time"))
}

fn weekday_monday0(weekday: Weekday) -> u8 {
    weekday.num_days_from_monday() as u8
}

/// Resolve a local wall time.
///
/// Spring-forward gaps (`02:30` on a US DST start) are skipped. Fall-back
/// overlaps fire once at the earlier offset.
fn resolve_local_wall(tz: Tz, date: NaiveDate, hour: u8, minute: u8) -> Option<DateTime<Tz>> {
    let time = NaiveTime::from_hms_opt(hour as u32, minute as u32, 0)?;
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(when) => Some(when),
        chrono::LocalResult::None => None,
        chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest),
    }
}

fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                if is_secret_key(key) {
                    out.insert(key.clone(), serde_json::Value::String("[redacted]".into()));
                } else {
                    out.insert(key.clone(), redact_value(child));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_value).collect())
        }
        other => other.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "access_token"
            | "accesstoken"
            | "refresh_token"
            | "password"
            | "secret"
            | "authorization"
            | "api_key"
            | "apikey"
            | "bearer"
            | "cookie"
            | "set-cookie"
    )
}

fn validate_routine_id(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty()
        || value.len() > 256
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(invalid(format!("{field} is empty or invalid")));
    }
    Ok(())
}

fn validate_bounded(value: &str, max_bytes: usize, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(invalid(format!("{field} is empty or exceeds its bound")));
    }
    Ok(())
}

/// Decide the next durable next-fire after handling `fired` occurrences at `now`.
pub fn advance_next_fire(
    trigger: &RoutineTrigger,
    now: DateTime<Utc>,
    fired: &[DateTime<Utc>],
) -> Result<Option<DateTime<Utc>>, OrchError> {
    let after = fired.iter().copied().max().unwrap_or(now);
    trigger.next_after(after.max(now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        AgentAuthorityPolicy, DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
    };
    use chrono::{Datelike, Timelike};

    fn daily(tz: &str, hour: u8, minute: u8) -> RoutineTrigger {
        RoutineTrigger::Calendar {
            timezone: tz.into(),
            hour,
            minute,
            weekday: None,
        }
    }

    #[test]
    fn spring_forward_skips_missing_local_time() {
        // 2026-03-08 02:00 EST -> 03:00 EDT in America/New_York.
        let trigger = daily("America/New_York", 2, 30);
        let before = Utc.with_ymd_and_hms(2026, 3, 8, 6, 0, 0).unwrap(); // 01:00 EST
        let next = trigger.next_after(before).unwrap().unwrap();
        let local = next.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(local.month(), 3);
        assert_eq!(local.day(), 9);
        assert_eq!(local.hour(), 2);
        assert_eq!(local.minute(), 30);
    }

    #[test]
    fn fall_back_fires_once_at_first_offset() {
        let trigger = daily("America/New_York", 1, 30);
        // 2026-11-01 00:30 EDT.
        let before = Utc.with_ymd_and_hms(2026, 11, 1, 4, 30, 0).unwrap();
        let first = trigger.next_after(before).unwrap().unwrap();
        let local = first.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(local.day(), 1);
        assert_eq!(local.hour(), 1);
        assert_eq!(local.minute(), 30);
        let second = trigger.next_after(first).unwrap().unwrap();
        let second_local = second.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(second_local.day(), 2);
        assert_eq!(second_local.hour(), 1);
        assert_eq!(second_local.minute(), 30);
        // The repeated local hour makes the UTC gap 25 hours.
        assert_eq!(second.signed_duration_since(first), Duration::hours(25));
    }

    #[test]
    fn missed_skip_drops_stale_slots() {
        let trigger = RoutineTrigger::Interval {
            every_ms: 60_000,
            timezone: "UTC".into(),
            anchor: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        let routine = RoutineRecord {
            schema_version: ROUTINE_SCHEMA_VERSION,
            routine_id: "routine-1".into(),
            name: "tick".into(),
            agent_id: "agent-1".into(),
            session_id: Uuid::nil(),
            workspace: "/tmp/ws".into(),
            lifecycle: RoutineLifecycle::Enabled,
            trigger,
            work_template: WorkTemplate {
                kind: "routine".into(),
                objective: "tick".into(),
                priority: 0,
                policy: WorkPolicy::default(),
            },
            missed_run_policy: MissedRunPolicy::Skip,
            concurrency: RoutineConcurrencyPolicy::default(),
            retry: RoutineRetryPolicy::default(),
            revision: 1,
            next_fire_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
            last_fire_at: None,
            consecutive_failures: 0,
            circuit_open: false,
            created_by: "test".into(),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 5, 0).unwrap();
        assert!(due_occurrences(&routine, now).unwrap().is_empty());
        let mut catch_up = routine.clone();
        catch_up.missed_run_policy = MissedRunPolicy::CatchUp;
        assert_eq!(due_occurrences(&catch_up, now).unwrap().len(), 6);
        let mut coalesce = routine;
        coalesce.missed_run_policy = MissedRunPolicy::Coalesce;
        assert_eq!(due_occurrences(&coalesce, now).unwrap().len(), 1);
    }

    #[test]
    fn captured_policy_never_widens_or_grants_computer_use() {
        let spec = AgentSpec::initial(
            "agent-1",
            "/tmp/ws",
            "grok-4",
            AgentAuthorityPolicy {
                computer_use_allowed: true,
                auto_allowed_tools: vec!["run_terminal_cmd".into()],
                ..AgentAuthorityPolicy::default()
            },
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            "test",
        )
        .unwrap();
        let mut template = WorkTemplate {
            kind: "routine".into(),
            objective: "do work".into(),
            priority: 0,
            policy: WorkPolicy::default(),
        };
        template.policy.bounds.max_rounds = 2;
        let captured = CapturedActivationPolicy::capture(
            3,
            Some("agent-1"),
            Some(&spec),
            &template,
            &RunBounds {
                max_total_tokens: Some(DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS),
                ..RunBounds::default()
            },
        )
        .unwrap();
        assert!(!captured.computer_use_allowed);
        assert!(!captured.auto_approve_tools);
        assert_eq!(captured.run_bounds.max_rounds, 2);
        assert!(captured.work_policy.bounds.max_rounds <= spec.default_run_bounds.max_rounds);
    }

    #[test]
    fn payload_redaction_strips_secret_keys() {
        let redacted = redact_activation_payload(&serde_json::json!({
            "authorization": "Bearer secret-token",
            "nested": { "apiKey": "abc", "ok": true }
        }));
        assert_eq!(redacted["authorization"], "[redacted]");
        assert_eq!(redacted["nested"]["apiKey"], "[redacted]");
        assert_eq!(redacted["nested"]["ok"], true);
    }

    #[test]
    fn fake_clock_is_deterministic() {
        let clock = FakeClock::new(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap());
        clock.advance(Duration::minutes(15));
        assert_eq!(
            clock.now(),
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 15, 0).unwrap()
        );
    }
}
