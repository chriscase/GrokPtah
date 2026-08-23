use std::ffi::c_void;
use std::ptr::NonNull;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::macos_observation::{
    MacActionCancellation, MacActionCancellationSignal, MacNativeIdentity, MacObservationSource,
    MacSemanticExecutionMode, RawMacActionRequest, RawMacObservation, RawMacSemanticAction,
    RawMacSemanticNode, RawMacTarget,
};
use super::platform::{ComputerPermission, ComputerPermissionStatus, ComputerPlatformStatus};
use super::types::{
    ActionOutcome, ComputerAction, ComputerError, ComputerErrorCode, ComputerResult,
    ComputerUseLimits, ObservationGeometry, Sensitivity,
};

const PERMISSION_PROMPT_GRACE: StdDuration = StdDuration::from_secs(60);
const MAX_NATIVE_ACTION_REQUEST_BYTES: usize = 64 * 1024;

#[repr(C)]
struct NativeResult {
    status: i32,
    json: *mut u8,
    json_len: usize,
    png: *mut u8,
    png_len: usize,
    error: *mut u8,
    error_len: usize,
}

unsafe extern "C" {
    fn gpt_macos_observation_supported() -> bool;
    fn gpt_macos_screen_recording_granted() -> bool;
    fn gpt_macos_request_screen_recording() -> bool;
    fn gpt_macos_accessibility_granted() -> bool;
    fn gpt_macos_request_accessibility() -> bool;
    fn gpt_macos_list_targets() -> NativeResult;
    fn gpt_macos_observe(
        window_id: u32,
        process_id: i32,
        bundle_bytes: *const u8,
        bundle_len: usize,
        max_nodes: u32,
        max_dimension: u32,
        max_png_bytes: u64,
    ) -> NativeResult;
    fn gpt_macos_cancellation_create() -> *mut c_void;
    fn gpt_macos_cancellation_signal(cancellation: *mut c_void);
    fn gpt_macos_cancellation_is_signalled(cancellation: *const c_void) -> bool;
    fn gpt_macos_cancellation_free(cancellation: *mut c_void);
    fn gpt_macos_act(
        request_bytes: *const u8,
        request_len: usize,
        cancellation: *const c_void,
    ) -> NativeResult;
    fn gpt_macos_result_free(result: *mut NativeResult);
}

#[derive(Debug, Default)]
struct PermissionTracker {
    ever_granted: AtomicBool,
    prompted_at: Mutex<Option<Instant>>,
}

impl PermissionTracker {
    fn status(&self, granted: bool) -> ComputerPermissionStatus {
        if granted {
            self.ever_granted.store(true, Ordering::SeqCst);
            *self.prompted_at.lock() = None;
            return ComputerPermissionStatus::Granted;
        }
        if self.ever_granted.load(Ordering::SeqCst) {
            return ComputerPermissionStatus::Revoked;
        }
        match *self.prompted_at.lock() {
            Some(at) if at.elapsed() < PERMISSION_PROMPT_GRACE => {
                ComputerPermissionStatus::PromptPending
            }
            Some(_) => ComputerPermissionStatus::Denied,
            None => ComputerPermissionStatus::Missing,
        }
    }

    fn record_prompt(&self) {
        *self.prompted_at.lock() = Some(Instant::now());
    }
}

#[derive(Debug)]
struct NativeMacActionCancellation {
    context: NonNull<c_void>,
}

// The Objective-C allocation contains only one C11 atomic boolean. Its
// lifetime is owned by this value and every access crosses the atomic FFI.
unsafe impl Send for NativeMacActionCancellation {}
unsafe impl Sync for NativeMacActionCancellation {}

impl NativeMacActionCancellation {
    fn new() -> ComputerResult<Self> {
        let context =
            NonNull::new(unsafe { gpt_macos_cancellation_create() }).ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Internal,
                    "macOS could not allocate an action cancellation signal",
                )
            })?;
        Ok(Self { context })
    }
}

impl MacActionCancellationSignal for NativeMacActionCancellation {
    fn cancel(&self) {
        unsafe { gpt_macos_cancellation_signal(self.context.as_ptr()) };
    }

    fn is_cancelled(&self) -> bool {
        unsafe { gpt_macos_cancellation_is_signalled(self.context.as_ptr()) }
    }

    fn native_context(&self) -> *mut c_void {
        self.context.as_ptr()
    }
}

