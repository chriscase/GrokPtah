//! Deterministic fixtures shared by unit and contract tests.
//!
//! Everything here is offline and synthetic: no provider credential, no
//! network, no real workspace. The paths are built from the platform temporary
//! directory so the fixtures stay absolute on every supported platform without
//! creating anything on disk.

use std::path::{Path, PathBuf};

use grokptah_agent_sdk::run::ExecutionMode;
use grokptah_agent_sdk::{
    CONTRACT_VERSION, CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier,
};

use crate::authority::ResolvedBounds;
use crate::authority::{CAP_EXECUTE, CAP_OBSERVE, CAP_PROMOTE, CAP_QUEUE, CAP_RESUME, CAP_REVIEW};
use crate::config::{EngineSelection, HostConfig, HostLimits};
use crate::store::{RunPhase, RunRecord};

/// Fixed timestamp used by record fixtures.
pub const TS: &str = "2026-01-01T00:00:00.000Z";
/// Fixed epoch millisecond used by clock fixtures.
pub const NOW_MS: u64 = 1_767_225_600_000;

fn descriptor(
    id: &str,
    tier: CapabilityTier,
    mutating: bool,
    human_gate: bool,
    availability: CapabilityAvailability,
) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.to_owned(),
        tier,
        mutating,
        human_gate,
        availability,
        description: format!("fixture capability {id}"),
    }
}

/// A capability set covering every availability the host must handle:
/// available, gated, and unavailable.
pub fn capability_fixture() -> CapabilitySet {
    CapabilitySet {
        contract: CONTRACT_VERSION.to_owned(),
        capabilities: vec![
            descriptor(
                CAP_OBSERVE,
                CapabilityTier::Observe,
                false,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_EXECUTE,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_QUEUE,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_REVIEW,
                CapabilityTier::Review,
                false,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_PROMOTE,
                CapabilityTier::Promote,
                true,
                true,
                CapabilityAvailability::Gated,
            ),
            descriptor(
                CAP_RESUME,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Unavailable,
            ),
        ],
    }
}

/// Root under which fixture paths are built. Nothing is created here.
pub fn fixture_root() -> PathBuf {
    std::env::temp_dir().join("grokptah-headless-fixture")
}

/// A validated configuration over synthetic sibling roots.
pub fn config_fixture() -> HostConfig {
    let root = fixture_root();
    config_for(&root.join("host-home"), &root.join("project"))
}

/// A validated configuration over explicit roots.
pub fn config_for(home: &Path, workspace: &Path) -> HostConfig {
    HostConfig {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        session_id: "session-fixture".to_owned(),
        capabilities: capability_fixture(),
        grants: Vec::new(),
        limits: HostLimits {
            max_active_runs: 1,
            max_queued_runs: 2,
            max_prompt_bytes: 4_096,
            max_rounds: 4,
            max_duration_ms: 60_000,
            event_retention: 32,
            max_event_bytes: 8_192,
            lease_ttl_ms: 1_000,
            attention_ttl_ms: 5_000,
        },
        engine: EngineSelection::Disabled,
    }
}

/// A durable run record fixture in an exact phase.
pub fn run_record_fixture(run_id: &str, phase: RunPhase) -> RunRecord {
    RunRecord {
        run_id: run_id.to_owned(),
        session_id: "session-fixture".to_owned(),
        workspace: "project".to_owned(),
        request_id: format!("req-{run_id}"),
        phase,
        prompt_preview: "build".to_owned(),
        request_fingerprint: "fingerprint-request".to_owned(),
        created_at: TS.to_owned(),
        updated_at: TS.to_owned(),
        revision: 1,
        rounds_used: 0,
        bounds: ResolvedBounds {
            max_prompt_bytes: 4_096,
            max_rounds: 4,
            max_duration_ms: 60_000,
        },
        execution_mode: ExecutionMode::IsolatedWorktree,
        started_at_ms: None,
        pending_steering: Vec::new(),
        attention: None,
        stop_reason: None,
        completion: None,
    }
}

/// A fixture script exercising progress, completion, escalation, and failure.
pub const FIXTURE_SCRIPT: &str = r#"{
  "prompts": {
    "build": [
      {"kind": "progress", "update": {"note": "planning"}},
      {"kind": "completed",
       "changedFiles": [{"path": "src/lib.rs", "summary": "add guard"}],
       "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n",
       "fingerprint": "fingerprint-build"}
    ],
    "escalate": [
      {"kind": "needsAttention", "attention": "permission_required",
       "reasonCode": "shell_write_requested", "detail": "engine asked to write outside the run"}
    ],
    "fail": [
      {"kind": "failed", "reasonCode": "engine_refused", "detail": "no route"}
    ],
    "forever": [
      {"kind": "progress", "update": {"note": "still working"}}
    ],
    "noop": [
      {"kind": "completed", "changedFiles": [], "diff": "", "fingerprint": "fingerprint-noop"}
    ],
    "leak": [
      {"kind": "failed", "reasonCode": "engine_leak",
       "detail": "retry with XAI_API_KEY=xai-abcdefghijklmnopqrstuvwxyz012345"}
    ],
    "escape": [
      {"kind": "completed", "changedFiles": [{"path": "/etc/shadow", "summary": "x"}],
       "diff": "", "fingerprint": "fingerprint-escape"}
    ]
  },
  "default": [
    {"kind": "failed", "reasonCode": "unscripted", "detail": "no scripted outcome"}
  ]
}"#;
