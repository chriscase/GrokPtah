use std::fmt;

use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualLifecycle,
    IsolatedVisualLifecycleState, IsolatedVisualTerminalDisposition,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::isolated_visual_frames::{IsolatedVisualFrame, IsolatedVisualFrameCarrier};
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, IsolatedVisualHelperEventCode, IsolatedVisualHelperSupervisor,
    IsolatedVisualHelperSupervisorState,
};
use super::isolated_visual_input::{IsolatedVisualInputGate, IsolatedVisualInputMessage};
use super::isolated_visual_input_wire::IsolatedVisualInputWire;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

/// Host-owned coordinator for the authenticated isolated visual runtime seam.
///
/// This object deliberately does not spawn a process or enable a capability.
/// It joins the lifecycle, helper control ABI, binding acknowledgement, frame
/// carrier, and input gate so the packaged supervisor has one stateful contract
/// to drive. Frame bytes and channel secrets never cross its public
/// debug/projection boundary.
pub struct IsolatedVisualRuntimeSession {
    lifecycle: IsolatedVisualLifecycle,
    helper: IsolatedVisualHelperSupervisor,
    binding: IsolatedVisualChannelBinding,
    challenge: [u8; 32],
    frame_carrier: Option<IsolatedVisualFrameCarrier>,
    input_wire: Option<IsolatedVisualInputWire>,
    input_gate: IsolatedVisualInputGate,
}

impl fmt::Debug for IsolatedVisualRuntimeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedVisualRuntimeSession")
            .field("lifecycle", &self.lifecycle)
            .field("helper", &self.helper)
            .field("binding", &self.binding)
            .field("challenge", &"[REDACTED]")
            .field("frame_carrier_ready", &self.frame_carrier.is_some())
            .field("input_wire_ready", &self.input_wire.is_some())
            .field("input_gate", &self.input_gate)
            .finish()
    }
}

impl IsolatedVisualRuntimeSession {
    pub fn new(
        contract: IsolatedVisualLaunchContract,
        challenge: [u8; 32],
    ) -> ComputerResult<Self> {
        let lifecycle = IsolatedVisualLifecycle::new(contract.clone())?;
        let binding = IsolatedVisualChannelBinding::from_contract(&contract)?;
        let input_gate = IsolatedVisualInputGate::new(contract.manifest.limits.clone())?;
        Ok(Self {
            lifecycle,
            helper: IsolatedVisualHelperSupervisor::new(),
            binding,
            challenge,
            frame_carrier: None,
            input_wire: None,
            input_gate,
        })
    }

    pub fn lifecycle(&self) -> &IsolatedVisualLifecycle {
        &self.lifecycle
    }

    pub fn lifecycle_state(&self) -> IsolatedVisualLifecycleState {
        self.lifecycle.state()
    }

    pub fn helper_state(&self) -> IsolatedVisualHelperSupervisorState {
        self.helper.state()
    }

    pub fn input_frame_sequence(&self) -> u64 {
        self.input_gate.frame_sequence()
    }

    pub fn input_sequence(&self) -> u64 {
        self.input_gate.next_input_sequence()
    }

    /// Returns the private start byte after the helper has emitted Prepared.
    pub fn start_control(&mut self) -> ComputerResult<u8> {
        let control = self.helper.start()?;
        self.lifecycle.begin_start()?;
        Ok(control)
    }

