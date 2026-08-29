use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{validate_id, SCHEMA_VERSION};
use crate::manifest::{
    ComputerSurfaceBinding, HelperIdentity, IsolatedSourceManifest, IsolatedVisualManifest,
    IsolatedVisualResourceLimits,
};

/// Live guest phases. Terminal truth is a separate field so `failed`,
/// `interrupted`, and `quarantined` cannot be confused with a resumable phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedGuestPhase {
    Create,
    Ready,
    Running,
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedGuestTerminal {
    Failed,
    Interrupted,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedEvidenceClass {
    /// Deterministic simulator. Never VM qualification.
    SimulatorIneligible,
    /// Source compilation or fixture materialization. Never VM qualification.
    SourceCompilationIneligible,
    /// Real Virtualization.framework boot/frame/input/cleanup.
    VirtualizationFramework,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedGuestRecord {
    pub schema_version: u32,
    pub guest_id: String,
    pub run_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub helper: HelperIdentity,
    pub surface: ComputerSurfaceBinding,
    pub input_domain_id: String,
    pub conflict_domain_id: String,
    pub source: IsolatedSourceManifest,
    pub packaged_manifest: Option<IsolatedVisualManifest>,
    pub phase: IsolatedGuestPhase,
    pub terminal: Option<IsolatedGuestTerminal>,
    pub cleaned: bool,
    #[serde(default)]
    pub occupancy_resource_key: String,
    pub evidence_class: IsolatedEvidenceClass,
    pub limits: IsolatedVisualResourceLimits,
    pub frame_epoch: u64,
    pub frames_seen: u32,
    pub input_events_seen: u32,
    pub resident_frame_bytes: u64,
    pub captured_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub disposition: Option<String>,
}

impl IsolatedGuestRecord {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::internal(
                "isolated guest record schema is unsupported",
            ));
        }
        validate_id("guest_id", &self.guest_id)?;
        validate_id("run_id", &self.run_id)?;
        validate_id("work_id", &self.work_id)?;
        validate_id("work_attempt_id", &self.work_attempt_id)?;
        validate_id("agent_id", &self.agent_id)?;
        validate_id("input_domain_id", &self.input_domain_id)?;
        validate_id("conflict_domain_id", &self.conflict_domain_id)?;
        self.helper.validate()?;
        self.surface.validate()?;
        self.source.validate()?;
        self.limits.validate()?;
        if !self.occupancy_resource_key.is_empty() {
            validate_id("occupancy_resource_key", &self.occupancy_resource_key)?;
        }
        if self.agent_spec_revision == 0 {
            return Err(IsolatedError::invalid(
                "agent_spec_revision must be greater than zero",
            ));
        }
        if self.cleaned && self.terminal.is_none() && self.phase != IsolatedGuestPhase::Closing {
            return Err(IsolatedError::internal(
                "cleaned guest is not in a terminal closing state",
            ));
        }
        if self.phase != IsolatedGuestPhase::Closing && self.cleaned {
            return Err(IsolatedError::internal("cleaned flag requires closing"));
        }
        Ok(())
    }

    pub fn is_live(&self) -> bool {
        self.terminal.is_none() && !self.cleaned
    }

    pub fn transition(
        &mut self,
        next: IsolatedGuestPhase,
        now: DateTime<Utc>,
    ) -> IsolatedResult<()> {
        if self.terminal.is_some() {
            return Err(IsolatedError::invalid_state(
                "terminal guest cannot change phase",
            ));
        }
        if now < self.updated_at {
            return Err(IsolatedError::conflict("guest clock moved backwards"));
        }
        let legal = matches!(
            (self.phase, next),
            (IsolatedGuestPhase::Create, IsolatedGuestPhase::Ready)
                | (IsolatedGuestPhase::Ready, IsolatedGuestPhase::Running)
                | (IsolatedGuestPhase::Create, IsolatedGuestPhase::Closing)
                | (IsolatedGuestPhase::Ready, IsolatedGuestPhase::Closing)
                | (IsolatedGuestPhase::Running, IsolatedGuestPhase::Closing)
        );
        if !legal {
            return Err(IsolatedError::invalid_state(
                "invalid isolated guest phase transition",
            ));
        }
        if next == IsolatedGuestPhase::Running {
            self.started_at = Some(now);
        }
        self.phase = next;
        self.updated_at = now;
        Ok(())
    }

    pub fn terminate(
        &mut self,
        terminal: IsolatedGuestTerminal,
        now: DateTime<Utc>,
        disposition: &str,
    ) -> IsolatedResult<()> {
        if now < self.updated_at {
            return Err(IsolatedError::conflict("guest clock moved backwards"));
        }
        if self.terminal.is_some() {
            return Err(IsolatedError::invalid_state("guest is already terminal"));
        }
        self.phase = IsolatedGuestPhase::Closing;
        self.terminal = Some(terminal);
        self.ended_at = Some(now);
        self.updated_at = now;
        self.disposition = Some(disposition.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{
        isolated_conflict_domain_id, isolated_input_domain_id, ISOLATED_VISUAL_BACKEND_ID,
    };
    use crate::manifest::{
        IsolatedSourceEntry, IsolatedVisualResourceLimits, SourceObject, SourceObjectKind,
    };

    fn guest() -> IsolatedGuestRecord {
        let now = Utc::now();
        IsolatedGuestRecord {
            schema_version: 1,
            guest_id: "guest-1".into(),
            run_id: "run-1".into(),
            work_id: "work-1".into(),
            work_attempt_id: "attempt-1".into(),
            agent_id: "agent-1".into(),
            agent_spec_revision: 1,
            helper: HelperIdentity {
                helper_id: "helper-1".into(),
                content_sha256: "a".repeat(64),
                signing_requirement_sha256: "b".repeat(64),
            },
            surface: ComputerSurfaceBinding::issue(),
            input_domain_id: isolated_input_domain_id("guest-1"),
            conflict_domain_id: isolated_conflict_domain_id("guest-1"),
            source: IsolatedSourceManifest {
                schema_version: 1,
                backend_id: ISOLATED_VISUAL_BACKEND_ID.into(),
                guest_protocol_version: 1,
                objects: vec![IsolatedSourceEntry {
                    relative_path: "guest-init.c".into(),
                    object: SourceObject {
                        digest_sha256: "c".repeat(64),
                        kind: SourceObjectKind::Blob,
                        media_type: "text/x-c".into(),
                        byte_len: 16,
                    },
                }],
                helper_content_sha256: "a".repeat(64),
                helper_signing_requirement_sha256: "b".repeat(64),
                guest_image_sha256: None,
                configuration_sha256: "d".repeat(64),
            },
            packaged_manifest: None,
            phase: IsolatedGuestPhase::Create,
            terminal: None,
            cleaned: false,
            occupancy_resource_key: String::new(),
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            limits: IsolatedVisualResourceLimits::proof_defaults(),
            frame_epoch: 0,
            frames_seen: 0,
            input_events_seen: 0,
            resident_frame_bytes: 0,
            captured_bytes: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            ended_at: None,
            disposition: None,
        }
    }

    #[test]
    fn phases_are_forward_only_and_terminal_blocks_resume() {
        let mut guest = guest();
        let now = guest.updated_at;
        guest
            .transition(
                IsolatedGuestPhase::Ready,
                now + chrono::Duration::seconds(1),
            )
            .unwrap();
        guest
            .transition(
                IsolatedGuestPhase::Running,
                now + chrono::Duration::seconds(2),
            )
            .unwrap();
        guest
            .terminate(
                IsolatedGuestTerminal::Interrupted,
                now + chrono::Duration::seconds(3),
                "restart",
            )
            .unwrap();
        assert!(guest
            .transition(
                IsolatedGuestPhase::Running,
                now + chrono::Duration::seconds(4)
            )
            .is_err());
        assert!(!guest.is_live());
    }
}
