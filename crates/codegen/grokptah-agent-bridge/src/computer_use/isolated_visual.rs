use serde::{Deserialize, Serialize};

use super::types::{
    validate_id, ComputerError, ComputerErrorCode, ComputerResult, ComputerSurfaceBinding,
};

pub const ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION: u32 = 1;
pub const MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID: &str = "macos_isolated_visual_candidate_v1";

const MAX_VCPUS: u8 = 2;
const MAX_MEMORY_MIB: u32 = 4_096;
const MAX_OVERLAY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_DISPLAY_WIDTH: u32 = 1_280;
const MAX_DISPLAY_HEIGHT: u32 = 800;
const MAX_FRAMES_PER_SECOND: u8 = 10;
const MAX_ENCODED_FRAME_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DURATION_SECONDS: u64 = 30 * 60;
const MAX_INPUT_EVENTS: u32 = 256;
const MAX_TEXT_EVENT_BYTES: u32 = 4 * 1024;

fn validate_digest(name: &str, value: &str) -> ComputerResult<()> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("invalid {name}"),
        ));
    }
    Ok(())
}

/// Closed default profile for the disposable guest. Any future host bridge is
/// a new reviewed capability, not a mutable model/provider setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualSecurityProfile {
    pub network_devices: u8,
    pub host_clipboard: bool,
    pub shared_directories: bool,
    pub credential_forwarding: bool,
    pub host_input_forwarding: bool,
    pub usb_passthrough: bool,
    pub camera: bool,
    pub microphone: bool,
}

impl IsolatedVisualSecurityProfile {
    pub fn locked_down() -> Self {
        Self {
            network_devices: 0,
            host_clipboard: false,
            shared_directories: false,
            credential_forwarding: false,
            host_input_forwarding: false,
            usb_passthrough: false,
            camera: false,
            microphone: false,
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        if self.network_devices != 0
            || self.host_clipboard
            || self.shared_directories
            || self.credential_forwarding
            || self.host_input_forwarding
            || self.usb_passthrough
            || self.camera
            || self.microphone
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "isolated visual profile requests an unreviewed host bridge",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualResourceLimits {
    pub virtual_cpus: u8,
    pub memory_mib: u32,
    pub overlay_bytes: u64,
    pub display_width: u32,
    pub display_height: u32,
    pub frames_per_second: u8,
    pub encoded_frame_bytes: u64,
    pub duration_seconds: u64,
    pub input_events: u32,
    pub text_event_bytes: u32,
}

impl IsolatedVisualResourceLimits {
    pub fn proof_defaults() -> Self {
        Self {
            virtual_cpus: MAX_VCPUS,
            memory_mib: MAX_MEMORY_MIB,
            overlay_bytes: MAX_OVERLAY_BYTES,
            display_width: MAX_DISPLAY_WIDTH,
            display_height: MAX_DISPLAY_HEIGHT,
            frames_per_second: MAX_FRAMES_PER_SECOND,
            encoded_frame_bytes: MAX_ENCODED_FRAME_BYTES,
            duration_seconds: 10 * 60,
            input_events: MAX_INPUT_EVENTS,
            text_event_bytes: MAX_TEXT_EVENT_BYTES,
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        if self.virtual_cpus == 0
            || self.virtual_cpus > MAX_VCPUS
            || self.memory_mib == 0
            || self.memory_mib > MAX_MEMORY_MIB
            || self.overlay_bytes == 0
            || self.overlay_bytes > MAX_OVERLAY_BYTES
            || self.display_width == 0
            || self.display_width > MAX_DISPLAY_WIDTH
            || self.display_height == 0
            || self.display_height > MAX_DISPLAY_HEIGHT
            || self.frames_per_second == 0
            || self.frames_per_second > MAX_FRAMES_PER_SECOND
            || self.encoded_frame_bytes == 0
            || self.encoded_frame_bytes > MAX_ENCODED_FRAME_BYTES
            || self.duration_seconds == 0
            || self.duration_seconds > MAX_DURATION_SECONDS
            || self.input_events == 0
            || self.input_events > MAX_INPUT_EVENTS
            || self.text_event_bytes == 0
            || self.text_event_bytes > MAX_TEXT_EVENT_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated visual resource request exceeds the proof ceiling",
            ));
        }
        Ok(())
    }
}

/// Exact packaged identities required before a helper or guest can start.
/// Digests are public evidence; paths, channel secrets, process handles, and
/// signing material are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualManifest {
    pub schema_version: u32,
    pub backend_id: String,
    pub guest_protocol_version: u32,
    pub helper_content_sha256: String,
    pub helper_signing_requirement_sha256: String,
    pub guest_image_sha256: String,
    pub configuration_sha256: String,
    pub security_profile: IsolatedVisualSecurityProfile,
    pub limits: IsolatedVisualResourceLimits,
}