impl Drop for NativeMacActionCancellation {
    fn drop(&mut self) {
        unsafe { gpt_macos_cancellation_free(self.context.as_ptr()) };
    }
}

#[derive(Debug, Default)]
pub(crate) struct NativeMacObservationSource {
    screen_recording: PermissionTracker,
    accessibility: PermissionTracker,
}

impl NativeMacObservationSource {
    pub(crate) fn new() -> ComputerResult<Self> {
        Ok(Self::default())
    }

    fn supported() -> bool {
        // This is a pure runtime availability probe and never prompts.
        unsafe { gpt_macos_observation_supported() }
    }

    fn screen_granted() -> bool {
        // CoreGraphics documents this as the non-prompting preflight call.
        unsafe { gpt_macos_screen_recording_granted() }
    }

    fn accessibility_granted() -> bool {
        // AXIsProcessTrusted without options never requests consent.
        unsafe { gpt_macos_accessibility_granted() }
    }
}

#[async_trait]
impl MacObservationSource for NativeMacObservationSource {
    fn status(&self) -> ComputerPlatformStatus {
        let supported = Self::supported();
        ComputerPlatformStatus {
            platform_id: "macos".into(),
            available: supported,
            minimum_os_version: Some("14.0".into()),
            screen_recording: if supported {
                self.screen_recording.status(Self::screen_granted())
            } else {
                ComputerPermissionStatus::Unsupported
            },
            accessibility: if supported {
                self.accessibility.status(Self::accessibility_granted())
            } else {
                ComputerPermissionStatus::Unsupported
            },
            detail: (!supported).then(|| "macOS 14 or later is required".into()),
        }
    }

    async fn request_permission(
        &self,
        permission: ComputerPermission,
    ) -> ComputerResult<ComputerPermissionStatus> {
        if !Self::supported() {
            return Err(ComputerError::new(
                ComputerErrorCode::UnsupportedPlatform,
                "macOS 14 or later is required",
            ));
        }
        let granted = tokio::task::spawn_blocking(move || unsafe {
            match permission {
                ComputerPermission::ScreenRecording => gpt_macos_request_screen_recording(),
                ComputerPermission::Accessibility => gpt_macos_request_accessibility(),
            }
        })
        .await
        .map_err(join_error)?;
        let tracker = match permission {
            ComputerPermission::ScreenRecording => &self.screen_recording,
            ComputerPermission::Accessibility => &self.accessibility,
        };
        if !granted {
            tracker.record_prompt();
        }
        Ok(tracker.status(granted))
    }

    async fn list_targets(&self) -> ComputerResult<Vec<RawMacTarget>> {
        tokio::task::spawn_blocking(|| {
            let native = unsafe { gpt_macos_list_targets() };
            let output = copy_native_result(native)?;
            let decoded: Vec<NativeTarget> =
                serde_json::from_slice(&output.json).map_err(|_| {
                    ComputerError::new(
                        ComputerErrorCode::BackendFailure,
                        "macOS target list was malformed",
                    )
                })?;
            decoded.into_iter().map(RawMacTarget::try_from).collect()
        })
        .await
        .map_err(join_error)?
    }

    async fn revalidate_target(
        &self,
        identity: &MacNativeIdentity,
    ) -> ComputerResult<RawMacTarget> {
        let identity = identity.clone();
        self.list_targets()
            .await?
            .into_iter()
            .find(|target| target.identity == identity)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::TargetClosed,
                    "selected macOS window is no longer available",
                )
            })
    }

    async fn observe(
        &self,
        identity: &MacNativeIdentity,
        limits: &ComputerUseLimits,
    ) -> ComputerResult<RawMacObservation> {
        let identity = identity.clone();
        let limits = *limits;
        tokio::task::spawn_blocking(move || {
            let native = unsafe {
                gpt_macos_observe(
                    identity.window_id,
                    identity.process_id,
                    identity.bundle_id.as_ptr(),
                    identity.bundle_id.len(),
                    limits.max_semantic_elements,
                    limits.max_screenshot_dimension,
                    limits.max_screenshot_bytes,
                )
            };
            let output = copy_native_result(native)?;
            let decoded: NativeObservation =
                serde_json::from_slice(&output.json).map_err(|_| {
                    ComputerError::new(
                        ComputerErrorCode::BackendFailure,
                        "macOS observation metadata was malformed",
                    )
                })?;
            decoded.into_raw(output.png)
        })
        .await
        .map_err(join_error)?
    }

    fn action_cancellation(&self) -> ComputerResult<MacActionCancellation> {
        Ok(MacActionCancellation::new(Arc::new(
            NativeMacActionCancellation::new()?,
        )))
    }

    fn supports_measured_background(&self) -> bool {
        true
    }

    async fn act(
        &self,
        identity: &MacNativeIdentity,
        request: &RawMacActionRequest,
    ) -> ComputerResult<ActionOutcome> {
        let encoded = encode_action_request(identity, request)?;
        let cancellation = request.cancellation.clone();
        tokio::task::spawn_blocking(move || {
            let native = unsafe {
                gpt_macos_act(
                    encoded.as_ptr(),
                    encoded.len(),
                    cancellation.native_context(),
                )
            };
            let output = copy_native_result(native)?;
            serde_json::from_slice(&output.json).map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::BackendFailure,
                    "macOS action outcome was malformed",
                )
            })
        })
        .await
        .map_err(join_error)?
    }
}

