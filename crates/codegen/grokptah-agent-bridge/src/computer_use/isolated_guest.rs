//! Explicit isolated-guest phases, one-agent lease ownership, and capture
//! redaction.
//!
//! This module does **not** replace [`IsolatedVisualLifecycle`]. Those states
//! remain the host/helper contract (`Prepared` … `Terminated`). The five guest
//! phases below are a closed projection used by simulator proofs:
//! create, ready, running, closing, failed.
//!
//! It also does not replace durable [`super::store`] surface leases. It is the
//! guest-bootstrap ownership fence: one Agent may control one guest, a stale
//! lease cannot drive the guest, and cancel/crash still require exact cleanup.

use serde_json::Value;

use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualLifecycle,
    IsolatedVisualLifecycleState, IsolatedVisualTerminalDisposition,
};
use super::isolated_visual_frames::IsolatedVisualFrame;
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, IsolatedVisualHelperEventCode, IsolatedVisualHelperSupervisorState,
    ISOLATED_VISUAL_HELPER_EVENT_BYTES, ISOLATED_VISUAL_HELPER_EVENT_MAGIC,
    ISOLATED_VISUAL_HELPER_EVENT_VERSION,
};
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

const FORBIDDEN_CAPTURE_KEYS: &[&str] = &[
    "apiKey",
    "api_key",
    "authorization",
    "baseUrl",
    "base_url",
    "bearer",
    "channelSecret",
    "channel_secret",
    "clipboard",
    "clipboardContents",
    "clipboard_contents",
    "credential",
    "credentials",
    "helperPath",
    "helper_path",
    "hostClipboard",
    "hostHome",
    "host_home",
    "ipAddress",
    "ip_address",
    "ipv4",
    "ipv6",
    "macAddress",
    "mac_address",
    "networkDevices",
    "networkInterface",
    "network_interface",
    "overlayPath",
    "overlay_path",
    "password",
    "sharedDirectory",
    "shared_directory",
    "ssid",
    "token",
];

/// Closed guest-facing phases. Names are the bootstrap contract; they never
/// reopen [`IsolatedVisualLifecycle`] or skip cleanup evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolatedGuestPhase {
    Create,
    Ready,
    Running,
    Closing,
    Failed,
}

impl IsolatedGuestPhase {
    pub fn from_runtime(
        lifecycle: &IsolatedVisualLifecycle,
        helper: IsolatedVisualHelperSupervisorState,
    ) -> Self {
        if matches!(helper, IsolatedVisualHelperSupervisorState::Failed(_)) {
            return Self::Failed;
        }
        if matches!(
            lifecycle.terminal_disposition,
            Some(IsolatedVisualTerminalDisposition::Failed)
                | Some(IsolatedVisualTerminalDisposition::Interrupted)
        ) {
            return Self::Failed;
        }
        match lifecycle.state() {
            IsolatedVisualLifecycleState::Prepared | IsolatedVisualLifecycleState::Starting => {
                Self::Create
            }
            IsolatedVisualLifecycleState::ReadOnlyReady => {
                if helper == IsolatedVisualHelperSupervisorState::Bound {
                    Self::Running
                } else {
                    Self::Ready
                }
            }
            IsolatedVisualLifecycleState::Stopping
            | IsolatedVisualLifecycleState::CleanupPending
            | IsolatedVisualLifecycleState::Terminated => Self::Closing,
        }
    }
}

/// Path-free, secret-free capture envelope. Frame bytes stay off this type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedCapturedArtifact {
    pub frame_sequence: u64,
    pub width: u32,
    pub height: u32,
    pub content_sha256: String,
}

pub fn project_captured_artifact(frame: &IsolatedVisualFrame) -> IsolatedCapturedArtifact {
    IsolatedCapturedArtifact {
        frame_sequence: frame.frame_sequence,
        width: frame.width,
        height: frame.height,
        content_sha256: hex_digest(&frame.content_sha256),
    }
}