impl IsolatedVisualManifest {
    pub fn validate(&self) -> ComputerResult<()> {
        if self.schema_version != ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION
            || self.backend_id != MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID
            || self.guest_protocol_version != ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual manifest version or backend identity is unsupported",
            ));
        }
        validate_digest("helper content digest", &self.helper_content_sha256)?;
        validate_digest(
            "helper signing requirement digest",
            &self.helper_signing_requirement_sha256,
        )?;
        validate_digest("guest image digest", &self.guest_image_sha256)?;
        validate_digest("configuration digest", &self.configuration_sha256)?;
        self.security_profile.validate()?;
        self.limits.validate()?;
        Ok(())
    }
}

/// One exact no-input lifecycle binding. An overlay path and channel secret
/// exist only in the eventual host-owned runtime and must never be serialized
/// into this contract or projected to a model/provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualLaunchContract {
    pub run_id: String,
    pub surface: ComputerSurfaceBinding,
    pub input_domain_id: String,
    pub manifest: IsolatedVisualManifest,
}

impl IsolatedVisualLaunchContract {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("run_id", &self.run_id)?;
        self.surface.validate()?;
        validate_id("input_domain_id", &self.input_domain_id)?;
        if self.input_domain_id == self.surface.surface_id()
            || self.input_domain_id == self.surface.incarnation()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated input domain is not independent from its surface identity",
            ));
        }
        self.manifest.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualLifecycleState {
    Prepared,
    Starting,
    ReadOnlyReady,
    Stopping,
    CleanupPending,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualTerminalDisposition {
    Cancelled,
    Interrupted,
    Failed,
}

/// Secret-free result of exact process/open-handle and resource deletion
/// checks. The lifecycle consumes it only when it matches the bound surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedVisualCleanupEvidence {
    surface: ComputerSurfaceBinding,
    helper_process_absent: bool,
    no_open_handles: bool,
    overlay_removed: bool,
    frame_cache_removed: bool,
}

impl IsolatedVisualCleanupEvidence {
    /// Construct evidence only after the host supervisor has completed every
    /// exact process, handle, overlay, and frame-cache check. This constructor
    /// is crate-private so a model, provider, or external coordinator cannot
    /// manufacture terminal cleanup authority from serialized booleans.
    pub(crate) fn verified(
        surface: ComputerSurfaceBinding,
        helper_process_absent: bool,
        no_open_handles: bool,
        overlay_removed: bool,
        frame_cache_removed: bool,
    ) -> ComputerResult<Self> {
        surface.validate()?;
        let evidence = Self {
            surface,
            helper_process_absent,
            no_open_handles,
            overlay_removed,
            frame_cache_removed,
        };
        evidence.validates_for(&evidence.surface)?;
        Ok(evidence)
    }

    fn validates_for(&self, surface: &ComputerSurfaceBinding) -> ComputerResult<()> {
        self.surface.validate()?;
        if &self.surface != surface {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "cleanup evidence belongs to another isolated surface",
            ));
        }
        if !self.helper_process_absent || !self.no_open_handles {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated helper or resource handle is still active",
            ));
        }
        if !self.overlay_removed || !self.frame_cache_removed {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated overlay or frame cache cleanup is incomplete",
            ));
        }
        Ok(())
    }
}

/// Deterministic lifecycle contract for the future no-input VM proof. It owns
/// no VM itself and deliberately has no transition that resumes after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualLifecycle {
    pub contract: IsolatedVisualLaunchContract,
    pub state: IsolatedVisualLifecycleState,
    pub revision: u64,
    pub terminal_disposition: Option<IsolatedVisualTerminalDisposition>,
}

impl IsolatedVisualLifecycle {
    pub fn new(contract: IsolatedVisualLaunchContract) -> ComputerResult<Self> {
        contract.validate()?;
        Ok(Self {
            contract,
            state: IsolatedVisualLifecycleState::Prepared,
            revision: 1,
            terminal_disposition: None,
        })
    }

    pub fn state(&self) -> IsolatedVisualLifecycleState {
        self.state
    }

    pub fn contract(&self) -> &IsolatedVisualLaunchContract {
        &self.contract
    }

    pub fn begin_start(&mut self) -> ComputerResult<()> {
        self.transition(
            IsolatedVisualLifecycleState::Prepared,
            IsolatedVisualLifecycleState::Starting,
        )
    }

    pub fn mark_read_only_ready(&mut self) -> ComputerResult<()> {
        self.transition(
            IsolatedVisualLifecycleState::Starting,
            IsolatedVisualLifecycleState::ReadOnlyReady,
        )
    }

