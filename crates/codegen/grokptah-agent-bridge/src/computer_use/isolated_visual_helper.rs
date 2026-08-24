use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

/// The helper event ABI is deliberately smaller than the model-facing
/// protocol. It is a fixed-size, network-byte-order carrier between the
/// future host supervisor and the signed Virtualization helper.
pub const ISOLATED_VISUAL_HELPER_EVENT_MAGIC: u32 = 0x4750_5449;
pub const ISOLATED_VISUAL_HELPER_EVENT_VERSION: u16 = 1;
pub const ISOLATED_VISUAL_HELPER_EVENT_BYTES: usize = 16;
pub const ISOLATED_VISUAL_HELPER_CONTROL_START: u8 = 1;
pub const ISOLATED_VISUAL_HELPER_CONTROL_STOP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum IsolatedVisualHelperEventCode {
    Prepared = 1,
    Running = 2,
    Stopped = 3,
    Failure = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IsolatedVisualHelperFailure {
    InvalidInvocation = 1,
    InvalidDescriptor = 2,
    InvalidConfiguration = 3,
    StartNotAuthorized = 4,
    VirtualizationUnavailable = 5,
    ConfigurationRejected = 6,
    StartFailed = 7,
    ControlLost = 8,
    StopFailed = 9,
    GuestStopped = 10,
    GuestProtocol = 11,
}

impl IsolatedVisualHelperFailure {
    fn from_wire(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::InvalidInvocation,
            2 => Self::InvalidDescriptor,
            3 => Self::InvalidConfiguration,
            4 => Self::StartNotAuthorized,
            5 => Self::VirtualizationUnavailable,
            6 => Self::ConfigurationRejected,
            7 => Self::StartFailed,
            8 => Self::ControlLost,
            9 => Self::StopFailed,
            10 => Self::GuestStopped,
            11 => Self::GuestProtocol,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolatedVisualHelperEvent {
    pub code: IsolatedVisualHelperEventCode,
    pub failure: Option<IsolatedVisualHelperFailure>,
}

impl IsolatedVisualHelperEvent {
    pub fn decode(bytes: &[u8]) -> ComputerResult<Self> {
        if bytes.len() != ISOLATED_VISUAL_HELPER_EVENT_BYTES {
            return Err(invalid_event("helper event has an invalid length"));
        }
        let magic = u32::from_be_bytes(bytes[0..4].try_into().expect("length checked"));
        let version = u16::from_be_bytes(bytes[4..6].try_into().expect("length checked"));
        let code = u16::from_be_bytes(bytes[6..8].try_into().expect("length checked"));
        let detail = u32::from_be_bytes(bytes[8..12].try_into().expect("length checked"));
        let reserved = u32::from_be_bytes(bytes[12..16].try_into().expect("length checked"));
        if magic != ISOLATED_VISUAL_HELPER_EVENT_MAGIC
            || version != ISOLATED_VISUAL_HELPER_EVENT_VERSION
            || reserved != 0
        {
            return Err(invalid_event("helper event header is invalid"));
        }
        let code = match code {
            1 => IsolatedVisualHelperEventCode::Prepared,
            2 => IsolatedVisualHelperEventCode::Running,
            3 => IsolatedVisualHelperEventCode::Stopped,
            4 => IsolatedVisualHelperEventCode::Failure,
            _ => return Err(invalid_event("helper event code is unknown")),
        };
        let failure = match code {
            IsolatedVisualHelperEventCode::Failure => Some(
                IsolatedVisualHelperFailure::from_wire(detail)
                    .ok_or_else(|| invalid_event("helper failure code is unknown"))?,
            ),
            _ if detail == 0 => None,
            _ => return Err(invalid_event("non-failure helper event has a detail")),
        };
        Ok(Self { code, failure })
    }
}

fn invalid_event(message: &'static str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::InvalidRequest, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolatedVisualHelperSupervisorState {
    AwaitingPrepared,
    Prepared,
    StartSent,
    Running,
    StopSent,
    Stopped,
    Failed(IsolatedVisualHelperFailure),
}

/// Host-side state machine for the fixed helper ABI. This does not spawn a
/// process or grant a capability; it makes the eventual supervisor's event
/// ordering and control intent testable before a packaged runtime exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolatedVisualHelperSupervisor {
    state: IsolatedVisualHelperSupervisorState,
}

impl Default for IsolatedVisualHelperSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl IsolatedVisualHelperSupervisor {
    pub const fn new() -> Self {
        Self {
            state: IsolatedVisualHelperSupervisorState::AwaitingPrepared,
        }
    }

    pub fn state(&self) -> IsolatedVisualHelperSupervisorState {
        self.state
    }

    pub fn start(&mut self) -> ComputerResult<u8> {
        if self.state != IsolatedVisualHelperSupervisorState::Prepared {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated helper start is not allowed in the current state",
            ));
        }
        self.state = IsolatedVisualHelperSupervisorState::StartSent;
        Ok(ISOLATED_VISUAL_HELPER_CONTROL_START)
    }

    pub fn stop(&mut self) -> ComputerResult<u8> {
        if self.state != IsolatedVisualHelperSupervisorState::Running {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated helper stop is not allowed in the current state",
            ));
        }
        self.state = IsolatedVisualHelperSupervisorState::StopSent;
        Ok(ISOLATED_VISUAL_HELPER_CONTROL_STOP)
    }

    pub fn accept_event(&mut self, event: IsolatedVisualHelperEvent) -> ComputerResult<()> {
        match event.code {
            IsolatedVisualHelperEventCode::Failure => {
                let failure = event.failure.ok_or_else(|| {
                    invalid_event("helper failure event is missing its failure code")
                })?;
                if matches!(
                    self.state,
                    IsolatedVisualHelperSupervisorState::Stopped
                        | IsolatedVisualHelperSupervisorState::Failed(_)
                ) {
                    return Err(invalid_event("helper emitted an event after termination"));
                }
                self.state = IsolatedVisualHelperSupervisorState::Failed(failure);
            }
            IsolatedVisualHelperEventCode::Prepared
                if self.state == IsolatedVisualHelperSupervisorState::AwaitingPrepared =>
            {
                self.state = IsolatedVisualHelperSupervisorState::Prepared;
            }
            IsolatedVisualHelperEventCode::Running
                if self.state == IsolatedVisualHelperSupervisorState::StartSent =>
            {
                self.state = IsolatedVisualHelperSupervisorState::Running;
            }
            IsolatedVisualHelperEventCode::Stopped
                if self.state == IsolatedVisualHelperSupervisorState::StopSent =>
            {
                self.state = IsolatedVisualHelperSupervisorState::Stopped;
            }
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated helper event violates the host control order",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: u16, detail: u32) -> [u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES] {
        let mut bytes = [0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
        bytes[0..4].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_MAGIC.to_be_bytes());
        bytes[4..6].copy_from_slice(&ISOLATED_VISUAL_HELPER_EVENT_VERSION.to_be_bytes());
        bytes[6..8].copy_from_slice(&code.to_be_bytes());
        bytes[8..12].copy_from_slice(&detail.to_be_bytes());
        bytes
    }

    #[test]
    fn decodes_closed_network_order_event_abi() {
        let prepared = IsolatedVisualHelperEvent::decode(&event(1, 0)).unwrap();
        assert_eq!(prepared.code, IsolatedVisualHelperEventCode::Prepared);
        assert_eq!(prepared.failure, None);
        let failure = IsolatedVisualHelperEvent::decode(&event(4, 11)).unwrap();
        assert_eq!(
            failure.failure,
            Some(IsolatedVisualHelperFailure::GuestProtocol)
        );
    }

    #[test]
    fn rejects_header_unknown_code_and_detail_variants() {
        let mut bad_magic = event(1, 0);
        bad_magic[0] = 0;
        assert!(IsolatedVisualHelperEvent::decode(&bad_magic).is_err());
        let mut bad_version = event(1, 0);
        bad_version[5] = 2;
        assert!(IsolatedVisualHelperEvent::decode(&bad_version).is_err());
        let mut bad_reserved = event(1, 0);
        bad_reserved[12] = 1;
        assert!(IsolatedVisualHelperEvent::decode(&bad_reserved).is_err());
        assert!(IsolatedVisualHelperEvent::decode(&[0; 15]).is_err());
        assert!(IsolatedVisualHelperEvent::decode(&event(99, 0)).is_err());
        assert!(IsolatedVisualHelperEvent::decode(&event(1, 1)).is_err());
        assert!(IsolatedVisualHelperEvent::decode(&event(4, 99)).is_err());
    }

    #[test]
    fn supervisor_requires_prepared_start_running_stop_stopped() {
        let mut supervisor = IsolatedVisualHelperSupervisor::new();
        assert_eq!(
            supervisor.start().unwrap_err().code,
            ComputerErrorCode::Conflict
        );
        supervisor
            .accept_event(IsolatedVisualHelperEvent::decode(&event(1, 0)).unwrap())
            .unwrap();
        assert_eq!(
            supervisor.start().unwrap(),
            ISOLATED_VISUAL_HELPER_CONTROL_START
        );
        assert_eq!(
            supervisor
                .accept_event(IsolatedVisualHelperEvent::decode(&event(3, 0)).unwrap())
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        supervisor
            .accept_event(IsolatedVisualHelperEvent::decode(&event(2, 0)).unwrap())
            .unwrap();
        assert_eq!(
            supervisor.stop().unwrap(),
            ISOLATED_VISUAL_HELPER_CONTROL_STOP
        );
        supervisor
            .accept_event(IsolatedVisualHelperEvent::decode(&event(3, 0)).unwrap())
            .unwrap();
        assert_eq!(
            supervisor.state(),
            IsolatedVisualHelperSupervisorState::Stopped
        );
    }

    #[test]
    fn failure_is_terminal_and_preserves_the_closed_code() {
        let mut supervisor = IsolatedVisualHelperSupervisor::new();
        supervisor
            .accept_event(IsolatedVisualHelperEvent::decode(&event(4, 2)).unwrap())
            .unwrap();
        assert_eq!(
            supervisor.state(),
            IsolatedVisualHelperSupervisorState::Failed(
                IsolatedVisualHelperFailure::InvalidDescriptor
            )
        );
        assert!(supervisor
            .accept_event(IsolatedVisualHelperEvent::decode(&event(1, 0)).unwrap())
            .is_err());
    }
}