struct OwnedNativeResult {
    json: Vec<u8>,
    png: Vec<u8>,
}

fn copy_native_result(mut native: NativeResult) -> ComputerResult<OwnedNativeResult> {
    let status = native.status;
    let json = unsafe { copy_native_bytes(native.json, native.json_len) };
    let png = unsafe { copy_native_bytes(native.png, native.png_len) };
    let error_bytes = unsafe { copy_native_bytes(native.error, native.error_len) };
    unsafe { gpt_macos_result_free(&mut native) };
    if status != 0 {
        let message = String::from_utf8(error_bytes)
            .ok()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "native macOS observation failed".into());
        return Err(ComputerError::new(native_error_code(status), message));
    }
    Ok(OwnedNativeResult { json, png })
}

unsafe fn copy_native_bytes(pointer: *const u8, length: usize) -> Vec<u8> {
    if pointer.is_null() || length == 0 {
        Vec::new()
    } else {
        // The shim owns this allocation until gpt_macos_result_free is called below.
        unsafe { slice::from_raw_parts(pointer, length) }.to_vec()
    }
}

fn native_error_code(status: i32) -> ComputerErrorCode {
    match status {
        1 => ComputerErrorCode::UnsupportedPlatform,
        2 => ComputerErrorCode::PermissionRequired,
        3 => ComputerErrorCode::TargetClosed,
        4 => ComputerErrorCode::TargetChanged,
        5 => ComputerErrorCode::SensitiveSurface,
        6 => ComputerErrorCode::LimitReached,
        8 => ComputerErrorCode::InvalidRequest,
        9 => ComputerErrorCode::ForbiddenAction,
        10 => ComputerErrorCode::Interrupted,
        11 => ComputerErrorCode::UncertainOutcome,
        _ => ComputerErrorCode::BackendFailure,
    }
}

fn join_error(error: tokio::task::JoinError) -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::BackendFailure,
        format!("native macOS observation worker failed: {error}"),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeIdentity {
    window_id: u32,
    process_id: i32,
    bundle_id: String,
}