/// Strip sensitive keys from a captured metadata object. Leftover forbidden
/// keys or host-path/clipboard/credential/network needles fail closed.
pub fn redact_isolated_capture(value: &Value) -> ComputerResult<Value> {
    let mut redacted = value.clone();
    strip_forbidden_keys(&mut redacted);
    if contains_forbidden_key(&redacted) || contains_sensitive_needle(&redacted) {
        return Err(ComputerError::new(
            ComputerErrorCode::ForbiddenAction,
            "isolated capture still contains a forbidden path, clipboard, credential, or network field",
        ));
    }
    Ok(redacted)
}

/// One Agent's exact lease on one isolated guest. Revision is monotonic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedGuestLease {
    pub lease_id: String,
    pub agent_id: String,
    pub revision: u64,
}

/// Simulator-owned guest bootstrap session. Not a VM and not a durable store.
pub struct IsolatedGuestSession {
    runtime: IsolatedVisualRuntimeSession,
    lease: Option<IsolatedGuestLease>,
}

impl IsolatedGuestSession {
    pub fn create(
        contract: IsolatedVisualLaunchContract,
        challenge: [u8; 32],
    ) -> ComputerResult<Self> {
        Ok(Self {
            runtime: IsolatedVisualRuntimeSession::new(contract, challenge)?,
            lease: None,
        })
    }

    pub fn phase(&self) -> IsolatedGuestPhase {
        IsolatedGuestPhase::from_runtime(self.runtime.lifecycle(), self.runtime.helper_state())
    }

    pub fn lease(&self) -> Option<&IsolatedGuestLease> {
        self.lease.as_ref()
    }

    /// One Agent per guest. A second Agent is denied while a lease is live.
    pub fn acquire(&mut self, agent_id: impl Into<String>) -> ComputerResult<IsolatedGuestLease> {
        let agent_id = agent_id.into();
        if agent_id.is_empty() || agent_id.len() > 256 || agent_id.contains('\0') {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated guest agent_id is invalid",
            ));
        }
        if !matches!(
            self.phase(),
            IsolatedGuestPhase::Create | IsolatedGuestPhase::Ready | IsolatedGuestPhase::Running
        ) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest is not acquirable after close or failure",
            ));
        }
        if let Some(existing) = &self.lease {
            if existing.agent_id != agent_id {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated guest is already leased to another agent",
                ));
            }
            return Ok(existing.clone());
        }
        let lease = IsolatedGuestLease {
            lease_id: uuid::Uuid::new_v4().to_string(),
            agent_id,
            revision: 1,
        };
        self.lease = Some(lease.clone());
        Ok(lease)
    }

    pub fn drive_to_ready(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
    ) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        if self.phase() != IsolatedGuestPhase::Create {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest is not in create",
            ));
        }
        self.runtime.accept_helper_event(decode_helper_event(
            IsolatedVisualHelperEventCode::Prepared,
            0,
        )?)?;
        self.runtime.start_control()?;
        self.runtime.accept_helper_event(decode_helper_event(
            IsolatedVisualHelperEventCode::Running,
            0,
        )?)?;
        if self.phase() != IsolatedGuestPhase::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest did not become ready",
            ));
        }
        Ok(())
    }

    pub fn drive_to_running(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
    ) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        if self.phase() != IsolatedGuestPhase::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest is not ready",
            ));
        }
        self.runtime.bind_control()?;
        self.runtime.accept_helper_event(decode_helper_event(
            IsolatedVisualHelperEventCode::Bound,
            0,
        )?)?;
        if self.phase() != IsolatedGuestPhase::Running {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest did not become running",
            ));
        }
        Ok(())
    }

    /// Control is denied without a live matching lease, even if the helper is
    /// bound. A stale revision is rejected without mutating the guest.
    pub fn control(&self, agent_id: &str, lease: &IsolatedGuestLease) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        if self.phase() != IsolatedGuestPhase::Running {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest control requires a running leased guest",
            ));
        }
        Ok(())
    }

    pub fn cancel(&mut self, agent_id: &str, lease: &IsolatedGuestLease) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        match self.phase() {
            IsolatedGuestPhase::Create | IsolatedGuestPhase::Ready => {
                self.runtime.accept_helper_event(decode_helper_event(
                    IsolatedVisualHelperEventCode::Failure,
                    HELPER_FAILURE_CONTROL_LOST,
                )?)?;
            }
            IsolatedGuestPhase::Running => {
                self.runtime
                    .stop_control(IsolatedVisualTerminalDisposition::Cancelled)?;
                self.runtime.accept_helper_event(decode_helper_event(
                    IsolatedVisualHelperEventCode::Stopped,
                    0,
                )?)?;
            }
            IsolatedGuestPhase::Closing | IsolatedGuestPhase::Failed => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "isolated guest is already closing or failed",
                ));
            }
        }
        self.revoke_lease();
        Ok(())
    }

    pub fn fail_guest(&mut self, agent_id: &str, lease: &IsolatedGuestLease) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        self.runtime.accept_helper_event(decode_helper_event(
            IsolatedVisualHelperEventCode::Failure,
            HELPER_FAILURE_CONTROL_LOST,
        )?)?;
        self.revoke_lease();
        if self.phase() != IsolatedGuestPhase::Failed {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest did not enter failed",
            ));
        }
        Ok(())
    }

    pub fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        if self.lease.is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated guest cleanup requires the lease to be revoked",
            ));
        }
        self.runtime.complete_cleanup(evidence)?;
        Ok(())
    }

    fn require_lease(&self, agent_id: &str, presented: &IsolatedGuestLease) -> ComputerResult<()> {
        let Some(live) = &self.lease else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated guest control requires a valid lease",
            ));
        };
        if live.agent_id != agent_id {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "isolated guest is leased to another agent",
            ));
        }
        if live.lease_id != presented.lease_id || live.revision != presented.revision {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated guest lease is stale",
            ));
        }
        Ok(())
    }

    fn revoke_lease(&mut self) {
        self.lease = None;
    }
}

