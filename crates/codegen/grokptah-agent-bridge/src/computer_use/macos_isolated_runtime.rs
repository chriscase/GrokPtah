use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::Duration;

use super::isolated_guest::IsolatedGuestLease;
use super::isolated_visual::{
    IsolatedVisualCleanupEvidence, IsolatedVisualLaunchContract, IsolatedVisualTerminalDisposition,
};
use super::isolated_visual_driver::IsolatedVisualRuntimeDriver;
use super::isolated_visual_frames::IsolatedVisualFrame;
use super::isolated_visual_helper_control::{
    read_isolated_visual_challenge_with_timeout, IsolatedVisualHelperControl,
};
use super::isolated_visual_input::IsolatedVisualInputMessage;
use super::isolated_visual_runtime::IsolatedVisualRuntimeSession;
use super::isolated_visual_stream::IsolatedVisualStream;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

const NATIVE_OK: i32 = 0;
const NATIVE_TARGET_CHANGED: i32 = 4;
const NATIVE_LIMIT_REACHED: i32 = 6;
const NATIVE_BACKEND_FAILURE: i32 = 7;
const NATIVE_INVALID_REQUEST: i32 = 8;
const NATIVE_FORBIDDEN_ACTION: i32 = 9;
const NATIVE_BACKEND_UNAVAILABLE: i32 = 12;
const NATIVE_UNAUTHORIZED: i32 = 13;
const MAX_NATIVE_ERROR_BYTES: usize = 512;
const PREPARED_EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(30);
const RUNNING_EVENT_TIMEOUT: Duration = Duration::from_secs(60);
const BOUND_EVENT_TIMEOUT: Duration = Duration::from_secs(15);
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(15);
const INPUT_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
const STOPPING_EVENT_TIMEOUT: Duration = Duration::from_secs(15);
const FORCE_REAP_TIMEOUT: Duration = Duration::from_secs(5);

#[repr(C)]
struct NativeIsolatedRuntimeSpawnResult {
    status: i32,
    pid: i32,
    control_fd: i32,
    event_fd: i32,
    input_fd: i32,
    frame_fd: i32,
    challenge_fd: i32,
    error: *mut u8,
    error_len: usize,
}

unsafe extern "C" {
    fn gpt_macos_isolated_runtime_spawn(
        helper_fd: i32,
        guest_image_fd: i32,
        configuration_fd: i32,
    ) -> NativeIsolatedRuntimeSpawnResult;
    fn gpt_macos_isolated_runtime_spawn_result_free(result: *mut NativeIsolatedRuntimeSpawnResult);
}

fn native_error(result: &NativeIsolatedRuntimeSpawnResult) -> String {
    if result.error.is_null() || result.error_len == 0 || result.error_len > MAX_NATIVE_ERROR_BYTES
    {
        return "Packaged isolated helper launch failed".into();
    }
    // SAFETY: the native result owns this bounded allocation until its paired
    // free function is called by the caller.
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result.error, result.error_len) })
        .into_owned()
}

fn native_error_code(status: i32) -> ComputerErrorCode {
    match status {
        NATIVE_TARGET_CHANGED => ComputerErrorCode::TargetChanged,
        NATIVE_LIMIT_REACHED => ComputerErrorCode::LimitReached,
        NATIVE_INVALID_REQUEST => ComputerErrorCode::InvalidRequest,
        NATIVE_FORBIDDEN_ACTION => ComputerErrorCode::ForbiddenAction,
        NATIVE_BACKEND_UNAVAILABLE => ComputerErrorCode::BackendUnavailable,
        NATIVE_UNAUTHORIZED => ComputerErrorCode::Unauthorized,
        NATIVE_BACKEND_FAILURE => ComputerErrorCode::BackendFailure,
        _ => ComputerErrorCode::BackendFailure,
    }
}

