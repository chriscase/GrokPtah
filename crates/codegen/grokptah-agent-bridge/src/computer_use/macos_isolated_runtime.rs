use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::Duration;

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
const STOPPING_EVENT_TIMEOUT: Duration = Duration::from_secs(15);

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

fn close_spawn_descriptors(result: &NativeIsolatedRuntimeSpawnResult) {
    for descriptor in [
        result.control_fd,
        result.event_fd,
        result.input_fd,
        result.frame_fd,
        result.challenge_fd,
    ] {
        close_fd(descriptor);
    }
}

fn terminate_process(pid: libc::pid_t) {
    if pid <= 0 {
        return;
    }
    // SAFETY: the PID was returned by our launch shim. A failed kill is
    // intentionally ignored during drop; the normal stop path reports it.
    unsafe {
        let _ = libc::kill(pid, libc::SIGKILL);
        let _ = libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}

/// A macOS-only supervisor that owns the packaged helper process and all five
/// private channels. It is deliberately not wired into capability admission:
/// callers must still provide a reviewed package manifest and independently
/// prove boot, rendering, input, and cleanup before exposing this backend.
pub struct IsolatedVisualPackagedRuntime {
    pid: libc::pid_t,
    exited: bool,
    driver: IsolatedVisualRuntimeDriver<
        BufReader<File>,
        BufWriter<File>,
        BufReader<File>,
        BufWriter<File>,
    >,
}

impl IsolatedVisualPackagedRuntime {
    pub fn launch(contract: IsolatedVisualLaunchContract) -> ComputerResult<Self> {
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
            || native.control_fd < 0
            || native.event_fd < 0
            || native.input_fd < 0
            || native.frame_fd < 0
            || native.challenge_fd < 0
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
        let mut challenge_reader =
            unsafe { BufReader::new(File::from_raw_fd(native.challenge_fd)) };
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
            BufReader::new(unsafe { File::from_raw_fd(native.event_fd) }),
            BufWriter::new(unsafe { File::from_raw_fd(native.control_fd) }),
        );
        let stream = IsolatedVisualStream::new(
            BufReader::new(unsafe { File::from_raw_fd(native.frame_fd) }),
            BufWriter::new(unsafe { File::from_raw_fd(native.input_fd) }),
        );
        unsafe { gpt_macos_isolated_runtime_spawn_result_free(&mut native) };
        Ok(Self {
            pid,
            exited: false,
            driver: IsolatedVisualRuntimeDriver::new(runtime, helper, stream),
        })
    }

    /// Drives the fixed Prepared → start → Running → bind → Bound sequence.
    pub fn start(&mut self) -> ComputerResult<()> {
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(PREPARED_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        if let Err(error) = self.driver.start() {
            return self.abort_with_error(error);
        }
        if let Err(error) = self
            .driver
            .receive_helper_event_with_timeout(RUNNING_EVENT_TIMEOUT)
        {
            return self.abort_with_error(error);
        }
        if let Err(error) = self.driver.bind() {
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

    pub fn read_frame(&mut self) -> ComputerResult<Option<IsolatedVisualFrame>> {
        match self.driver.read_frame() {
            Ok(frame) => Ok(frame),
            Err(error) => self.abort_with_error(error),
        }
    }

    pub fn write_input(
        &mut self,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<()> {
        match self
            .driver
            .write_input(input_sequence, request_nonce, message)
        {
            Ok(()) => Ok(()),
            Err(error) => self.abort_with_error(error),
        }
    }

    pub fn stop(&mut self, disposition: IsolatedVisualTerminalDisposition) -> ComputerResult<()> {
        if let Err(error) = self.driver.stop(disposition) {
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
        self.wait_for_exit()
    }

    /// Completes the terminal transition only after the caller has verified
    /// exact helper/process absence, open-handle closure, overlay removal, and
    /// frame-cache removal for this surface incarnation.
    pub fn complete_cleanup(
        &mut self,
        evidence: &IsolatedVisualCleanupEvidence,
    ) -> ComputerResult<()> {
        self.driver.complete_cleanup(evidence)
    }

    fn abort_with_error<T>(&mut self, error: ComputerError) -> ComputerResult<T> {
        let _ = self.driver.fail();
        if !self.exited {
            let _ = self.wait_for_exit();
        }
        Err(error)
    }

    pub fn runtime(&self) -> &IsolatedVisualRuntimeSession {
        self.driver.runtime()
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
                return Err(ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    "isolated helper wait failed",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        terminate_process(self.pid);
        self.exited = true;
        Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "isolated helper exceeded its bounded exit grace period",
        ))
    }
}

impl Drop for IsolatedVisualPackagedRuntime {
    fn drop(&mut self) {
        if !self.exited {
            terminate_process(self.pid);
            self.exited = true;
        }
    }
}
