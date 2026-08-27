//! Platform-neutral packaged launch admission, per-operation authority, and
//! deterministic launch/cleanup receipts.
//!
//! The packaged macOS supervisor in [`super::macos_isolated_runtime`] receives
//! one child process id and five private channel descriptors from the native
//! spawn shim. Every check on that set previously lived inside
//! `#[cfg(target_os = "macos")]` code — duplicated once in the runtime and once
//! in the artifact opener — so on any non-macOS host nothing compiled it and
//! nothing tested it. Both copies also accepted descriptors `0`, `1`, and `2`,
//! which would let a mis-behaving shim hand back stdin/stdout/stderr as if they
//! were private guest channels.
//!
//! This module holds that admission decision once, in ordinary portable source,
//! and then binds it to the identities that make it meaningful:
//!
//! * the packaged artifact receipt ([`IsolatedVisualPackagedArtifactReceipt`]),
//!   which can only be minted after signature, entitlement, and content
//!   measurement all succeed;
//! * the launch contract's run / surface / input-domain triple; and
//! * exactly one [`IsolatedGuestLease`].
//!
//! It owns no process, opens no descriptor, and launches no VM. Admission here
//! is necessary for packaged control and never sufficient: real boot, render,
//! input, and reap proof remains macOS hardware work.

use serde::Serialize;

use super::isolated_guest::IsolatedGuestLease;
use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualManifest,
    MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
};
use super::isolated_visual_artifacts::IsolatedVisualPackagedArtifactReceipt;
use super::types::{
    validate_id, ComputerError, ComputerErrorCode, ComputerResult, ComputerSurfaceBinding,
};

pub const ISOLATED_VISUAL_LAUNCH_RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Lowest descriptor a private guest channel may occupy. `0`, `1`, and `2` are
/// the inherited standard streams: a shim that returns one of them is either
/// broken or redirecting guest traffic onto a host stream, and both fail closed.
pub const ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR: i64 = 3;

/// Upper bound on a plausible descriptor number. This is not a resource limit;
/// it rejects sentinel and garbage values that a native struct could carry.
pub const ISOLATED_VISUAL_MAX_DESCRIPTOR: i64 = 1_048_575;

/// The packaged helper contract is exactly five private channels. A launch that
/// reports any other number of usable channels is incomplete, not degraded.
pub const ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT: usize = 5;

/// Shared private-descriptor rule for every packaged native result.
///
/// A returned descriptor set is usable only when each entry is a real, private
/// descriptor and no two entries alias. `0`, `1`, and `2` are excluded because
/// they are the inherited standard streams, not private packaged channels.
/// This is the one implementation; the macOS artifact opener and the packaged
/// runtime both call it instead of keeping their own copies.
pub(crate) fn descriptors_are_private_and_distinct(descriptors: &[i64]) -> bool {
    descriptors.iter().enumerate().all(|(index, descriptor)| {
        *descriptor >= ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR
            && *descriptor <= ISOLATED_VISUAL_MAX_DESCRIPTOR
            && descriptors[..index].iter().all(|prior| prior != descriptor)
    })
}

/// The five private channels the packaged helper must return, in the fixed
/// order used by every receipt so serialization stays deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualChannelRole {
    Control,
    Event,
    Input,
    Frame,
    Challenge,
}

impl IsolatedVisualChannelRole {
    pub const ALL: [Self; ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT] = [
        Self::Control,
        Self::Event,
        Self::Input,
        Self::Frame,
        Self::Challenge,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Event => "event",
            Self::Input => "input",
            Self::Frame => "frame",
            Self::Challenge => "challenge",
        }
    }
}

/// Raw process and channel identity as reported by a packaged launch. Values
/// are plain integers so a native shim result maps onto this type without
/// interpretation; nothing here is trusted until [`Self::admit`] accepts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolatedVisualLaunchDescriptors {
    pub process_id: i64,
    pub control: i64,
    pub event: i64,
    pub input: i64,
    pub frame: i64,
    pub challenge: i64,
}

impl IsolatedVisualLaunchDescriptors {
    fn channels(&self) -> [(IsolatedVisualChannelRole, i64); ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT] {
        [
            (IsolatedVisualChannelRole::Control, self.control),
            (IsolatedVisualChannelRole::Event, self.event),
            (IsolatedVisualChannelRole::Input, self.input),
            (IsolatedVisualChannelRole::Frame, self.frame),
            (IsolatedVisualChannelRole::Challenge, self.challenge),
        ]
    }

    /// Exact completeness and identity check for one packaged launch.
    ///
    /// A launch is admissible only when the child process is a plausible,
    /// non-self process and all five channels are present, private, distinct,
    /// and in range. Partial success is not a state this contract has.
    pub fn admit(self) -> ComputerResult<IsolatedVisualLaunchDescriptorSet> {
        if self.process_id <= 1 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated launch reported no usable child process",
            ));
        }
        if self.process_id > i64::from(u32::MAX) {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated launch reported an implausible child process",
            ));
        }
        if self.process_id == i64::from(std::process::id()) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "isolated launch reported the host process as its guest supervisor",
            ));
        }

        let channels = self.channels();
        for (index, (role, descriptor)) in channels.iter().enumerate() {
            if *descriptor < ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR {
                return Err(ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    format!(
                        "isolated {} channel is missing or aliases a standard stream",
                        role.label()
                    ),
                ));
            }
            if *descriptor > ISOLATED_VISUAL_MAX_DESCRIPTOR {
                return Err(ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    format!(
                        "isolated {} channel descriptor is out of range",
                        role.label()
                    ),
                ));
            }
            if let Some((prior, _)) = channels[..index]
                .iter()
                .find(|(_, earlier)| earlier == descriptor)
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    format!(
                        "isolated {} channel aliases the {} channel",
                        role.label(),
                        prior.label()
                    ),
                ));
            }
        }

        // The per-role loop above exists for precise diagnostics. The single
        // shared predicate below is the invariant every packaged native result
        // is held to, so admission here and the macOS callers cannot drift.
        let raw = channels.map(|(_, descriptor)| descriptor);
        if !descriptors_are_private_and_distinct(&raw) {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated launch returned an incomplete private channel set",
            ));
        }

        Ok(IsolatedVisualLaunchDescriptorSet { channels })
    }
}