    /// Accepts one authenticated helper event and advances the coupled
    /// lifecycle. The caller must provide the exact fixed-size event decoded
    /// by [`IsolatedVisualHelperEvent::decode`].
    pub fn accept_helper_event(&mut self, event: IsolatedVisualHelperEvent) -> ComputerResult<()> {
        let code = event.code;
        if code == IsolatedVisualHelperEventCode::Running
            && self.lifecycle_state() != IsolatedVisualLifecycleState::Starting
        {
            return Err(runtime_order_error("running event arrived before start"));
        }
        if code == IsolatedVisualHelperEventCode::Bound
            && self.lifecycle_state() != IsolatedVisualLifecycleState::ReadOnlyReady
        {
            return Err(runtime_order_error(
                "bound event arrived before guest readiness",
            ));
        }
        if code == IsolatedVisualHelperEventCode::Stopped
            && self.lifecycle_state() != IsolatedVisualLifecycleState::Stopping
        {
            return Err(runtime_order_error("stopped event arrived before stop"));
        }

        self.helper.accept_event(event)?;
        match code {
            IsolatedVisualHelperEventCode::Running => self.lifecycle.mark_read_only_ready(),
            IsolatedVisualHelperEventCode::Bound => {
                self.frame_carrier = Some(IsolatedVisualFrameCarrier::new_host_with_challenge(
                    self.lifecycle.contract(),
                    &self.challenge,
                )?);
                self.input_wire = Some(IsolatedVisualInputWire::new_host_with_challenge(
                    self.lifecycle.contract(),
                    &self.challenge,
                )?);
                Ok(())
            }
            IsolatedVisualHelperEventCode::Stopped => self.lifecycle.require_cleanup(),
            IsolatedVisualHelperEventCode::Failure => {
                self.input_gate.poison();
                self.lifecycle.fail()
            }
            IsolatedVisualHelperEventCode::Prepared => Ok(()),
        }
    }

    /// Creates the private helper control frame carrying the exact run
    /// binding. The helper must later emit `Bound` before frame/input traffic
    /// or terminal stop is accepted.
    pub fn bind_control(&mut self) -> ComputerResult<Vec<u8>> {
        if self.lifecycle_state() != IsolatedVisualLifecycleState::ReadOnlyReady {
            return Err(runtime_order_error("guest binding is not allowed yet"));
        }
        self.helper.bind(&self.binding, &self.challenge)
    }

    /// Opens one authenticated frame chunk. Once a complete frame arrives it
    /// becomes the sole freshness fence for subsequent input admission.
    pub fn open_frame_chunk(
        &mut self,
        encoded: &[u8],
    ) -> ComputerResult<Option<IsolatedVisualFrame>> {
        if self.helper_state() != IsolatedVisualHelperSupervisorState::Bound {
            return Err(runtime_order_error("frame traffic requires a bound guest"));
        }
        let frame = self
            .frame_carrier
            .as_mut()
            .ok_or_else(|| runtime_order_error("frame carrier is not initialized"))?
            .open_chunk(encoded)?;
        if let Some(frame) = frame.as_ref() {
            if let Err(error) =
                self.input_gate
                    .bind_frame(frame.frame_sequence, frame.width, frame.height)
            {
                self.input_gate.poison();
                return Err(error);
            }
        }
        Ok(frame)
    }

    /// Seals one host-to-guest input packet only against the latest complete
    /// frame and the challenge-derived channel key.
    pub fn seal_input(
        &mut self,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<Vec<u8>> {
        if self.helper_state() != IsolatedVisualHelperSupervisorState::Bound {
            return Err(runtime_order_error("input traffic requires a bound guest"));
        }
        let frame_sequence = self.input_gate.frame_sequence();
        self.input_wire
            .as_ref()
            .ok_or_else(|| runtime_order_error("input wire is not initialized"))?
            .seal(
                &mut self.input_gate,
                frame_sequence,
                input_sequence,
                request_nonce,
                message,
            )
    }

    /// Returns the private stop byte only after the guest binding has been
    /// acknowledged. Lifecycle cleanup remains a separate mandatory step.
    pub fn stop_control(
        &mut self,
        disposition: IsolatedVisualTerminalDisposition,
    ) -> ComputerResult<u8> {
        self.input_gate.terminal_check()?;
        let control = self.helper.stop()?;
        self.lifecycle.begin_stop(disposition)?;
        Ok(control)
    }

    pub fn interrupt_on_restart(&mut self) -> ComputerResult<()> {
        self.input_gate.poison();
        self.lifecycle.interrupt_on_restart()
    }

    pub fn terminal_check(&self) -> ComputerResult<()> {
        self.input_gate.terminal_check()
    }

    pub(crate) fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        self.lifecycle.complete_cleanup(evidence)
    }