const HELPER_FAILURE_CONTROL_LOST: u32 = 8;

fn decode_helper_event(
    code: IsolatedVisualHelperEventCode,
    detail: u32,
) -> ComputerResult<IsolatedVisualHelperEvent> {
    IsolatedVisualHelperEvent::decode(&helper_event_bytes(code, detail))
}

fn helper_event_bytes(code: IsolatedVisualHelperEventCode, detail: u32) -> [u8; 16] {
    let mut bytes = [0u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
    bytes[0..4].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_MAGIC.to_be_bytes());
    bytes[4..6].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_VERSION.to_be_bytes());
    bytes[6..8].copy_from_slice(&(code as u16).to_be_bytes());
    bytes[8..12].copy_from_slice(&detail.to_be_bytes());
    bytes
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn strip_forbidden_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !is_forbidden_key(key));
            for child in map.values_mut() {
                strip_forbidden_keys(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_forbidden_keys(child);
            }
        }
        _ => {}
    }
}

fn is_forbidden_key(key: &str) -> bool {
    FORBIDDEN_CAPTURE_KEYS
        .iter()
        .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
}

fn contains_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| is_forbidden_key(key)) || map.values().any(contains_forbidden_key)
        }
        Value::Array(values) => values.iter().any(contains_forbidden_key),
        _ => false,
    }
}