fn close_fd(descriptor: i32) {
    if descriptor >= 0 {
        // SAFETY: descriptors are returned by the native launch shim and are
        // closed exactly once on an error path.
        unsafe {
            libc::close(descriptor);
        }
    }
}

fn descriptors_are_distinct(descriptors: &[i32]) -> bool {
    descriptors.iter().enumerate().all(|(index, descriptor)| {
        *descriptor >= 0 && descriptors[..index].iter().all(|prior| prior != descriptor)
    })
}

fn waitpid_without_interrupt(pid: libc::pid_t) -> Option<libc::pid_t> {
    for _ in 0..8 {
        // SAFETY: the PID was returned by the native launch shim and this
        // supervisor is the only code allowed to reap it.
        let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        if result < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                std::thread::yield_now();
                continue;
            }
        }
        return Some(result);
    }
    None
}

fn close_spawn_descriptors(result: &NativeIsolatedRuntimeSpawnResult) {
    let descriptors = [
        result.control_fd,
        result.event_fd,
        result.input_fd,
        result.frame_fd,
        result.challenge_fd,
    ];
    for (index, descriptor) in descriptors.iter().enumerate() {
        if *descriptor >= 0 && descriptors[..index].iter().all(|prior| prior != descriptor) {
            close_fd(*descriptor);
        }
    }
}

