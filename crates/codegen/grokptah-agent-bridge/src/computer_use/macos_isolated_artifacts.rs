use std::fs::File;
use std::os::fd::FromRawFd;
use std::ptr::NonNull;
use std::slice;

use sha2::{Digest, Sha256};

use super::isolated_visual::IsolatedVisualManifest;
use super::isolated_visual_artifacts::{
    measure_open_isolated_visual_artifacts, IsolatedVisualPackagedArtifactReceipt,
};
use super::isolated_visual_launch::descriptors_are_private_and_distinct;
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
const MAX_REQUIREMENT_DATA_BYTES: usize = 64 * 1024;

/// Verified, still-open artifact handles for the one packaged helper launch.
/// The paths and descriptors remain private to the host supervisor; only the
/// path-free receipt may cross a projection boundary.
pub(super) struct IsolatedVisualRuntimeArtifacts {
    pub(super) helper: File,
    pub(super) guest_image: File,
    pub(super) configuration: File,
    pub(super) receipt: IsolatedVisualPackagedArtifactReceipt,
}

#[repr(C)]
struct NativeIsolatedArtifactsResult {
    status: i32,
    helper_fd: i32,
    guest_image_fd: i32,
    configuration_fd: i32,
    requirement_data: *mut u8,
    requirement_data_len: usize,
    error: *mut u8,
    error_len: usize,
}

unsafe extern "C" {
    fn gpt_macos_isolated_artifacts_open() -> NativeIsolatedArtifactsResult;
    fn gpt_macos_isolated_artifacts_result_free(result: *mut NativeIsolatedArtifactsResult);
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

fn copy_native_bytes(
    pointer: *const u8,
    length: usize,
    maximum: usize,
    label: &str,
) -> ComputerResult<Vec<u8>> {
    if length == 0 || length > maximum || NonNull::new(pointer.cast_mut()).is_none() {
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            format!("invalid {label} returned by packaged-artifact verifier"),
        ));
    }
    // SAFETY: the native result owns this allocation until its paired free
    // function is called below, and its length was bounded above.
    Ok(unsafe { slice::from_raw_parts(pointer, length) }.to_vec())
}

fn copy_native_error(result: &NativeIsolatedArtifactsResult) -> String {
    if result.error_len == 0 || result.error_len > MAX_NATIVE_ERROR_BYTES || result.error.is_null()
    {
        return "Packaged isolated artifact verification failed".into();
    }
    // SAFETY: the native allocation remains owned by `result` during this
    // bounded copy and is released immediately afterward.
    String::from_utf8_lossy(unsafe { slice::from_raw_parts(result.error, result.error_len) })
        .into_owned()
}

fn close_returned_descriptors(result: &NativeIsolatedArtifactsResult) {
    let descriptors = [
        result.helper_fd,
        result.guest_image_fd,
        result.configuration_fd,
    ];
    for (index, descriptor) in descriptors.iter().enumerate() {
        // A standard stream is never ours to close, so an unusable verifier
        // result cannot take stdin/stdout/stderr down with it.
        if descriptors_are_private_and_distinct(&[i64::from(*descriptor)])
            && descriptors[..index].iter().all(|prior| prior != descriptor)
        {
            // SAFETY: this path runs only before ownership is transferred to a
            // `File`, and each returned descriptor is closed at most once.
            unsafe {
                libc::close(*descriptor);
            }
        }
    }
}

pub(super) fn measure_packaged_artifacts(
    manifest: &IsolatedVisualManifest,
) -> ComputerResult<IsolatedVisualPackagedArtifactReceipt> {
    Ok(open_packaged_runtime_artifacts(manifest)?.receipt)
}