    /// Completes cleanup from host-observed process/handle/overlay/cache facts.
    /// Evidence construction stays crate-private so a coordinator cannot
    /// manufacture terminal authority from serialized booleans.
    pub fn complete_observed_cleanup(
        &mut self,
        helper_process_absent: bool,
        no_open_handles: bool,
        overlay_removed: bool,
        frame_cache_removed: bool,
    ) -> ComputerResult<()> {
        let evidence = IsolatedVisualCleanupEvidence::verified(
            self.lifecycle.contract().surface.clone(),
            helper_process_absent,
            no_open_handles,
            overlay_removed,
            frame_cache_removed,
        )?;
        self.complete_cleanup(&evidence)
    }

    /// Records helper/runtime failure without skipping cleanup evidence.
    pub fn fail(&mut self) -> ComputerResult<()> {
        self.input_gate.poison();
        self.lifecycle.fail()
    }
}

fn runtime_order_error(message: &'static str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::InvalidState, message)
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual::{
        IsolatedVisualManifest, IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
    };
    use super::super::types::ComputerSurfaceBinding;
    use super::*;

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: "run-runtime-session".into(),
            surface: ComputerSurfaceBinding {
                surface_id: "surface-runtime".into(),
                incarnation: "incarnation-runtime".into(),
            },
            input_domain_id: "input-runtime".into(),
            manifest: IsolatedVisualManifest {
                schema_version: 1,
                backend_id: "macos_isolated_visual_candidate_v1".into(),
                guest_protocol_version: 1,
                helper_content_sha256: "a".repeat(64),
                helper_signing_requirement_sha256: "b".repeat(64),
                guest_image_sha256: "c".repeat(64),
                configuration_sha256: "d".repeat(64),
                security_profile: IsolatedVisualSecurityProfile::locked_down(),
                limits: IsolatedVisualResourceLimits::proof_defaults(),
            },
        }
    }

    const REQUEST_NONCE: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn runtime_requires_binding_before_channels_and_stop() {
        let contract = contract();
        let mut runtime = IsolatedVisualRuntimeSession::new(contract.clone(), [7; 32]).unwrap();
        assert!(runtime.start_control().is_err());
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(1, 0)).unwrap())
            .unwrap();
        runtime.start_control().unwrap();
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(2, 0)).unwrap())
            .unwrap();
        assert!(runtime
            .stop_control(IsolatedVisualTerminalDisposition::Cancelled)
            .is_err());
        runtime.bind_control().unwrap();
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(5, 0)).unwrap())
            .unwrap();
        let mut guest =
            IsolatedVisualFrameCarrier::new_guest_with_challenge(&contract, &[7; 32]).unwrap();
        let chunks = guest
            .seal_frame(1, REQUEST_NONCE, 2, 2, &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(
            runtime.open_frame_chunk(&chunks[0]).unwrap().unwrap().bytes,
            vec![1, 2, 3, 4]
        );
        let packet = runtime
            .seal_input(
                1,
                REQUEST_NONCE,
                IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },
            )
            .unwrap();
        assert!(!packet.is_empty());
        runtime
            .stop_control(IsolatedVisualTerminalDisposition::Cancelled)
            .unwrap();
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(3, 0)).unwrap())
            .unwrap();
        assert_eq!(
            runtime.lifecycle_state(),
            IsolatedVisualLifecycleState::CleanupPending
        );
        runtime.terminal_check().unwrap();
    }

    #[test]
    fn runtime_rejects_input_before_binding() {
        let contract = contract();
        let mut runtime = IsolatedVisualRuntimeSession::new(contract, [7; 32]).unwrap();
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(1, 0)).unwrap())
            .unwrap();
        runtime.start_control().unwrap();
        runtime
            .accept_helper_event(IsolatedVisualHelperEvent::decode(&event(2, 0)).unwrap())
            .unwrap();
        assert!(runtime
            .seal_input(
                1,
                REQUEST_NONCE,
                IsolatedVisualInputMessage::Text { text: "x".into() }
            )
            .is_err());
    }

    fn event(code: u16, detail: u32) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[0..4].copy_from_slice(&0x4750_5449u32.to_be_bytes());
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        bytes[6..8].copy_from_slice(&code.to_be_bytes());
        bytes[8..12].copy_from_slice(&detail.to_be_bytes());
        bytes
    }
}