/// A packaged launch whose process and five private channels have passed exact
/// completeness and identity admission. Descriptor numbers stay private: they
/// are host resources and never reach a receipt, projection, or provider.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IsolatedVisualLaunchDescriptorSet {
    channels: [(IsolatedVisualChannelRole, i64); ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT],
}

impl std::fmt::Debug for IsolatedVisualLaunchDescriptorSet {
    /// Descriptor numbers are host resources, so an admitted set prints its
    /// roles and never its descriptors.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedVisualLaunchDescriptorSet")
            .field("channel_count", &self.channels.len())
            .field("roles", &self.roles())
            .finish()
    }
}

impl IsolatedVisualLaunchDescriptorSet {
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn roles(&self) -> [IsolatedVisualChannelRole; ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT] {
        self.channels.map(|(role, _)| role)
    }
}

/// The run / surface / input-domain triple one packaged guest is bound to.
/// Control presented against any other triple is a wrong-guest call, checked
/// before a lease is even consulted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedVisualGuestBinding {
    pub run_id: String,
    pub surface: ComputerSurfaceBinding,
    pub input_domain_id: String,
}

impl IsolatedVisualGuestBinding {
    pub fn from_contract(contract: &IsolatedVisualLaunchContract) -> ComputerResult<Self> {
        contract.validate()?;
        Ok(Self {
            run_id: contract.run_id.clone(),
            surface: contract.surface.clone(),
            input_domain_id: contract.input_domain_id.clone(),
        })
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("run_id", &self.run_id)?;
        self.surface.validate()?;
        validate_id("input_domain_id", &self.input_domain_id)
    }

    fn require(&self, presented: &Self) -> ComputerResult<()> {
        self.validate()?;
        presented.validate()?;
        if self != presented {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated guest control targets another run, surface, or input domain",
            ));
        }
        Ok(())
    }
}

/// The five authority-bearing operations a leased packaged guest exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualGuestOperation {
    Start,
    ReadFrame,
    WriteInput,
    Stop,
    Cleanup,
}

impl IsolatedVisualGuestOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::ReadFrame => "read frame",
            Self::WriteInput => "write input",
            Self::Stop => "stop",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualAuthorityState {
    Admitted,
    Started,
    Stopping,
    Revoked,
    Terminated,
}

/// Why packaged authority ended. Every reason requires the same exact cleanup
/// evidence; none of them resumes control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualRevocation {
    OperatorStop,
    Cancelled,
    HelperLoss,
    RestartInterrupted,
    LeaseRevoked,
}

/// Exactly one Agent's authority over exactly one admitted packaged guest.
///
/// This does not replace [`super::isolated_visual::IsolatedVisualLifecycle`],
/// which stays the host/helper state contract, nor the durable store's surface
/// leases. It is the packaged boundary: which operation this caller may attempt
/// right now, against this guest, on this launch.
pub struct IsolatedVisualLaunchAuthority {
    binding: IsolatedVisualGuestBinding,
    manifest: IsolatedVisualManifest,
    descriptors: IsolatedVisualLaunchDescriptorSet,
    lease: Option<IsolatedGuestLease>,
    agent_id: String,
    lease_revision: u64,
    state: IsolatedVisualAuthorityState,
    revocation: Option<IsolatedVisualRevocation>,
}

impl std::fmt::Debug for IsolatedVisualLaunchAuthority {
    /// The live lease is authority-bearing, so it is never printed.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedVisualLaunchAuthority")
            .field("binding", &self.binding)
            .field("descriptors", &self.descriptors)
            .field("agent_id", &self.agent_id)
            .field("lease_revision", &self.lease_revision)
            .field("lease_held", &self.lease.is_some())
            .field("state", &self.state)
            .field("revocation", &self.revocation)
            .finish()
    }
}

impl IsolatedVisualLaunchAuthority {
    /// Admit one launch. Every identity is checked together: the contract must
    /// validate, the artifact receipt must match that contract's manifest, and
    /// the lease must be live.
    ///
    /// This is public but not a capability: the only way to obtain an
    /// [`IsolatedVisualPackagedArtifactReceipt`] is
    /// [`super::isolated_visual_artifacts::measure_packaged_isolated_visual_artifacts`],
    /// which requires a signed macOS package and fails closed everywhere else.
    /// A caller therefore cannot assemble authority out of serialized values.
    pub fn admit(
        contract: &IsolatedVisualLaunchContract,
        receipt: &IsolatedVisualPackagedArtifactReceipt,
        descriptors: IsolatedVisualLaunchDescriptorSet,
        lease: IsolatedGuestLease,
    ) -> ComputerResult<Self> {
        contract.validate()?;
        receipt.validate_against_manifest(&contract.manifest)?;
        let binding = IsolatedVisualGuestBinding::from_contract(contract)?;
        let agent_id = lease.agent_id.clone();
        let lease_revision = lease.revision;
        lease.require(&agent_id, &lease)?;
        Ok(Self {
            binding,
            manifest: contract.manifest.clone(),
            descriptors,
            lease: Some(lease),
            agent_id,
            lease_revision,
            state: IsolatedVisualAuthorityState::Admitted,
            revocation: None,
        })
    }