    pub fn begin_stop(
        &mut self,
        disposition: IsolatedVisualTerminalDisposition,
    ) -> ComputerResult<()> {
        if !matches!(
            self.state,
            IsolatedVisualLifecycleState::Starting | IsolatedVisualLifecycleState::ReadOnlyReady
        ) || self.terminal_disposition.is_some()
        {
            return Err(invalid_transition());
        }
        self.state = IsolatedVisualLifecycleState::Stopping;
        self.terminal_disposition = Some(disposition);
        self.bump_revision();
        Ok(())
    }

    /// Records a helper/runtime failure and requires the same exact cleanup
    /// evidence as an operator stop. A failure never resumes the lifecycle or
    /// skips the cleanup proof.
    pub fn fail(&mut self) -> ComputerResult<()> {
        if matches!(
            self.state,
            IsolatedVisualLifecycleState::Prepared
                | IsolatedVisualLifecycleState::Starting
                | IsolatedVisualLifecycleState::ReadOnlyReady
                | IsolatedVisualLifecycleState::Stopping
        ) && self.terminal_disposition.is_none()
        {
            self.state = IsolatedVisualLifecycleState::CleanupPending;
            self.terminal_disposition = Some(IsolatedVisualTerminalDisposition::Failed);
            self.bump_revision();
            return Ok(());
        }
        if self.state == IsolatedVisualLifecycleState::Stopping
            && self.terminal_disposition.is_some()
        {
            return self.require_cleanup();
        }
        Err(invalid_transition())
    }

    /// The out-of-band stop/kill grace period has ended. This transition does
    /// not claim the process or handles are absent; exact cleanup evidence is
    /// still mandatory.
    pub fn require_cleanup(&mut self) -> ComputerResult<()> {
        if self.state != IsolatedVisualLifecycleState::Stopping
            || self.terminal_disposition.is_none()
        {
            return Err(invalid_transition());
        }
        self.state = IsolatedVisualLifecycleState::CleanupPending;
        self.bump_revision();
        Ok(())
    }

    /// Reopening a nonterminal lifecycle always interrupts it and requires
    /// cleanup. There is intentionally no automatic-resume transition.
    pub fn interrupt_on_restart(&mut self) -> ComputerResult<()> {
        match self.state {
            IsolatedVisualLifecycleState::Prepared => {
                self.state = IsolatedVisualLifecycleState::Terminated;
            }
            IsolatedVisualLifecycleState::Starting
            | IsolatedVisualLifecycleState::ReadOnlyReady
            | IsolatedVisualLifecycleState::Stopping
            | IsolatedVisualLifecycleState::CleanupPending => {
                self.state = IsolatedVisualLifecycleState::CleanupPending;
            }
            IsolatedVisualLifecycleState::Terminated => return Ok(()),
        }
        self.terminal_disposition = Some(IsolatedVisualTerminalDisposition::Interrupted);
        self.bump_revision();
        Ok(())
    }