fn terminate_process(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return true;
    }
    // Reap/check ownership before sending a signal. If the child already
    // exited, or another owner reaped it, the numeric PID must never be
    // treated as a live process that can be killed after PID reuse.
    let Some(initial) = waitpid_without_interrupt(pid) else {
        return false;
    };
    if initial == pid {
        return true;
    }
    if initial < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return true;
        }
        // `waitpid_without_interrupt` already retries EINTR, so any remaining
        // error means ownership could not be established safely.
        return false;
    }
    // SAFETY: the PID was returned by our launch shim. A failed kill is
    // intentionally followed by bounded reap polling; the normal stop path
    // reports a failure to reap rather than blocking forever.
    unsafe {
        let _ = libc::kill(pid, libc::SIGKILL);
    }
    let attempts = FORCE_REAP_TIMEOUT.as_millis().max(1) / 100;
    for _ in 0..attempts {
        // SAFETY: this is the child PID owned by this supervisor.
        let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        if result == pid {
            return true;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return true;
            }
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// A macOS-only supervisor that owns the packaged helper process and all five
/// private channels. It is deliberately not wired into capability admission:
/// callers must still provide a reviewed package manifest and independently
/// prove boot, rendering, input, and cleanup before exposing this backend.
pub(crate) struct IsolatedVisualPackagedRuntime {
    pid: libc::pid_t,
    exited: bool,
    lease: Option<IsolatedGuestLease>,
    driver: IsolatedVisualRuntimeDriver<File, File, File, File>,
}

impl IsolatedVisualPackagedRuntime {
    pub(crate) fn launch(contract: IsolatedVisualLaunchContract) -> ComputerResult<Self> {
        contract.validate()?;
        // Hold the exact read-only package handles through the native launch
        // call. This binds the caller's manifest to measured artifact bytes
        // and the helper's designated requirement before any child process is
        // created; native launch then repeats its path/signature identity
        // checks immediately before spawn.
        let verified_artifacts =
            super::macos_isolated_artifacts::open_packaged_runtime_artifacts(&contract.manifest)?;
        // SAFETY: the native shim returns either an error buffer or ownership
        // of five distinct descriptors and one child PID. The paired free
        // function releases only the error buffer.
        let mut native = unsafe {
            gpt_macos_isolated_runtime_spawn(
                verified_artifacts.helper.as_raw_fd(),
                verified_artifacts.guest_image.as_raw_fd(),
                verified_artifacts.configuration.as_raw_fd(),
            )
        };
        if native.status != NATIVE_OK {
            let error = ComputerError::new(native_error_code(native.status), native_error(&native));
            unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
            return Err(error);
        }
        if native.pid <= 0
            || !descriptors_are_distinct(&[
                native.control_fd,
                native.event_fd,
                native.input_fd,
                native.frame_fd,
                native.challenge_fd,
            ])
        {
            close_spawn_descriptors(&native);
            terminate_process(native.pid);
            unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated launch returned an incomplete process/channel set",
            ));
        }

        // SAFETY: each descriptor is transferred from the native result once.
        let pid = native.pid;
        let mut challenge_reader = unsafe { File::from_raw_fd(native.challenge_fd) };
        let challenge = match read_isolated_visual_challenge_with_timeout(
            &mut challenge_reader,
            CHALLENGE_TIMEOUT,
        ) {
            Ok(challenge) => challenge,
            Err(error) => {
                drop(challenge_reader);
                close_fd(native.control_fd);
                close_fd(native.event_fd);
                close_fd(native.input_fd);
                close_fd(native.frame_fd);
                terminate_process(pid);
                unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
                return Err(error);
            }
        };
        drop(challenge_reader);

        let runtime = match IsolatedVisualRuntimeSession::new(contract, challenge) {
            Ok(runtime) => runtime,
            Err(error) => {
                close_fd(native.control_fd);
                close_fd(native.event_fd);
                close_fd(native.input_fd);
                close_fd(native.frame_fd);
                terminate_process(pid);
                unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
                return Err(error);
            }
        };
        // SAFETY: successful construction transfers each remaining descriptor
        // into exactly one Rust File owner.
        let helper = IsolatedVisualHelperControl::new(
            unsafe { File::from_raw_fd(native.event_fd) },
            unsafe { File::from_raw_fd(native.control_fd) },
        );
        let stream =
            IsolatedVisualStream::new(unsafe { File::from_raw_fd(native.frame_fd) }, unsafe {
                File::from_raw_fd(native.input_fd)
            });
        unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
        Ok(Self {
            pid,
            exited: false,
            lease: None,
            driver: IsolatedVisualRuntimeDriver::new(runtime, helper, stream),
        })
    }

    /// Claims this packaged guest for exactly one Agent. The supervisor is
    /// intentionally unusable until the caller presents the returned lease.
    pub(crate) fn acquire(
        &mut self,
        agent_id: impl Into<String>,
    ) -> ComputerResult<IsolatedGuestLease> {
        let agent_id = agent_id.into();
        if self.exited {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated packaged guest has already exited",
            ));
        }
        if let Some(existing) = &self.lease {
            if existing.agent_id != agent_id.as_str() {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated packaged guest is already leased to another agent",
                ));
            }
            return Ok(existing.clone());
        }
        let lease = IsolatedGuestLease::issue(agent_id)?;
        self.lease = Some(lease.clone());
        Ok(lease)
    }

    /// Drives the fixed Prepared → start → Running → bind → Bound sequence.
    pub(crate) fn start(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
    ) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(PREPARED_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        if let Err(error) = self.driver.start_with_timeout(CONTROL_WRITE_TIMEOUT) {
            return self.abort_with_error(error);
        }
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(RUNNING_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        if let Err(error) = self.driver.bind_with_timeout(CONTROL_WRITE_TIMEOUT) {
            return self.abort_with_error(error);
        }
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(BOUND_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        Ok(())
    }

    pub(crate) fn read_frame(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
    ) -> ComputerResult<Option<IsolatedVisualFrame>> {
        self.require_lease(agent_id, lease)?;
        match self.driver.read_frame_with_timeout(FRAME_READ_TIMEOUT) {
            Ok(frame) => Ok(frame),
            Err(error) => self.abort_with_error(error),
        }
    }

    pub(crate) fn write_input(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        match self.driver.write_input_with_timeout(
            input_sequence,
            request_nonce,
            message,
            INPUT_WRITE_TIMEOUT,
        ) {
            Ok(()) => Ok(()),
            Err(error) => self.abort_with_error(error),
        }
    }

    pub(crate) fn stop(
        &mut self,
        agent_id: &str,
        lease: &IsolatedGuestLease,
        disposition: IsolatedVisualTerminalDisposition,
    ) -> ComputerResult<()> {
        self.require_lease(agent_id, lease)?;
        if let Err(error) = self
            .driver
            .stop_with_timeout(disposition, CONTROL_WRITE_TIMEOUT)
        {
            if self.driver.runtime().lifecycle_state()
                == super::isolated_visual::IsolatedVisualLifecycleState::Stopping
            {
                return self.abort_with_error(error);
            }
            return Err(error);
        }
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(STOPPING_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        // Stop is terminal even when bounded reaping reports an error. Never
        // leave a live lease attached to a stopping/exited guest: a failed
        // stop must not create a path to resume or retry input. Cleanup still
        // requires its independent exact evidence and may fail closed while
        // the process/handles are unresolved.
        let result = self.wait_for_exit();
        finish_terminal_stop(&mut self.lease, result)
    }

    /// Completes the terminal transition only after the caller has verified
    /// exact helper/process absence, open-handle closure, overlay removal, and
    /// frame-cache removal for this surface incarnation.
    pub(crate) fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        if self.lease.is_some() {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "isolated packaged guest cleanup requires the lease to be revoked",
            ));
        }
        self.driver.complete_cleanup(evidence)
    }

    fn abort_with_error<T>(&mut self, error: ComputerError) -> ComputerResult<T> {
        let _ = self.driver.fail();
        self.lease = None;
        if !self.exited {
            let _ = self.wait_for_exit();
        }
        Err(error)
    }

    pub(crate) fn runtime(&self) -> &IsolatedVisualRuntimeSession {
        self.driver.runtime()
    }

    fn require_lease(&self, agent_id: &str, presented: &IsolatedGuestLease) -> ComputerResult<()> {
        let Some(live) = &self.lease else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated packaged guest control requires a valid lease",
            ));
        };
        live.require(agent_id, presented)
    }

    fn wait_for_exit(&mut self) -> ComputerResult<()> {
        for _ in 0..50 {
            // SAFETY: this is the child PID owned by this supervisor.
            let result = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == self.pid {
                self.exited = true;
                return Ok(());
            }
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.exited = true;
                    return Ok(());
                }
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    "isolated helper wait failed",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if terminate_process(self.pid) {
            self.exited = true;
            Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated helper exceeded its bounded exit grace period",
            ))
        } else {
            Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "isolated helper could not be reaped after forced termination",
            ))
        }
    }
}

