use std::io::{self, Read, Write};

use super::isolated_visual::IsolatedVisualTerminalDisposition;
use super::isolated_visual_helper::{
    IsolatedVisualHelperEvent, ISOLATED_VISUAL_HELPER_EVENT_BYTES,
};
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

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
}