pub(super) fn open_packaged_runtime_artifacts(
    manifest: &IsolatedVisualManifest,
) -> ComputerResult<IsolatedVisualRuntimeArtifacts> {
    manifest.validate()?;
    // SAFETY: the Objective-C shim returns a plain owned result whose buffers
    // are released with the paired function and whose descriptors transfer
    // only on a successful status.
    let mut native = unsafe { gpt_macos_isolated_artifacts_open() };
    if native.status != NATIVE_OK {
        let error =
            ComputerError::new(native_error_code(native.status), copy_native_error(&native));
        // The native error path closes any descriptors before returning.
        unsafe { gpt_macos_isolated_artifacts_result_free(&mut native) };
        return Err(error);
    }
    if !descriptors_are_private_and_distinct(&[
        i64::from(native.helper_fd),
        i64::from(native.guest_image_fd),
        i64::from(native.configuration_fd),
    ]) {
        close_returned_descriptors(&native);
        unsafe { gpt_macos_isolated_artifacts_result_free(&mut native) };
        return Err(ComputerError::new(
            ComputerErrorCode::BackendFailure,
            "packaged-artifact verifier returned an incomplete descriptor set",
        ));
    }
    let requirement_data = match copy_native_bytes(
        native.requirement_data,
        native.requirement_data_len,
        MAX_REQUIREMENT_DATA_BYTES,
        "helper designated requirement",
    ) {
        Ok(data) => data,
        Err(error) => {
            close_returned_descriptors(&native);
            unsafe { gpt_macos_isolated_artifacts_result_free(&mut native) };
            return Err(error);
        }
    };
    let helper_fd = native.helper_fd;
    let guest_image_fd = native.guest_image_fd;
    let configuration_fd = native.configuration_fd;
    unsafe { gpt_macos_isolated_artifacts_result_free(&mut native) };

    // SAFETY: successful native verification transfers one distinct, valid,
    // read-only descriptor for each artifact to this function exactly once.
    let mut helper = unsafe { File::from_raw_fd(helper_fd) };
    let mut guest_image = unsafe { File::from_raw_fd(guest_image_fd) };
    let mut configuration = unsafe { File::from_raw_fd(configuration_fd) };
    let measurements =
        measure_open_isolated_visual_artifacts(&mut helper, &mut guest_image, &mut configuration)?;
    let helper_signing_requirement_sha256 = format!("{:x}", Sha256::digest(requirement_data));
    let receipt = IsolatedVisualPackagedArtifactReceipt::verified(
        helper_signing_requirement_sha256,
        measurements,
    )?;
    receipt.validate_against_manifest(manifest)?;
    Ok(IsolatedVisualRuntimeArtifacts {
        helper,
        guest_image,
        configuration,
        receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_error_mapping_is_closed() {
        assert_eq!(
            native_error_code(NATIVE_TARGET_CHANGED),
            ComputerErrorCode::TargetChanged
        );
        assert_eq!(
            native_error_code(NATIVE_LIMIT_REACHED),
            ComputerErrorCode::LimitReached
        );
        assert_eq!(
            native_error_code(NATIVE_UNAUTHORIZED),
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(native_error_code(999), ComputerErrorCode::BackendFailure);
    }

    #[test]
    fn packaged_identity_shim_freezes_paths_and_privilege_boundary() {
        let shim = include_str!("macos_native_shim.m");
        for required in [
            "com.chriscase.grokptah",
            "com.chriscase.grokptah.isolated-visual-helper",
            "Contents/MacOS/grokptah-isolated-visual-helper",
            "Contents/Resources/isolated-visual/grokptah-isolated-guest-v1.img",
            "Contents/Resources/isolated-visual/grokptah-isolated-config-v1.json",
            "O_RDONLY | O_CLOEXEC | O_NOFOLLOW_ANY",
            "kSecCSCheckNestedCode",
            "kSecCSCheckAllArchitectures",
            "kSecCSRestrictSymlinks",
            "kSecCodeSignatureRuntime",
            "kSecCodeSignatureLibraryValidation",
            "com.apple.security.app-sandbox",
            "com.apple.security.virtualization",
            "com.apple.vm.networking",
            "com.apple.security.get-task-allow",
            "SecCodeCopyDesignatedRequirement",
            "SecRequirementCopyData",
            "GPTPathStillNamesArtifact",
        ] {
            assert!(shim.contains(required), "native shim omits {required}");
        }
    }

    #[test]
    fn artifact_descriptor_set_must_be_complete_and_unique() {
        assert!(descriptors_are_private_and_distinct(&[3_i64, 4, 5]));
        assert!(!descriptors_are_private_and_distinct(&[3_i64, 4, 4]));
        assert!(!descriptors_are_private_and_distinct(&[3_i64, -1, 5]));
        assert!(!descriptors_are_private_and_distinct(&[0_i64, 1, 2]));
    }
}