fn finish_terminal_stop<T>(
    lease: &mut Option<IsolatedGuestLease>,
    result: ComputerResult<T>,
) -> ComputerResult<T> {
    *lease = None;
    result
}

impl Drop for IsolatedVisualPackagedRuntime {
    fn drop(&mut self) {
        if !self.exited {
            terminate_process(self.pid);
            self.exited = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{descriptors_are_distinct, finish_terminal_stop, IsolatedGuestLease};
    use crate::computer_use::types::{ComputerError, ComputerErrorCode};

    #[test]
    fn native_launch_descriptor_set_must_be_complete_and_unique() {
        assert!(descriptors_are_distinct(&[3, 4, 5, 6, 7]));
        assert!(!descriptors_are_distinct(&[3, 4, 4, 6, 7]));
        assert!(!descriptors_are_distinct(&[3, -1, 5, 6, 7]));
    }

    #[test]
    fn terminal_stop_revokes_lease_even_when_reaping_fails() {
        let mut lease = Some(IsolatedGuestLease {
            lease_id: "lease".into(),
            agent_id: "agent".into(),
            revision: 1,
        });
        let result = finish_terminal_stop::<()>(
            &mut lease,
            Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "reap failed",
            )),
        );
        assert!(lease.is_none());
        assert_eq!(result.unwrap_err().code, ComputerErrorCode::BackendFailure);
    }
}
