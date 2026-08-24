use super::isolated_visual::{IsolatedVisualCleanupEvidence, IsolatedVisualTerminalDisposition};
use super::isolated_visual_frames::IsolatedVisualFrame;
use super::isolated_visual_helper_control::IsolatedVisualHelperControl;
use super::isolated_visual_input::IsolatedVisualInputMessage;
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::isolated_visual_stream::IsolatedVisualStream;
use super::types::ComputerResult;
use std::os::fd::AsRawFd;
use std::time::Duration;

/// The single host-side seam the packaged supervisor drives.
///
/// The driver deliberately accepts inherited readers/writers instead of
/// opening a process or socket. That keeps descriptor ownership in the
/// platform supervisor while making it impossible for callers to advance the
/// helper lifecycle, frame freshness, and authenticated input state through
/// unrelated coordinators.
pub struct IsolatedVisualRuntimeDriver<ER, CW, FR, IW> {
    runtime: IsolatedVisualRuntimeSession,
    helper: IsolatedVisualHelperControl<ER, CW>,
    stream: IsolatedVisualStream<FR, IW>,
}

impl<ER, CW, FR, IW> IsolatedVisualRuntimeDriver<ER, CW, FR, IW> {
    pub fn new(
        runtime: IsolatedVisualRuntimeSession,
        helper: IsolatedVisualHelperControl<ER, CW>,
        stream: IsolatedVisualStream<FR, IW>,
    ) -> Self {
        Self {
            runtime,
            helper,
            stream,
        }
    }

    pub fn runtime(&self) -> &IsolatedVisualRuntimeSession {
        &self.runtime
    }

    pub fn start(&mut self) -> ComputerResult<()> {
        self.helper.send_start(&mut self.runtime)
    }

    pub fn start_with_timeout(&mut self, timeout: Duration) -> ComputerResult<()>
    where
        CW: AsRawFd,
    {
        self.helper
            .send_start_with_timeout(&mut self.runtime, timeout)
    }

    pub fn receive_helper_event(&mut self) -> ComputerResult<()> {
        self.helper.receive_event(&mut self.runtime)
    }

    pub fn receive_helper_event_with_timeout(&mut self, timeout: Duration) -> ComputerResult<()>
    where
        ER: AsRawFd,
    {
        self.helper
            .receive_event_with_timeout(&mut self.runtime, timeout)
    }

    pub fn bind(&mut self) -> ComputerResult<()> {
        self.helper.send_binding(&mut self.runtime)
    }

    pub fn bind_with_timeout(&mut self, timeout: Duration) -> ComputerResult<()>
    where
        CW: AsRawFd,
    {
        self.helper
            .send_binding_with_timeout(&mut self.runtime, timeout)
    }

    pub fn read_frame(&mut self) -> ComputerResult<Option<IsolatedVisualFrame>> {
        self.stream.read_frame_chunk(&mut self.runtime)
    }

    pub fn read_frame_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> ComputerResult<Option<IsolatedVisualFrame>>
    where
        FR: AsRawFd,
    {
        self.stream
            .read_frame_chunk_with_timeout(&mut self.runtime, timeout)
    }

    pub fn write_input(
        &mut self,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<()> {
        self.stream
            .write_input(&mut self.runtime, input_sequence, request_nonce, message)
    }

    pub fn write_input_with_timeout(
        &mut self,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
        timeout: Duration,
    ) -> ComputerResult<()>
    where
        IW: AsRawFd,
    {
        self.stream.write_input_with_timeout(
            &mut self.runtime,
            input_sequence,
            request_nonce,
            message,
            timeout,
        )
    }

    pub fn stop(&mut self, disposition: IsolatedVisualTerminalDisposition) -> ComputerResult<()> {
        self.helper.send_stop(&mut self.runtime, disposition)
    }

    pub fn stop_with_timeout(
        &mut self,
        disposition: IsolatedVisualTerminalDisposition,
        timeout: Duration,
    ) -> ComputerResult<()>
    where
        CW: AsRawFd,
    {
        self.helper
            .send_stop_with_timeout(&mut self.runtime, disposition, timeout)
    }

    pub(crate) fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        self.runtime.complete_cleanup(evidence)
    }

    pub(crate) fn fail(&mut self) -> ComputerResult<()> {
        self.runtime.fail()
    }

    pub fn into_parts(
        self,
    ) -> (
        IsolatedVisualRuntimeSession,
        IsolatedVisualHelperControl<ER, CW>,
        IsolatedVisualStream<FR, IW>,
    ) {
        (self.runtime, self.helper, self.stream)
    }
}