fn contains_sensitive_needle(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().any(contains_sensitive_needle),
        Value::Array(values) => values.iter().any(contains_sensitive_needle),
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("/users/")
                || lower.contains("/private/")
                || lower.contains("/home/")
                || lower.contains("clipboard:")
                || lower.contains("password=")
                || lower.contains("token=")
                || lower.contains("api_key=")
                || lower.contains("ssid=")
                || lower.starts_with("http://")
                || lower.starts_with("https://")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual::{
        IsolatedVisualManifest, IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
        MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
    };
    use super::super::isolated_visual_frames::IsolatedVisualFrameCarrier;
    use super::super::types::ComputerSurfaceBinding;
    use super::*;
    use serde_json::json;

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: "run-guest-bootstrap".into(),
            surface: ComputerSurfaceBinding {
                surface_id: "surface-guest".into(),
                incarnation: "incarnation-guest".into(),
            },
            input_domain_id: "input-guest".into(),
            manifest: IsolatedVisualManifest {
                schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
                backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
                guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
                helper_content_sha256: "a".repeat(64),
                helper_signing_requirement_sha256: "b".repeat(64),
                guest_image_sha256: "c".repeat(64),
                configuration_sha256: "d".repeat(64),
                security_profile: IsolatedVisualSecurityProfile::locked_down(),
                limits: IsolatedVisualResourceLimits::proof_defaults(),
            },
        }
    }

    fn cleanup(session: &IsolatedGuestSession) -> IsolatedVisualCleanupEvidence {
        IsolatedVisualCleanupEvidence::verified(
            session.runtime.lifecycle().contract.surface.clone(),
            true,
            true,
            true,
            true,
        )
        .unwrap()
    }

    fn running_guest() -> (IsolatedGuestSession, IsolatedGuestLease) {
        let mut guest = IsolatedGuestSession::create(contract(), [9; 32]).unwrap();
        let lease = guest.acquire("agent-a").unwrap();
        guest.drive_to_ready("agent-a", &lease).unwrap();
        guest.drive_to_running("agent-a", &lease).unwrap();
        (guest, lease)
    }

    #[test]
    fn phases_are_create_ready_running_closing_failed() {
        let mut guest = IsolatedGuestSession::create(contract(), [9; 32]).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Create);
        let lease = guest.acquire("agent-a").unwrap();
        guest.drive_to_ready("agent-a", &lease).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Ready);
        guest.drive_to_running("agent-a", &lease).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Running);
        guest.cancel("agent-a", &lease).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Closing);
        guest.complete_cleanup(&cleanup(&guest)).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Closing);

        let mut failed = IsolatedGuestSession::create(contract(), [3; 32]).unwrap();
        let lease = failed.acquire("agent-a").unwrap();
        failed.drive_to_ready("agent-a", &lease).unwrap();
        failed.fail_guest("agent-a", &lease).unwrap();
        assert_eq!(failed.phase(), IsolatedGuestPhase::Failed);
    }

    #[test]
    fn one_agent_per_guest_and_stale_lease_cannot_control() {
        let (mut guest, lease) = running_guest();
        guest.control("agent-a", &lease).unwrap();
        assert_eq!(
            guest.acquire("agent-b").unwrap_err().code,
            ComputerErrorCode::Conflict
        );
        assert_eq!(
            guest.control("agent-b", &lease).unwrap_err().code,
            ComputerErrorCode::ForbiddenAction
        );
        let stale = IsolatedGuestLease {
            lease_id: lease.lease_id.clone(),
            agent_id: lease.agent_id.clone(),
            revision: lease.revision + 1,
        };
        assert_eq!(
            guest.control("agent-a", &stale).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn control_without_lease_is_denied() {
        let guest = IsolatedGuestSession::create(contract(), [9; 32]).unwrap();
        let forged = IsolatedGuestLease {
            lease_id: "lease-forged".into(),
            agent_id: "agent-a".into(),
            revision: 1,
        };
        assert_eq!(
            guest.control("agent-a", &forged).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn concurrent_agents_cannot_both_hold_or_control() {
        let mut first = IsolatedGuestSession::create(contract(), [1; 32]).unwrap();
        let mut second = IsolatedGuestSession::create(contract(), [2; 32]).unwrap();
        let a = first.acquire("agent-a").unwrap();
        let b = second.acquire("agent-b").unwrap();
        first.drive_to_ready("agent-a", &a).unwrap();
        first.drive_to_running("agent-a", &a).unwrap();
        second.drive_to_ready("agent-b", &b).unwrap();
        second.drive_to_running("agent-b", &b).unwrap();
        first.control("agent-a", &a).unwrap();
        second.control("agent-b", &b).unwrap();
        assert_eq!(
            first.control("agent-b", &b).unwrap_err().code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(
            first.acquire("agent-b").unwrap_err().code,
            ComputerErrorCode::Conflict
        );
    }

    #[test]
    fn cancel_revokes_lease_and_requires_cleanup() {
        let (mut guest, lease) = running_guest();
        guest.cancel("agent-a", &lease).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Closing);
        assert!(guest.lease().is_none());
        assert_eq!(
            guest.control("agent-a", &lease).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            guest.acquire("agent-b").unwrap_err().code,
            ComputerErrorCode::InvalidState
        );
        guest.complete_cleanup(&cleanup(&guest)).unwrap();
        assert_eq!(
            guest.runtime.lifecycle_state(),
            IsolatedVisualLifecycleState::Terminated
        );
    }

    #[test]
    fn guest_failure_is_fail_closed_and_still_requires_cleanup() {
        let mut guest = IsolatedGuestSession::create(contract(), [9; 32]).unwrap();
        let lease = guest.acquire("agent-a").unwrap();
        guest.drive_to_ready("agent-a", &lease).unwrap();
        guest.fail_guest("agent-a", &lease).unwrap();
        assert_eq!(guest.phase(), IsolatedGuestPhase::Failed);
        assert!(guest.lease().is_none());
        assert_eq!(
            guest.control("agent-a", &lease).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        guest.complete_cleanup(&cleanup(&guest)).unwrap();
        assert_eq!(
            guest.runtime.lifecycle().terminal_disposition,
            Some(IsolatedVisualTerminalDisposition::Failed)
        );
    }

    #[test]
    fn captured_artifacts_drop_bytes_and_redact_sensitive_metadata() {
        let contract = contract();
        let mut guest_carrier =
            IsolatedVisualFrameCarrier::new_guest_with_challenge(&contract, &[9; 32]).unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        let chunks = guest_carrier
            .seal_frame(1, &nonce, 2, 2, &[1, 2, 3, 4])
            .unwrap();
        let mut host =
            IsolatedVisualFrameCarrier::new_host_with_challenge(&contract, &[9; 32]).unwrap();
        let frame = host.open_chunk(&chunks[0]).unwrap().unwrap();
        let captured = project_captured_artifact(&frame);
        let encoded = serde_json::to_string(&captured).unwrap();
        assert!(!encoded.contains("bytes"));
        assert!(!encoded.contains("/Users/"));
        assert_eq!(captured.frame_sequence, 1);

        let dirty = json!({
            "frameSequence": 1,
            "clipboard": "secret-paste",
            "helperPath": "/Users/chris/helper",
            "credential": "xai-not-a-real-key",
            "baseUrl": "https://api.example.invalid",
            "width": 2
        });
        let redacted = redact_isolated_capture(&dirty).unwrap();
        let text = redacted.to_string();
        assert!(!text.contains("clipboard"));
        assert!(!text.contains("helperPath"));
        assert!(!text.contains("credential"));
        assert!(!text.contains("baseUrl"));
        assert_eq!(redacted["width"], 2);

        let leftover = json!({ "note": "clipboard: copied token=abc" });
        assert_eq!(
            redact_isolated_capture(&leftover).unwrap_err().code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    #[test]
    fn running_control_still_requires_bound_helper() {
        let mut guest = IsolatedGuestSession::create(contract(), [9; 32]).unwrap();
        let lease = guest.acquire("agent-a").unwrap();
        guest.drive_to_ready("agent-a", &lease).unwrap();
        assert_eq!(
            guest.control("agent-a", &lease).unwrap_err().code,
            ComputerErrorCode::InvalidState
        );
    }
}