    pub fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        if self.state != IsolatedVisualLifecycleState::CleanupPending
            || self.terminal_disposition.is_none()
        {
            return Err(invalid_transition());
        }
        evidence.validates_for(&self.contract.surface)?;
        self.state = IsolatedVisualLifecycleState::Terminated;
        self.bump_revision();
        Ok(())
    }

    pub fn validate(&self) -> ComputerResult<()> {
        self.contract.validate()?;
        if self.revision == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual lifecycle revision is invalid",
            ));
        }
        let terminal_expected = matches!(
            self.state,
            IsolatedVisualLifecycleState::Stopping
                | IsolatedVisualLifecycleState::CleanupPending
                | IsolatedVisualLifecycleState::Terminated
        );
        if terminal_expected != self.terminal_disposition.is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated visual lifecycle disposition contradicts its state",
            ));
        }
        Ok(())
    }

    fn transition(
        &mut self,
        expected: IsolatedVisualLifecycleState,
        next: IsolatedVisualLifecycleState,
    ) -> ComputerResult<()> {
        if self.state != expected || self.terminal_disposition.is_some() {
            return Err(invalid_transition());
        }
        self.state = next;
        self.bump_revision();
        Ok(())
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

fn invalid_transition() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::InvalidState,
        "invalid isolated visual lifecycle transition",
    )
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn manifest() -> IsolatedVisualManifest {
        IsolatedVisualManifest {
            schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
            helper_content_sha256: "a".repeat(64),
            helper_signing_requirement_sha256: "b".repeat(64),
            guest_image_sha256: "c".repeat(64),
            configuration_sha256: "d".repeat(64),
            security_profile: IsolatedVisualSecurityProfile::locked_down(),
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        }
    }

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: Uuid::new_v4().to_string(),
            surface: ComputerSurfaceBinding::issue(),
            input_domain_id: Uuid::new_v4().to_string(),
            manifest: manifest(),
        }
    }

    fn cleanup(surface: &ComputerSurfaceBinding) -> IsolatedVisualCleanupEvidence {
        IsolatedVisualCleanupEvidence {
            surface: surface.clone(),
            helper_process_absent: true,
            no_open_handles: true,
            overlay_removed: true,
            frame_cache_removed: true,
        }
    }

    #[test]
    fn manifest_rejects_unreviewed_bridges_and_resource_expansion() {
        let mut candidate = manifest();
        candidate.validate().unwrap();
        candidate.security_profile.network_devices = 1;
        assert_eq!(
            candidate.validate().unwrap_err().code,
            ComputerErrorCode::ForbiddenAction
        );
        candidate.security_profile = IsolatedVisualSecurityProfile::locked_down();
        candidate.limits.memory_mib = MAX_MEMORY_MIB + 1;
        assert_eq!(
            candidate.validate().unwrap_err().code,
            ComputerErrorCode::LimitReached
        );
        candidate.limits = IsolatedVisualResourceLimits::proof_defaults();
        candidate.guest_image_sha256 = "A".repeat(64);
        assert_eq!(
            candidate.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn no_input_lifecycle_requires_exact_cleanup_before_terminal() {
        let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
        lifecycle.begin_start().unwrap();
        lifecycle.mark_read_only_ready().unwrap();
        lifecycle
            .begin_stop(IsolatedVisualTerminalDisposition::Cancelled)
            .unwrap();
        lifecycle.require_cleanup().unwrap();

        let mut incomplete = cleanup(&lifecycle.contract.surface);
        incomplete.no_open_handles = false;
        assert_eq!(
            lifecycle.complete_cleanup(&incomplete).unwrap_err().code,
            ComputerErrorCode::Conflict
        );
        assert_eq!(
            lifecycle.state,
            IsolatedVisualLifecycleState::CleanupPending
        );

        lifecycle
            .complete_cleanup(&cleanup(&lifecycle.contract.surface))
            .unwrap();
        lifecycle.validate().unwrap();
        assert_eq!(lifecycle.state, IsolatedVisualLifecycleState::Terminated);
        assert_eq!(
            lifecycle.terminal_disposition,
            Some(IsolatedVisualTerminalDisposition::Cancelled)
        );
    }

    #[test]
    fn restart_interrupts_without_resume_and_foreign_cleanup_is_rejected() {
        let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
        lifecycle.begin_start().unwrap();
        lifecycle.mark_read_only_ready().unwrap();
        lifecycle.interrupt_on_restart().unwrap();
        assert_eq!(
            lifecycle.state,
            IsolatedVisualLifecycleState::CleanupPending
        );
        assert_eq!(
            lifecycle.mark_read_only_ready().unwrap_err().code,
            ComputerErrorCode::InvalidState
        );

        let foreign = cleanup(&contract().surface);
        assert_eq!(
            lifecycle.complete_cleanup(&foreign).unwrap_err().code,
            ComputerErrorCode::ForbiddenTarget
        );
        lifecycle
            .complete_cleanup(&cleanup(&lifecycle.contract.surface))
            .unwrap();
        lifecycle.interrupt_on_restart().unwrap();
        assert_eq!(lifecycle.state, IsolatedVisualLifecycleState::Terminated);
    }

    #[test]
    fn failure_after_committed_stop_still_requires_exact_cleanup() {
        let mut lifecycle = IsolatedVisualLifecycle::new(contract()).unwrap();
        lifecycle.begin_start().unwrap();
        lifecycle.mark_read_only_ready().unwrap();
        lifecycle
            .begin_stop(IsolatedVisualTerminalDisposition::Interrupted)
            .unwrap();

        // A helper failure after the stop boundary must not reopen the run or
        // replace the already-recorded terminal disposition. It only advances
        // the same cleanup-pending path used by a normal stopped event.
        lifecycle.fail().unwrap();
        assert_eq!(
            lifecycle.state,
            IsolatedVisualLifecycleState::CleanupPending
        );
        assert_eq!(
            lifecycle.terminal_disposition,
            Some(IsolatedVisualTerminalDisposition::Interrupted)
        );
        assert!(lifecycle.mark_read_only_ready().is_err());

        lifecycle
            .complete_cleanup(&cleanup(&lifecycle.contract.surface))
            .unwrap();
        assert_eq!(lifecycle.state, IsolatedVisualLifecycleState::Terminated);
    }

    #[test]
    fn serialized_contract_contains_no_host_paths_or_channel_secret() {
        let value = serde_json::to_value(contract()).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 4);
        for required in ["inputDomainId", "manifest", "runId", "surface"] {
            assert!(object.contains_key(required));
        }
        let encoded = serde_json::to_string(&value).unwrap();
        for forbidden in [
            "overlayPath",
            "helperPath",
            "channelSecret",
            "credential",
            "hostHome",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