    pub fn state(&self) -> IsolatedVisualAuthorityState {
        self.state
    }

    pub fn revocation(&self) -> Option<IsolatedVisualRevocation> {
        self.revocation
    }

    pub fn binding(&self) -> &IsolatedVisualGuestBinding {
        &self.binding
    }

    pub fn channel_count(&self) -> usize {
        self.descriptors.channel_count()
    }

    fn permits(&self, operation: IsolatedVisualGuestOperation) -> bool {
        match self.state {
            IsolatedVisualAuthorityState::Admitted => {
                operation == IsolatedVisualGuestOperation::Start
            }
            IsolatedVisualAuthorityState::Started => matches!(
                operation,
                IsolatedVisualGuestOperation::ReadFrame
                    | IsolatedVisualGuestOperation::WriteInput
                    | IsolatedVisualGuestOperation::Stop
            ),
            IsolatedVisualAuthorityState::Stopping | IsolatedVisualAuthorityState::Revoked => {
                operation == IsolatedVisualGuestOperation::Cleanup
            }
            IsolatedVisualAuthorityState::Terminated => false,
        }
    }

    /// The single admission point for packaged control. Guest binding is
    /// checked before the lease so a wrong-guest call never consults, and never
    /// reports on, another guest's lease state.
    pub fn authorize(
        &self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
        binding: &IsolatedVisualGuestBinding,
        operation: IsolatedVisualGuestOperation,
    ) -> ComputerResult<()> {
        self.binding.require(binding)?;
        let Some(live) = self.lease.as_ref() else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated packaged authority was revoked and holds no lease",
            ));
        };
        live.require(agent_id, lease)?;
        if !self.permits(operation) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                format!(
                    "isolated packaged guest does not authorize {} in its current state",
                    operation.label()
                ),
            ));
        }
        Ok(())
    }

    pub fn record_started(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
        binding: &IsolatedVisualGuestBinding,
    ) -> ComputerResult<()> {
        self.authorize(
            agent_id,
            lease,
            binding,
            IsolatedVisualGuestOperation::Start,
        )?;
        self.state = IsolatedVisualAuthorityState::Started;
        Ok(())
    }

    /// Begin a graceful operator stop. Frame and input authority end here, not
    /// when cleanup finishes.
    pub fn begin_stop(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
        binding: &IsolatedVisualGuestBinding,
    ) -> ComputerResult<()> {
        self.authorize(agent_id, lease, binding, IsolatedVisualGuestOperation::Stop)?;
        self.state = IsolatedVisualAuthorityState::Stopping;
        self.revocation = Some(IsolatedVisualRevocation::OperatorStop);
        self.lease = None;
        Ok(())
    }

    /// End authority out of band: cancel, helper loss, restart, or an
    /// externally revoked lease. Cleanup is still mandatory afterwards, and a
    /// second revocation of the same authority is a conflict rather than a
    /// silent no-op, so a cancel racing a helper death cannot be counted twice.
    pub fn revoke(&mut self, reason: IsolatedVisualRevocation) -> ComputerResult<()> {
        match self.state {
            IsolatedVisualAuthorityState::Terminated => Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated packaged authority already completed cleanup",
            )),
            IsolatedVisualAuthorityState::Revoked => Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated packaged authority is already revoked",
            )),
            _ => {
                self.state = IsolatedVisualAuthorityState::Revoked;
                self.revocation = Some(reason);
                self.lease = None;
                Ok(())
            }
        }
    }

    /// Deterministic, path-free, secret-free launch identity. Descriptor
    /// numbers, the child process id, the channel secret, and the lease id are
    /// all deliberately absent; the receipt is `Serialize` only so it cannot be
    /// read back into authority.
    pub fn launch_receipt(&self) -> IsolatedVisualLaunchReceipt {
        IsolatedVisualLaunchReceipt {
            schema_version: ISOLATED_VISUAL_LAUNCH_RECEIPT_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            run_id: self.binding.run_id.clone(),
            surface_id: self.binding.surface.surface_id().into(),
            incarnation: self.binding.surface.incarnation().into(),
            input_domain_id: self.binding.input_domain_id.clone(),
            agent_id: self.agent_id.clone(),
            lease_revision: self.lease_revision,
            helper_content_sha256: self.manifest.helper_content_sha256.clone(),
            helper_signing_requirement_sha256: self
                .manifest
                .helper_signing_requirement_sha256
                .clone(),
            guest_image_sha256: self.manifest.guest_image_sha256.clone(),
            configuration_sha256: self.manifest.configuration_sha256.clone(),
            channels: IsolatedVisualChannelRole::ALL.to_vec(),
            state: self.state,
        }
    }

    /// Consume exact cleanup evidence and terminate this authority.
    ///
    /// Evidence is accepted only for this guest's own surface, and only once
    /// authority has already ended: cleanup is never how a live guest is
    /// stopped. The returned receipt reports the checks the host actually
    /// passed, because [`IsolatedVisualCleanupEvidence`] cannot be constructed
    /// with an incomplete one.
    pub fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<IsolatedVisualCleanupReceipt> {
        let disposition = match self.state {
            IsolatedVisualAuthorityState::Stopping | IsolatedVisualAuthorityState::Revoked => {
                self.revocation.ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::Internal,
                        "isolated packaged authority ended without a recorded disposition",
                    )
                })?
            }
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "isolated packaged cleanup requires authority to have ended first",
                ))
            }
        };
        evidence.validates_for(&self.binding.surface)?;
        self.state = IsolatedVisualAuthorityState::Terminated;
        Ok(IsolatedVisualCleanupReceipt {
            schema_version: ISOLATED_VISUAL_LAUNCH_RECEIPT_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            run_id: self.binding.run_id.clone(),
            surface_id: self.binding.surface.surface_id().into(),
            incarnation: self.binding.surface.incarnation().into(),
            input_domain_id: self.binding.input_domain_id.clone(),
            disposition,
            helper_process_absent: true,
            no_open_handles: true,
            overlay_removed: true,
            frame_cache_removed: true,
            lease_revoked: true,
            channels_released: ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT,
        })
    }
}