impl From<NativeIdentity> for MacNativeIdentity {
    fn from(value: NativeIdentity) -> Self {
        Self {
            window_id: value.window_id,
            process_id: value.process_id,
            bundle_id: value.bundle_id,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeTarget {
    window_id: u32,
    process_id: i32,
    bundle_id: String,
    application_name: String,
    frame: ObservationGeometry,
    on_screen: bool,
    active: bool,
    minimized: bool,
}

impl TryFrom<NativeTarget> for RawMacTarget {
    type Error = ComputerError;

    fn try_from(value: NativeTarget) -> Result<Self, Self::Error> {
        let target = Self {
            identity: MacNativeIdentity {
                window_id: value.window_id,
                process_id: value.process_id,
                bundle_id: value.bundle_id,
            },
            application_name: value.application_name,
            frame: value.frame,
            on_screen: value.on_screen,
            active: value.active,
            minimized: value.minimized,
        };
        target.identity.validate()?;
        target.frame.validate()?;
        Ok(target)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeObservation {
    identity: NativeIdentity,
    frame: ObservationGeometry,
    pixel_width: u32,
    pixel_height: u32,
    privacy_redacted: bool,
    nodes: Vec<NativeNode>,
    nodes_truncated: bool,
    sensitivity: Sensitivity,
}

impl NativeObservation {
    fn into_raw(self, screenshot_png: Vec<u8>) -> ComputerResult<RawMacObservation> {
        Ok(RawMacObservation {
            identity: self.identity.into(),
            captured_at: Utc::now(),
            frame: self.frame,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            privacy_redacted: self.privacy_redacted,
            screenshot_png,
            nodes: self
                .nodes
                .into_iter()
                .map(RawMacSemanticNode::try_from)
                .collect::<ComputerResult<_>>()?,
            nodes_truncated: self.nodes_truncated,
            sensitivity: self.sensitivity,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeNode {
    role: String,
    subrole: Option<String>,
    label: Option<String>,
    value: Option<String>,
    frame: Option<ObservationGeometry>,
    enabled: bool,
    focused: bool,
    sensitivity: Sensitivity,
    actions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeActionEnvelope<'a> {
    window_id: u32,
    process_id: i32,
    bundle_id: &'a str,
    expected_frame: ObservationGeometry,
    element_index: Option<usize>,
    expected_element: Option<NativeExpectedElement<'a>>,
    action: NativeActionPayload<'a>,
    execution_mode: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeExpectedElement<'a> {
    role: &'a str,
    subrole: Option<&'a str>,
    label: Option<&'a str>,
    value: Option<&'a str>,
    frame: Option<ObservationGeometry>,
    enabled: bool,
    sensitivity: Sensitivity,
    required_action: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeActionPayload<'a> {
    kind: &'static str,
    text: Option<&'a str>,
    delta_x: Option<i32>,
    delta_y: Option<i32>,
}

fn encode_action_request(
    identity: &MacNativeIdentity,
    request: &RawMacActionRequest,
) -> ComputerResult<Vec<u8>> {
    identity.validate()?;
    request.target_frame.validate()?;
    let (kind, text, delta_x, delta_y, required_action) = match &request.action {
        ComputerAction::ActivateTarget => ("activate", None, None, None, None),
        ComputerAction::Invoke { .. } => ("invoke", None, None, None, Some("invoke")),
        ComputerAction::SetValue { text, .. } => (
            "set_value",
            Some(text.as_str()),
            None,
            None,
            Some("set_value"),
        ),
        ComputerAction::Select { .. } => ("select", None, None, None, Some("select")),
        ComputerAction::Scroll {
            delta_x, delta_y, ..
        } => (
            "scroll",
            None,
            Some(*delta_x),
            Some(*delta_y),
            Some("scroll"),
        ),
        ComputerAction::KeyChord { .. }
        | ComputerAction::PointerClick { .. }
        | ComputerAction::PointerMove { .. }
        | ComputerAction::PointerButton { .. }
        | ComputerAction::TextInput { .. }
        | ComputerAction::Wait { .. } => {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "native macOS dispatch accepts semantic Accessibility actions only",
            ))
        }
    };
    let expected_element = match (&request.expected_element, required_action) {
        (Some(element), Some(required_action)) => Some(NativeExpectedElement {
            role: &element.role,
            subrole: element.subrole.as_deref(),
            label: element.label.as_deref(),
            value: element.value.as_deref(),
            frame: element.frame,
            enabled: element.enabled,
            sensitivity: element.sensitivity,
            required_action,
        }),
        (None, None) => None,
        _ => {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "native macOS action element binding is incomplete",
            ))
        }
    };
    if expected_element.is_some() != request.element_index.is_some() {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "native macOS action element index does not match its attestation",
        ));
    }
    let envelope = NativeActionEnvelope {
        window_id: identity.window_id,
        process_id: identity.process_id,
        bundle_id: &identity.bundle_id,
        expected_frame: request.target_frame,
        element_index: request.element_index,
        expected_element,
        action: NativeActionPayload {
            kind,
            text,
            delta_x,
            delta_y,
        },
        execution_mode: match request.execution_mode {
            MacSemanticExecutionMode::ForegroundRequired => "foreground_required",
            MacSemanticExecutionMode::MeasuredBackground => "measured_background",
        },
    };
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?;
    if encoded.len() > MAX_NATIVE_ACTION_REQUEST_BYTES {
        return Err(ComputerError::new(
            ComputerErrorCode::LimitReached,
            "native macOS action request exceeds its hard byte limit",
        ));
    }
    Ok(encoded)
}

impl TryFrom<NativeNode> for RawMacSemanticNode {
    type Error = ComputerError;

    fn try_from(value: NativeNode) -> Result<Self, Self::Error> {
        let mut actions = Vec::with_capacity(value.actions.len());
        for action in value.actions {
            actions.push(match action.as_str() {
                "invoke" => RawMacSemanticAction::Invoke,
                "set_value" => RawMacSemanticAction::SetValue,
                "select" => RawMacSemanticAction::Select,
                "scroll" => RawMacSemanticAction::Scroll,
                _ => {
                    return Err(ComputerError::new(
                        ComputerErrorCode::BackendFailure,
                        "macOS returned an unknown semantic action",
                    ))
                }
            });
        }
        Ok(Self {
            role: value.role,
            subrole: value.subrole,
            label: value.label,
            value: value.value,
            frame: value.frame,
            enabled: value.enabled,
            focused: value.focused,
            sensitivity: value.sensitivity,
            actions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_error_codes_are_closed() {
        assert_eq!(native_error_code(1), ComputerErrorCode::UnsupportedPlatform);
        assert_eq!(native_error_code(3), ComputerErrorCode::TargetClosed);
        assert_eq!(native_error_code(10), ComputerErrorCode::Interrupted);
        assert_eq!(native_error_code(11), ComputerErrorCode::UncertainOutcome);
        assert_eq!(native_error_code(999), ComputerErrorCode::BackendFailure);
    }

    #[test]
    fn permission_tracker_distinguishes_revocation() {
        let tracker = PermissionTracker::default();
        assert_eq!(tracker.status(false), ComputerPermissionStatus::Missing);
        assert_eq!(tracker.status(true), ComputerPermissionStatus::Granted);
        assert_eq!(tracker.status(false), ComputerPermissionStatus::Revoked);
    }

    #[test]
    fn native_action_cancellation_signal_is_exact_and_monotonic() {
        let cancellation = NativeMacActionCancellation::new().unwrap();
        assert!(!cancellation.is_cancelled());
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn native_status_probe_is_nonprompting_and_valid() {
        let source = NativeMacObservationSource::new().unwrap();
        let status = source.status();
        eprintln!("native computer-use status: {status:?}");
        status.validate().unwrap();
        assert_eq!(status.platform_id, "macos");
        assert!(
            status.available,
            "ScreenCaptureKit should load on the macOS 14+ test host: {status:?}"
        );
    }

    #[test]
    fn native_shim_exposes_semantic_accessibility_without_raw_input() {
        let shim = include_str!("macos_native_shim.m");
        for forbidden in [
            "CGEventPost",
            "CGWarpMouseCursorPosition",
            "CGAssociateMouseAndMouseCursorPosition",
            "NSPasteboard",
            "NSAppleScript",
            "kCGKeyboardEventKeycode",
        ] {
            assert!(
                !shim.contains(forbidden),
                "native shim contains {forbidden}"
            );
        }
        assert!(shim.contains("gpt_macos_act"));
        assert!(shim.contains("gpt_macos_cancellation_create"));
        assert!(shim.contains("gpt_macos_cancellation_signal"));
        assert!(shim.contains("GPT_MAC_INTERRUPTED"));
        assert!(shim.contains("GPT_MAC_UNCERTAIN_OUTCOME"));
        assert!(shim.contains("GPTCaptureUserInteractionState"));
        assert!(shim.contains("CGEventGetLocation"));
        assert!(shim.contains("measured_background"));
        assert!(shim.contains("AXUIElementPerformAction"));
        assert!(shim.contains("AXUIElementSetAttributeValue"));
        assert!(shim.contains("GPTTargetIsFocused"));
        assert!(shim.contains("visited.count >= maxNodes"));
        assert!(shim.contains("childrenError != kAXErrorNoValue"));
        assert!(shim.contains("NSApplicationLoad"));
        assert!(shim.contains("application.bundleIdentifier.length > 0"));
        assert!(shim.contains("@((BOOL)(!onScreen && !active))"));
        assert!(shim.contains("@((BOOL)nodesTruncated)"));
        assert!(shim.contains("@((BOOL)CFBooleanGetValue"));
        assert!(shim.contains("GPTActImpl"));
    }
}
