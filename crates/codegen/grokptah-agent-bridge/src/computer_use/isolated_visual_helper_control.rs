use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::Duration;

use super::isolated_visual::IsolatedVisualTerminalDisposition;
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, ISOLATED_VISUAL_HELPER_EVENT_BYTES,
};
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

pub const ISOLATED_VISUAL_CHALLENGE_BYTES: usize = 32;

/// Host-side adapter for the helper's inherited private control/event pipes.
///
/// The adapter does not spawn the helper or own its descriptors. It only
/// serializes controls returned by [`IsolatedVisualRuntimeSession`] and feeds
/// fixed-size helper events back into that coordinator. A real supervisor can
/// wrap the helper's FDs without reimplementing the state machine.
pub struct IsolatedVisualHelperControl<R, W> {
    event_reader: R,
    control_writer: W,
}

impl<R, W> IsolatedVisualHelperControl<R, W> {
    pub fn new(event_reader: R, control_writer: W) -> Self {
        Self {
            event_reader,
            control_writer,
        }
    }

    pub fn into_parts(self) -> (R, W) {
        (self.event_reader, self.control_writer)
    }
}

impl<R: Read, W: Write> IsolatedVisualHelperControl<R, W> {
    /// Reads and applies one fixed-size helper event. Unknown, truncated, or
    /// reordered events fail before any later control is emitted.
    pub fn receive_event(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
    ) -> ComputerResult<()> {
        let mut bytes = [0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
        read_exact(&mut self.event_reader, &mut bytes)?;
        let event = IsolatedVisualHelperEvent::decode(&bytes)?;
        runtime.accept_helper_event(event)
    }

    /// Waits for one helper event without allowing a dead helper to hold the
    /// supervisor forever. The underlying read remains exact and ordered;
    /// polling only supplies the lifecycle timeout.
    pub fn receive_event_with_timeout(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
        timeout: Duration,
    ) -> ComputerResult<()>
    where
        R: AsRawFd,
    {
        let timeout_millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: self.event_reader.as_raw_fd(),
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: the descriptor is borrowed from the supervisor-owned event
        // reader and remains valid for the duration of this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if ready == 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated helper event timed out",
            ));
        }
        if ready < 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated helper event poll failed",
            ));
        }
        self.receive_event(runtime)
    }

    /// Sends the exact start byte after the helper has reported Prepared.
    pub fn send_start(&mut self, runtime: &mut IsolatedVisualRuntimeSession) -> ComputerResult<()> {
        let control = runtime.start_control()?;
        self.write_control(&[control])
    }

    /// Sends the length-prefixed authenticated binding frame. The helper must
    /// return a Bound event before frame/input traffic or stop is allowed.
    pub fn send_binding(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
    ) -> ComputerResult<()> {
        let control = runtime.bind_control()?;
        self.write_control(&control)
    }

    /// Sends the exact stop byte after the guest binding acknowledgement.
    pub fn send_stop(
        &mut self,
        runtime: &mut IsolatedVisualRuntimeSession,
        disposition: IsolatedVisualTerminalDisposition,
    ) -> ComputerResult<()> {
        let control = runtime.stop_control(disposition)?;
        self.write_control(&[control])
    }

    fn write_control(&mut self, bytes: &[u8]) -> ComputerResult<()> {
        self.control_writer.write_all(bytes).map_err(stream_error)?;
        self.control_writer.flush().map_err(stream_error)
    }
}

fn read_exact<R: Read>(reader: &mut R, bytes: &mut [u8]) -> ComputerResult<()> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ComputerError::new(
                ComputerErrorCode::TargetClosed,
                "isolated helper event pipe closed mid-event",
            )
        } else {
            stream_error(error)
        }
    })
}

fn stream_error(error: io::Error) -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::BackendFailure,
        format!("isolated helper control I/O failed: {error}"),
    )
}

/// Reads the helper-generated per-launch challenge from its private inherited
/// channel. The challenge is held only by the host runtime coordinator and is
/// never serialized into a model/provider projection.
pub fn read_isolated_visual_challenge<R: Read>(reader: &mut R) -> ComputerResult<[u8; 32]> {
    let mut challenge = [0_u8; ISOLATED_VISUAL_CHALLENGE_BYTES];
    read_exact(reader, &mut challenge)?;
    if challenge.iter().all(|byte| *byte == 0) {
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "isolated helper returned an empty guest challenge",
        ));
    }
    Ok(challenge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn truncated_helper_event_is_terminal() {
        let mut control =
            IsolatedVisualHelperControl::new(Cursor::new(vec![0x47, 0x50]), Vec::<u8>::new());
        let mut bytes = [0_u8; ISOLATED_VISUAL_HELPER_EVENT_BYTES];
        let error = read_exact(&mut control.event_reader, &mut bytes).unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::TargetClosed);
    }

    #[test]
    fn challenge_reader_requires_a_complete_nonzero_challenge() {
        let mut complete = Cursor::new(vec![7_u8; ISOLATED_VISUAL_CHALLENGE_BYTES]);
        assert_eq!(
            read_isolated_visual_challenge(&mut complete).unwrap(),
            [7_u8; ISOLATED_VISUAL_CHALLENGE_BYTES]
        );
        let mut truncated = Cursor::new(vec![7_u8; 31]);
        assert!(read_isolated_visual_challenge(&mut truncated).is_err());
        let mut empty = Cursor::new(vec![0_u8; ISOLATED_VISUAL_CHALLENGE_BYTES]);
        assert!(read_isolated_visual_challenge(&mut empty).is_err());
    }
}