/// Deterministic public projection of one admitted packaged launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedVisualLaunchReceipt {
    pub schema_version: u32,
    pub backend_id: String,
    pub run_id: String,
    pub surface_id: String,
    pub incarnation: String,
    pub input_domain_id: String,
    pub agent_id: String,
    pub lease_revision: u64,
    pub helper_content_sha256: String,
    pub helper_signing_requirement_sha256: String,
    pub guest_image_sha256: String,
    pub configuration_sha256: String,
    pub channels: Vec<IsolatedVisualChannelRole>,
    pub state: IsolatedVisualAuthorityState,
}

/// Deterministic public projection of one completed packaged teardown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedVisualCleanupReceipt {
    pub schema_version: u32,
    pub backend_id: String,
    pub run_id: String,
    pub surface_id: String,
    pub incarnation: String,
    pub input_domain_id: String,
    pub disposition: IsolatedVisualRevocation,
    pub helper_process_absent: bool,
    pub no_open_handles: bool,
    pub overlay_removed: bool,
    pub frame_cache_removed: bool,
    pub lease_revoked: bool,
    pub channels_released: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::{
        IsolatedVisualArtifactMeasurement, IsolatedVisualArtifactMeasurements,
        IsolatedVisualArtifactRole, IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
    };

    const HELPER_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const REQUIREMENT_DIGEST: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    const GUEST_DIGEST: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const CONFIG_DIGEST: &str = "4444444444444444444444444444444444444444444444444444444444444444";

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: "run-packaged-launch".into(),
            surface: ComputerSurfaceBinding {
                surface_id: "surface-packaged".into(),
                incarnation: "incarnation-packaged".into(),
            },
            input_domain_id: "input-packaged".into(),
            manifest: IsolatedVisualManifest {
                schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
                backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
                guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
                helper_content_sha256: HELPER_DIGEST.into(),
                helper_signing_requirement_sha256: REQUIREMENT_DIGEST.into(),
                guest_image_sha256: GUEST_DIGEST.into(),
                configuration_sha256: CONFIG_DIGEST.into(),
                security_profile: IsolatedVisualSecurityProfile::locked_down(),
                limits: IsolatedVisualResourceLimits::proof_defaults(),
            },
        }
    }

    fn measurements() -> IsolatedVisualArtifactMeasurements {
        IsolatedVisualArtifactMeasurements {
            helper: IsolatedVisualArtifactMeasurement {
                role: IsolatedVisualArtifactRole::HelperExecutable,
                content_sha256: HELPER_DIGEST.into(),
                bytes: 4_096,
            },
            guest_image: IsolatedVisualArtifactMeasurement {
                role: IsolatedVisualArtifactRole::GuestImage,
                content_sha256: GUEST_DIGEST.into(),
                bytes: 8_192,
            },
            configuration: IsolatedVisualArtifactMeasurement {
                role: IsolatedVisualArtifactRole::Configuration,
                content_sha256: CONFIG_DIGEST.into(),
                bytes: 64,
            },
        }
    }

    fn receipt() -> IsolatedVisualPackagedArtifactReceipt {
        IsolatedVisualPackagedArtifactReceipt::verified(REQUIREMENT_DIGEST.into(), measurements())
            .unwrap()
    }

    fn descriptors() -> IsolatedVisualLaunchDescriptors {
        IsolatedVisualLaunchDescriptors {
            process_id: 4_242,
            control: 3,
            event: 4,
            input: 5,
            frame: 6,
            challenge: 7,
        }
    }

    fn authority() -> (
        IsolatedVisualLaunchAuthority,
        IsolatedGuestLease,
        IsolatedVisualGuestBinding,
    ) {
        let contract = contract();
        let lease = IsolatedGuestLease::issue("agent-a").unwrap();
        let binding = IsolatedVisualGuestBinding::from_contract(&contract).unwrap();
        let authority = IsolatedVisualLaunchAuthority::admit(
            &contract,
            &receipt(),
            descriptors().admit().unwrap(),
            lease.clone(),
        )
        .unwrap();
        (authority, lease, binding)
    }

    fn started() -> (
        IsolatedVisualLaunchAuthority,
        IsolatedGuestLease,
        IsolatedVisualGuestBinding,
    ) {
        let (mut authority, lease, binding) = authority();
        authority
            .record_started("agent-a", &lease, &binding)
            .unwrap();
        (authority, lease, binding)
    }

    fn evidence_for(surface: &ComputerSurfaceBinding) -> IsolatedVisualCleanupEvidence {
        IsolatedVisualCleanupEvidence::verified(surface.clone(), true, true, true, true).unwrap()
    }

    // ---------- descriptor identity and completeness ----------

    #[test]
    fn complete_private_distinct_channel_set_is_admitted() {
        let set = descriptors().admit().unwrap();
        assert_eq!(set.channel_count(), ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT);
        assert_eq!(set.roles(), IsolatedVisualChannelRole::ALL);
    }

    #[test]
    fn every_channel_rejects_a_standard_stream_alias() {
        for stream in [0_i64, 1, 2] {
            for slot in 0..ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT {
                let mut raw = descriptors();
                match slot {
                    0 => raw.control = stream,
                    1 => raw.event = stream,
                    2 => raw.input = stream,
                    3 => raw.frame = stream,
                    _ => raw.challenge = stream,
                }
                let error = raw.admit().unwrap_err();
                assert_eq!(
                    error.code,
                    ComputerErrorCode::BackendFailure,
                    "stream {stream} in slot {slot} must fail closed"
                );
                assert!(error.message.contains("standard stream"));
            }
        }
    }

    #[test]
    fn every_channel_rejects_a_missing_descriptor() {
        for slot in 0..ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT {
            let mut raw = descriptors();
            match slot {
                0 => raw.control = -1,
                1 => raw.event = -1,
                2 => raw.input = -1,
                3 => raw.frame = -1,
                _ => raw.challenge = -1,
            }
            assert_eq!(
                raw.admit().unwrap_err().code,
                ComputerErrorCode::BackendFailure,
                "missing descriptor in slot {slot} must fail closed"
            );
        }
    }

    #[test]
    fn duplicate_channels_are_rejected_and_name_both_roles() {
        let mut raw = descriptors();
        raw.frame = raw.control;
        let error = raw.admit().unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::BackendFailure);
        assert!(error.message.contains("frame"), "{}", error.message);
        assert!(error.message.contains("control"), "{}", error.message);

        let mut every_pair_rejected = 0;
        for left in 0..ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT {
            for right in (left + 1)..ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT {
                let mut raw = descriptors();
                let shared = 11_i64;
                for slot in [left, right] {
                    match slot {
                        0 => raw.control = shared,
                        1 => raw.event = shared,
                        2 => raw.input = shared,
                        3 => raw.frame = shared,
                        _ => raw.challenge = shared,
                    }
                }
                assert!(raw.admit().is_err());
                every_pair_rejected += 1;
            }
        }
        assert_eq!(every_pair_rejected, 10);
    }

    #[test]
    fn out_of_range_descriptors_are_rejected() {
        let mut raw = descriptors();
        raw.input = ISOLATED_VISUAL_MAX_DESCRIPTOR + 1;
        assert_eq!(
            raw.admit().unwrap_err().code,
            ComputerErrorCode::LimitReached
        );

        let mut raw = descriptors();
        raw.challenge = i64::MAX;
        assert_eq!(
            raw.admit().unwrap_err().code,
            ComputerErrorCode::LimitReached
        );
    }

    #[test]
    fn implausible_or_self_referential_processes_are_rejected() {
        for pid in [i64::MIN, -1, 0, 1] {
            let mut raw = descriptors();
            raw.process_id = pid;
            assert_eq!(
                raw.admit().unwrap_err().code,
                ComputerErrorCode::BackendFailure,
                "pid {pid} must fail closed"
            );
        }

        let mut raw = descriptors();
        raw.process_id = i64::from(u32::MAX) + 1;
        assert_eq!(
            raw.admit().unwrap_err().code,
            ComputerErrorCode::BackendFailure
        );

        let mut raw = descriptors();
        raw.process_id = i64::from(std::process::id());
        let error = raw.admit().unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);
        assert!(error.message.contains("host process"));
    }

    #[test]
    fn the_shared_private_descriptor_rule_is_the_one_admission_predicate() {
        assert!(descriptors_are_private_and_distinct(&[3, 4, 5, 6, 7]));
        assert!(!descriptors_are_private_and_distinct(&[0, 1, 2]));
        assert!(!descriptors_are_private_and_distinct(&[2, 3, 4]));
        assert!(!descriptors_are_private_and_distinct(&[3, 3]));
        assert!(!descriptors_are_private_and_distinct(&[-1]));
        assert!(!descriptors_are_private_and_distinct(&[
            ISOLATED_VISUAL_MAX_DESCRIPTOR + 1
        ]));
        assert!(descriptors_are_private_and_distinct(&[
            ISOLATED_VISUAL_FIRST_PRIVATE_DESCRIPTOR
        ]));
    }

    #[test]
    fn an_admitted_set_never_prints_descriptor_numbers() {
        let raw = IsolatedVisualLaunchDescriptors {
            process_id: 4_242,
            control: 31_337,
            event: 31_338,
            input: 31_339,
            frame: 31_340,
            challenge: 31_341,
        };
        let printed = format!("{:?}", raw.admit().unwrap());
        for needle in ["31337", "31338", "31339", "31340", "31341", "4242"] {
            assert!(!printed.contains(needle), "leaked {needle} in {printed}");
        }
        assert!(printed.contains("Challenge"));
    }

    // ---------- admission binds package, contract, and lease ----------

    #[test]
    fn admission_requires_the_receipt_to_match_the_manifest() {
        let mut drifted = contract();
        drifted.manifest.guest_image_sha256 = "9".repeat(64);
        let error = IsolatedVisualLaunchAuthority::admit(
            &drifted,
            &receipt(),
            descriptors().admit().unwrap(),
            IsolatedGuestLease::issue("agent-a").unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Unauthorized);
    }

    #[test]
    fn admission_requires_the_signing_requirement_to_match_the_manifest() {
        let mut forged_requirement = contract();
        forged_requirement
            .manifest
            .helper_signing_requirement_sha256 = "8".repeat(64);
        assert_eq!(
            IsolatedVisualLaunchAuthority::admit(
                &forged_requirement,
                &receipt(),
                descriptors().admit().unwrap(),
                IsolatedGuestLease::issue("agent-a").unwrap(),
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn admission_rejects_a_malformed_manifest_before_anything_else() {
        let mut bad_version = contract();
        bad_version.manifest.schema_version = 99;
        assert_eq!(
            IsolatedVisualLaunchAuthority::admit(
                &bad_version,
                &receipt(),
                descriptors().admit().unwrap(),
                IsolatedGuestLease::issue("agent-a").unwrap(),
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::InvalidRequest
        );

        let mut bridged = contract();
        bridged.manifest.security_profile.network_devices = 1;
        assert_eq!(
            IsolatedVisualLaunchAuthority::admit(
                &bridged,
                &receipt(),
                descriptors().admit().unwrap(),
                IsolatedGuestLease::issue("agent-a").unwrap(),
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );
    }

    #[test]
    fn admission_rejects_an_input_domain_that_is_not_independent() {
        let mut aliased = contract();
        aliased.input_domain_id = aliased.surface.surface_id().to_string();
        assert_eq!(
            IsolatedVisualLaunchAuthority::admit(
                &aliased,
                &receipt(),
                descriptors().admit().unwrap(),
                IsolatedGuestLease::issue("agent-a").unwrap(),
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::InvalidRequest
        );
    }

    // ---------- exact one agent, exact one guest ----------

    #[test]
    fn a_second_agent_is_denied_every_operation() {
        let (authority, lease, binding) = started();
        let other = IsolatedGuestLease::issue("agent-b").unwrap();
        for operation in [
            IsolatedVisualGuestOperation::Start,
            IsolatedVisualGuestOperation::ReadFrame,
            IsolatedVisualGuestOperation::WriteInput,
            IsolatedVisualGuestOperation::Stop,
            IsolatedVisualGuestOperation::Cleanup,
        ] {
            assert_eq!(
                authority
                    .authorize("agent-b", &other, &binding, operation)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction,
                "{operation:?} must be denied to a second agent"
            );
            // The right lease presented under the wrong agent name is also denied.
            assert_eq!(
                authority
                    .authorize("agent-b", &lease, &binding, operation)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction
            );
        }
    }

    #[test]
    fn a_stale_lease_revision_or_id_is_denied() {
        let (authority, lease, binding) = started();

        let mut stale = lease.clone();
        stale.revision = lease.revision + 1;
        assert_eq!(
            authority
                .authorize(
                    "agent-a",
                    &stale,
                    &binding,
                    IsolatedVisualGuestOperation::ReadFrame
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );

        let mut forged = lease.clone();
        forged.lease_id = "not-the-issued-lease".into();
        assert_eq!(
            authority
                .authorize(
                    "agent-a",
                    &forged,
                    &binding,
                    IsolatedVisualGuestOperation::ReadFrame
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn control_aimed_at_another_run_surface_or_input_domain_is_denied_first() {
        let (authority, lease, binding) = started();

        let mut other_run = binding.clone();
        other_run.run_id = "run-somewhere-else".into();
        let mut other_surface = binding.clone();
        other_surface.surface = ComputerSurfaceBinding {
            surface_id: "surface-other".into(),
            incarnation: "incarnation-other".into(),
        };
        let mut other_incarnation = binding.clone();
        other_incarnation.surface = ComputerSurfaceBinding {
            surface_id: binding.surface.surface_id().into(),
            incarnation: "incarnation-rotated".into(),
        };
        let mut other_domain = binding.clone();
        other_domain.input_domain_id = "input-other".into();

        for wrong in [other_run, other_surface, other_incarnation, other_domain] {
            assert_eq!(
                authority
                    .authorize(
                        "agent-a",
                        &lease,
                        &wrong,
                        IsolatedVisualGuestOperation::ReadFrame
                    )
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenTarget
            );
            // Binding is checked before the lease: a wrong guest never reports
            // on another guest's lease state.
            let stranger = IsolatedGuestLease::issue("agent-z").unwrap();
            assert_eq!(
                authority
                    .authorize(
                        "agent-z",
                        &stranger,
                        &wrong,
                        IsolatedVisualGuestOperation::ReadFrame
                    )
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenTarget
            );
        }
    }

    #[test]
    fn a_malformed_binding_is_rejected() {
        let (authority, lease, binding) = started();
        let mut malformed = binding.clone();
        malformed.run_id = "../escape".into();
        assert_eq!(
            authority
                .authorize(
                    "agent-a",
                    &lease,
                    &malformed,
                    IsolatedVisualGuestOperation::ReadFrame
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }

    // ---------- per-operation authority ----------

    #[test]
    fn each_state_authorizes_exactly_its_own_operations() {
        let all = [
            IsolatedVisualGuestOperation::Start,
            IsolatedVisualGuestOperation::ReadFrame,
            IsolatedVisualGuestOperation::WriteInput,
            IsolatedVisualGuestOperation::Stop,
            IsolatedVisualGuestOperation::Cleanup,
        ];

        let (authority, lease, binding) = authority();
        let allowed: Vec<_> = all
            .iter()
            .copied()
            .filter(|op| {
                authority
                    .authorize("agent-a", &lease, &binding, *op)
                    .is_ok()
            })
            .collect();
        assert_eq!(allowed, vec![IsolatedVisualGuestOperation::Start]);

        let (authority, lease, binding) = started();
        let allowed: Vec<_> = all
            .iter()
            .copied()
            .filter(|op| {
                authority
                    .authorize("agent-a", &lease, &binding, *op)
                    .is_ok()
            })
            .collect();
        assert_eq!(
            allowed,
            vec![
                IsolatedVisualGuestOperation::ReadFrame,
                IsolatedVisualGuestOperation::WriteInput,
                IsolatedVisualGuestOperation::Stop,
            ]
        );
    }

    #[test]
    fn start_is_not_replayable() {
        let (mut authority, lease, binding) = started();
        assert_eq!(
            authority
                .record_started("agent-a", &lease, &binding)
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidState
        );
    }

    #[test]
    fn frames_and_input_end_when_a_graceful_stop_begins() {
        let (mut authority, lease, binding) = started();
        authority.begin_stop("agent-a", &lease, &binding).unwrap();
        assert_eq!(authority.state(), IsolatedVisualAuthorityState::Stopping);
        assert_eq!(
            authority.revocation(),
            Some(IsolatedVisualRevocation::OperatorStop)
        );
        for denied in [
            IsolatedVisualGuestOperation::ReadFrame,
            IsolatedVisualGuestOperation::WriteInput,
            IsolatedVisualGuestOperation::Stop,
            IsolatedVisualGuestOperation::Start,
        ] {
            assert_eq!(
                authority
                    .authorize("agent-a", &lease, &binding, denied)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized,
                "{denied:?} must not survive a stop"
            );
        }
    }

    // ---------- helper loss, cancel race, no resume ----------

    #[test]
    fn helper_loss_revokes_authority_and_still_requires_cleanup() {
        let (mut authority, lease, binding) = started();
        authority
            .revoke(IsolatedVisualRevocation::HelperLoss)
            .unwrap();
        assert_eq!(authority.state(), IsolatedVisualAuthorityState::Revoked);
        assert_eq!(
            authority
                .authorize(
                    "agent-a",
                    &lease,
                    &binding,
                    IsolatedVisualGuestOperation::ReadFrame
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
        let receipt = authority
            .complete_cleanup(&evidence_for(&binding.surface))
            .unwrap();
        assert_eq!(receipt.disposition, IsolatedVisualRevocation::HelperLoss);
        assert_eq!(authority.state(), IsolatedVisualAuthorityState::Terminated);
    }

    #[test]
    fn a_cancel_racing_a_helper_death_is_counted_once() {
        let (mut authority, _lease, binding) = started();
        authority
            .revoke(IsolatedVisualRevocation::Cancelled)
            .unwrap();
        let second = authority
            .revoke(IsolatedVisualRevocation::HelperLoss)
            .unwrap_err();
        assert_eq!(second.code, ComputerErrorCode::Conflict);
        // The first reason wins; the race cannot rewrite the disposition.
        assert_eq!(
            authority.revocation(),
            Some(IsolatedVisualRevocation::Cancelled)
        );
        let receipt = authority
            .complete_cleanup(&evidence_for(&binding.surface))
            .unwrap();
        assert_eq!(receipt.disposition, IsolatedVisualRevocation::Cancelled);
    }

    #[test]
    fn revocation_is_terminal_and_never_resumes() {
        for reason in [
            IsolatedVisualRevocation::Cancelled,
            IsolatedVisualRevocation::HelperLoss,
            IsolatedVisualRevocation::RestartInterrupted,
            IsolatedVisualRevocation::LeaseRevoked,
        ] {
            let (mut authority, lease, binding) = started();
            authority.revoke(reason).unwrap();
            assert_eq!(
                authority
                    .record_started("agent-a", &lease, &binding)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized
            );
            assert_eq!(
                authority
                    .begin_stop("agent-a", &lease, &binding)
                    .unwrap_err()
                    .code,
                ComputerErrorCode::Unauthorized
            );
            // Re-presenting the original lease does not restore authority.
            assert!(authority
                .authorize(
                    "agent-a",
                    &lease,
                    &binding,
                    IsolatedVisualGuestOperation::WriteInput
                )
                .is_err());
        }
    }

    #[test]
    fn authority_can_be_revoked_before_the_guest_ever_started() {
        let (mut authority, _lease, binding) = authority();
        authority
            .revoke(IsolatedVisualRevocation::RestartInterrupted)
            .unwrap();
        let receipt = authority
            .complete_cleanup(&evidence_for(&binding.surface))
            .unwrap();
        assert_eq!(
            receipt.disposition,
            IsolatedVisualRevocation::RestartInterrupted
        );
    }

    // ---------- cleanup authority ----------

    #[test]
    fn cleanup_is_refused_while_authority_is_live() {
        let (mut authority, _lease, binding) = authority();
        assert_eq!(
            authority
                .complete_cleanup(&evidence_for(&binding.surface))
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidState
        );

        let (mut authority, _lease, binding) = started();
        assert_eq!(
            authority
                .complete_cleanup(&evidence_for(&binding.surface))
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidState
        );
    }

    #[test]
    fn cleanup_evidence_from_another_surface_is_refused() {
        let (mut authority, lease, binding) = started();
        authority.begin_stop("agent-a", &lease, &binding).unwrap();
        let foreign = ComputerSurfaceBinding {
            surface_id: "surface-elsewhere".into(),
            incarnation: "incarnation-elsewhere".into(),
        };
        assert_eq!(
            authority
                .complete_cleanup(&evidence_for(&foreign))
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenTarget
        );
        assert_eq!(authority.state(), IsolatedVisualAuthorityState::Stopping);
    }

    #[test]
    fn incomplete_cleanup_evidence_cannot_be_constructed_at_all() {
        let surface = contract().surface;
        for (helper, handles, overlay, cache) in [
            (false, true, true, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, false),
            (false, false, false, false),
        ] {
            assert!(
                IsolatedVisualCleanupEvidence::verified(
                    surface.clone(),
                    helper,
                    handles,
                    overlay,
                    cache
                )
                .is_err(),
                "incomplete cleanup ({helper},{handles},{overlay},{cache}) must fail closed"
            );
        }
    }

    #[test]
    fn cleanup_completes_exactly_once() {
        let (mut authority, lease, binding) = started();
        authority.begin_stop("agent-a", &lease, &binding).unwrap();
        authority
            .complete_cleanup(&evidence_for(&binding.surface))
            .unwrap();
        assert_eq!(
            authority
                .complete_cleanup(&evidence_for(&binding.surface))
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidState
        );
        assert_eq!(
            authority
                .revoke(IsolatedVisualRevocation::Cancelled)
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidState
        );
        for operation in [
            IsolatedVisualGuestOperation::Start,
            IsolatedVisualGuestOperation::ReadFrame,
            IsolatedVisualGuestOperation::WriteInput,
            IsolatedVisualGuestOperation::Stop,
            IsolatedVisualGuestOperation::Cleanup,
        ] {
            assert!(authority
                .authorize("agent-a", &lease, &binding, operation)
                .is_err());
        }
    }

    // ---------- deterministic, leak-free receipts ----------

    #[test]
    fn launch_receipts_are_deterministic_for_identical_launches() {
        let contract = contract();
        let lease = IsolatedGuestLease::issue("agent-a").unwrap();
        let build = || {
            IsolatedVisualLaunchAuthority::admit(
                &contract,
                &receipt(),
                descriptors().admit().unwrap(),
                lease.clone(),
            )
            .unwrap()
            .launch_receipt()
        };
        let first = build();
        let second = build();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&first).unwrap()
        );
    }

    #[test]
    fn launch_receipts_carry_package_identity_and_track_state() {
        let (mut authority, lease, binding) = authority();
        let admitted = authority.launch_receipt();
        assert_eq!(
            admitted.schema_version,
            ISOLATED_VISUAL_LAUNCH_RECEIPT_SCHEMA_VERSION
        );
        assert_eq!(
            admitted.backend_id,
            MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID
        );
        assert_eq!(admitted.helper_content_sha256, HELPER_DIGEST);
        assert_eq!(
            admitted.helper_signing_requirement_sha256,
            REQUIREMENT_DIGEST
        );
        assert_eq!(admitted.guest_image_sha256, GUEST_DIGEST);
        assert_eq!(admitted.configuration_sha256, CONFIG_DIGEST);
        assert_eq!(admitted.channels, IsolatedVisualChannelRole::ALL.to_vec());
        assert_eq!(admitted.state, IsolatedVisualAuthorityState::Admitted);

        authority
            .record_started("agent-a", &lease, &binding)
            .unwrap();
        assert_eq!(
            authority.launch_receipt().state,
            IsolatedVisualAuthorityState::Started
        );
        authority
            .revoke(IsolatedVisualRevocation::HelperLoss)
            .unwrap();
        assert_eq!(
            authority.launch_receipt().state,
            IsolatedVisualAuthorityState::Revoked
        );
    }

    #[test]
    fn receipts_carry_no_secret_path_descriptor_or_lease_needle() {
        let (mut authority, lease, binding) = started();
        let launch = serde_json::to_string(&authority.launch_receipt()).unwrap();
        authority.begin_stop("agent-a", &lease, &binding).unwrap();
        let cleanup = serde_json::to_string(
            &authority
                .complete_cleanup(&evidence_for(&binding.surface))
                .unwrap(),
        )
        .unwrap();

        for projection in [&launch, &cleanup] {
            let lowered = projection.to_ascii_lowercase();
            for needle in [
                &lease.lease_id as &str,
                "leaseid",
                "lease_id",
                "channelsecret",
                "challengebytes",
                "secret",
                "password",
                "token=",
                "apikey",
                "/users/",
                "/private/",
                "/home/",
                "/var/",
                "descriptor",
                "controlfd",
                "processid",
                "pid",
                "framebytes",
                "clipboard",
            ] {
                assert!(
                    !lowered.contains(&needle.to_ascii_lowercase()),
                    "projection leaked {needle}: {projection}"
                );
            }
        }
        // Role names are public identity and must stay: the receipt says which
        // channels a launch had, never what flowed through them.
        assert!(launch.contains("\"challenge\""));
        assert!(launch.contains("\"frame\""));
        // What it does carry is identity, not capability.
        assert!(launch.contains("agent-a"));
        assert!(launch.contains(HELPER_DIGEST));
        assert!(!launch.contains(&lease.lease_id));
    }

    #[test]
    fn cleanup_receipts_report_every_completed_check() {
        let (mut authority, lease, binding) = started();
        authority.begin_stop("agent-a", &lease, &binding).unwrap();
        let receipt = authority
            .complete_cleanup(&evidence_for(&binding.surface))
            .unwrap();
        assert!(receipt.helper_process_absent);
        assert!(receipt.no_open_handles);
        assert!(receipt.overlay_removed);
        assert!(receipt.frame_cache_removed);
        assert!(receipt.lease_revoked);
        assert_eq!(
            receipt.channels_released,
            ISOLATED_VISUAL_LAUNCH_CHANNEL_COUNT
        );
        assert_eq!(receipt.run_id, binding.run_id);
        assert_eq!(receipt.surface_id, binding.surface.surface_id());
        assert_eq!(receipt.incarnation, binding.surface.incarnation());
        assert_eq!(receipt.disposition, IsolatedVisualRevocation::OperatorStop);
        assert_eq!(
            serde_json::to_string(&receipt).unwrap(),
            serde_json::to_string(&receipt).unwrap()
        );
    }

    #[test]
    fn an_authority_never_prints_its_live_lease() {
        let (authority, lease, _binding) = started();
        let printed = format!("{authority:?}");
        assert!(!printed.contains(&lease.lease_id), "{printed}");
        assert!(printed.contains("lease_held: true"), "{printed}");
        assert!(printed.contains("agent-a"));
    }
}
