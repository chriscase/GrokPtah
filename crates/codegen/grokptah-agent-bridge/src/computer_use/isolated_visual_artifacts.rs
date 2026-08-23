use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::isolated_visual::IsolatedVisualManifest;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

pub const ISOLATED_VISUAL_MAX_HELPER_BYTES: u64 = 64_u64 * 1024 * 1024;
pub const ISOLATED_VISUAL_MAX_GUEST_IMAGE_BYTES: u64 = 32_u64 * 1024 * 1024 * 1024;
pub const ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES: u64 = 1024_u64 * 1024;

const MEASUREMENT_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualArtifactRole {
    HelperExecutable,
    GuestImage,
    Configuration,
}

impl IsolatedVisualArtifactRole {
    fn maximum_bytes(self) -> u64 {
        match self {
            Self::HelperExecutable => ISOLATED_VISUAL_MAX_HELPER_BYTES,
            Self::GuestImage => ISOLATED_VISUAL_MAX_GUEST_IMAGE_BYTES,
            Self::Configuration => ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::HelperExecutable => "isolated helper executable",
            Self::GuestImage => "isolated guest image",
            Self::Configuration => "isolated configuration",
        }
    }
}

/// Path-free content identity for one already-open packaged artifact. This is
/// not a code-signing receipt and cannot establish that a file came from the
/// application bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualArtifactMeasurement {
    pub role: IsolatedVisualArtifactRole,
    pub content_sha256: String,
    pub bytes: u64,
}

impl IsolatedVisualArtifactMeasurement {
    pub fn validate(&self) -> ComputerResult<()> {
        if self.bytes == 0 || self.bytes > self.role.maximum_bytes() {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                format!("{} exceeds its measurement ceiling", self.role.label()),
            ));
        }
        validate_digest(self.role.label(), &self.content_sha256)
    }
}

/// Exact content measurements for the three immutable Stage 9 artifacts.
/// Paths, descriptors, signing information, and file-system identity are not
/// serializable through this receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualArtifactMeasurements {
    pub helper: IsolatedVisualArtifactMeasurement,
    pub guest_image: IsolatedVisualArtifactMeasurement,
    pub configuration: IsolatedVisualArtifactMeasurement,
}

impl IsolatedVisualArtifactMeasurements {
    pub fn validate(&self) -> ComputerResult<()> {
        self.helper.validate()?;
        self.guest_image.validate()?;
        self.configuration.validate()?;
        if self.helper.role != IsolatedVisualArtifactRole::HelperExecutable
            || self.guest_image.role != IsolatedVisualArtifactRole::GuestImage
            || self.configuration.role != IsolatedVisualArtifactRole::Configuration
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated artifact receipt assigns an artifact to the wrong role",
            ));
        }
        Ok(())
    }

    /// Compare content identity with the closed launch manifest. This does not
    /// validate `helper_signing_requirement_sha256`; a future macOS package
    /// verifier must independently establish that value from the signed code.
    pub fn validate_content_against_manifest(
        &self,
        manifest: &IsolatedVisualManifest,
    ) -> ComputerResult<()> {
        self.validate()?;
        manifest.validate()?;
        if self.helper.content_sha256 != manifest.helper_content_sha256
            || self.guest_image.content_sha256 != manifest.guest_image_sha256
            || self.configuration.content_sha256 != manifest.configuration_sha256
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated artifact content does not match the launch manifest",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactFileIdentity {
    bytes: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl ArtifactFileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Self {
                bytes: metadata.len(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                bytes: metadata.len(),
                modified: metadata.modified().ok(),
            }
        }
    }
}

fn validate_digest(name: &str, value: &str) -> ComputerResult<()> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("invalid {name} digest"),
        ));
    }
    Ok(())
}

fn io_error(context: &str, error: std::io::Error) -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::BackendFailure,
        format!("{context}: {error}"),
    )
}

#[cfg(unix)]
fn validate_read_only_handle(file: &File) -> ComputerResult<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: F_GETFL only reads flags from this valid borrowed descriptor.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io_error(
            "could not inspect isolated artifact descriptor",
            std::io::Error::last_os_error(),
        ));
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(ComputerError::new(
            ComputerErrorCode::ForbiddenAction,
            "isolated artifacts must be measured through read-only descriptors",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_read_only_handle(_file: &File) -> ComputerResult<()> {
    Err(ComputerError::new(
        ComputerErrorCode::UnsupportedPlatform,
        "isolated artifact measurement currently requires a Unix descriptor",
    ))
}

fn validate_metadata(role: IsolatedVisualArtifactRole, metadata: &Metadata) -> ComputerResult<()> {
    if !metadata.file_type().is_file() {
        return Err(ComputerError::new(
            ComputerErrorCode::ForbiddenTarget,
            format!("{} is not a regular file", role.label()),
        ));
    }
    if metadata.len() == 0 || metadata.len() > role.maximum_bytes() {
        return Err(ComputerError::new(
            ComputerErrorCode::LimitReached,
            format!("{} exceeds its measurement ceiling", role.label()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let mode = metadata.mode();
        if mode & 0o022 != 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                format!("{} is group- or world-writable", role.label()),
            ));
        }
        let executable = mode & 0o111 != 0;
        if executable != (role == IsolatedVisualArtifactRole::HelperExecutable) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                format!("{} has an invalid executable mode", role.label()),
            ));
        }
    }
    Ok(())
}

fn sha256_exact(file: &mut File, expected_bytes: u64) -> ComputerResult<String> {
    let original_offset = file
        .stream_position()
        .map_err(|error| io_error("could not read isolated artifact offset", error))?;
    let result = (|| {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io_error("could not rewind isolated artifact", error))?;
        let mut hasher = Sha256::new();
        let mut remaining = expected_bytes;
        let mut buffer = vec![0_u8; MEASUREMENT_BUFFER_BYTES];
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    "isolated artifact measurement length cannot be represented",
                )
            })?;
            let read = file
                .read(&mut buffer[..wanted])
                .map_err(|error| io_error("could not read isolated artifact", error))?;
            if read == 0 {
                return Err(ComputerError::new(
                    ComputerErrorCode::TargetChanged,
                    "isolated artifact ended during measurement",
                ));
            }
            hasher.update(&buffer[..read]);
            remaining -= read as u64;
        }
        let mut trailing = [0_u8; 1];
        if file
            .read(&mut trailing)
            .map_err(|error| io_error("could not finish isolated artifact measurement", error))?
            != 0
        {
            return Err(ComputerError::new(
                ComputerErrorCode::TargetChanged,
                "isolated artifact grew during measurement",
            ));
        }
        Ok(format!("{:x}", hasher.finalize()))
    })();
    let restored = file
        .seek(SeekFrom::Start(original_offset))
        .map_err(|error| io_error("could not restore isolated artifact offset", error));
    match (result, restored) {
        (Ok(digest), Ok(_)) => Ok(digest),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Measure one already-open artifact without serializing or resolving its
/// path. The caller must open it read-only and keep the handle exclusively
/// borrowed for the duration of this operation. Bundle discovery, no-symlink
/// open, code-signing verification, and helper launch remain separate gates.
pub fn measure_open_isolated_visual_artifact(
    file: &mut File,
    role: IsolatedVisualArtifactRole,
) -> ComputerResult<IsolatedVisualArtifactMeasurement> {
    validate_read_only_handle(file)?;
    let before = file
        .metadata()
        .map_err(|error| io_error("could not inspect isolated artifact", error))?;
    validate_metadata(role, &before)?;
    let before_identity = ArtifactFileIdentity::from_metadata(&before);
    let content_sha256 = sha256_exact(file, before_identity.bytes)?;
    let after = file
        .metadata()
        .map_err(|error| io_error("could not re-inspect isolated artifact", error))?;
    validate_metadata(role, &after)?;
    if ArtifactFileIdentity::from_metadata(&after) != before_identity {
        return Err(ComputerError::new(
            ComputerErrorCode::TargetChanged,
            "isolated artifact identity changed during measurement",
        ));
    }
    let measurement = IsolatedVisualArtifactMeasurement {
        role,
        content_sha256,
        bytes: before_identity.bytes,
    };
    measurement.validate()?;
    Ok(measurement)
}

/// Measure all Stage 9 artifact contents from independently opened read-only
/// handles. This function deliberately returns no signing claim.
pub fn measure_open_isolated_visual_artifacts(
    helper: &mut File,
    guest_image: &mut File,
    configuration: &mut File,
) -> ComputerResult<IsolatedVisualArtifactMeasurements> {
    let measurements = IsolatedVisualArtifactMeasurements {
        helper: measure_open_isolated_visual_artifact(
            helper,
            IsolatedVisualArtifactRole::HelperExecutable,
        )?,
        guest_image: measure_open_isolated_visual_artifact(
            guest_image,
            IsolatedVisualArtifactRole::GuestImage,
        )?,
        configuration: measure_open_isolated_visual_artifact(
            configuration,
            IsolatedVisualArtifactRole::Configuration,
        )?,
    };
    measurements.validate()?;
    Ok(measurements)
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    use tempfile::tempdir;

    use super::*;
    use crate::computer_use::{
        IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION, ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
        MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
    };

    fn write_artifact(name: &str, bytes: &[u8], executable: bool) -> (tempfile::TempDir, File) {
        let directory = tempdir().unwrap();
        let path = directory.path().join(name);
        let mut writer = File::create(&path).unwrap();
        writer.write_all(bytes).unwrap();
        drop(writer);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
            )
            .unwrap();
        }
        let reader = File::open(path).unwrap();
        (directory, reader)
    }

    #[test]
    fn stable_read_only_handles_measure_without_leaking_paths_or_offsets() {
        let (helper_dir, mut helper) = write_artifact("helper", b"helper-v1", true);
        let (guest_dir, mut guest) = write_artifact("guest.img", b"guest-v1", false);
        let (config_dir, mut config) = write_artifact("config.json", b"{}", false);
        helper.seek(SeekFrom::Start(3)).unwrap();

        let measurements =
            measure_open_isolated_visual_artifacts(&mut helper, &mut guest, &mut config).unwrap();
        assert_eq!(measurements.helper.bytes, 9);
        assert_eq!(measurements.guest_image.bytes, 8);
        assert_eq!(measurements.configuration.bytes, 2);
        assert_eq!(helper.stream_position().unwrap(), 3);
        let serialized = serde_json::to_string(&measurements).unwrap();
        assert!(!serialized.contains(helper_dir.path().to_string_lossy().as_ref()));
        assert!(!serialized.contains(guest_dir.path().to_string_lossy().as_ref()));
        assert!(!serialized.contains(config_dir.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn writable_handles_wrong_modes_and_sparse_oversize_fail_closed() {
        let (helper_dir, _helper) = write_artifact("helper", b"helper-v1", true);
        let helper_path = helper_dir.path().join("helper");
        let mut writable = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&helper_path)
            .unwrap();
        assert_eq!(
            measure_open_isolated_visual_artifact(
                &mut writable,
                IsolatedVisualArtifactRole::HelperExecutable,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );

        let (_data_dir, mut executable_data) = write_artifact("guest.img", b"guest-v1", true);
        assert_eq!(
            measure_open_isolated_visual_artifact(
                &mut executable_data,
                IsolatedVisualArtifactRole::GuestImage,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );

        let (writable_dir, _configuration) = write_artifact("configuration.json", b"{}", false);
        let writable_path = writable_dir.path().join("configuration.json");
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&writable_path, std::fs::Permissions::from_mode(0o422))
                .unwrap();
        }
        let mut writable_mode = File::open(writable_path).unwrap();
        assert_eq!(
            measure_open_isolated_visual_artifact(
                &mut writable_mode,
                IsolatedVisualArtifactRole::Configuration,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenAction
        );

        let directory = tempdir().unwrap();
        let path = directory.path().join("config.json");
        let file = File::create(&path).unwrap();
        file.set_len(ISOLATED_VISUAL_MAX_CONFIGURATION_BYTES + 1)
            .unwrap();
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        let mut oversized = File::open(path).unwrap();
        assert_eq!(
            measure_open_isolated_visual_artifact(
                &mut oversized,
                IsolatedVisualArtifactRole::Configuration,
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::LimitReached
        );
    }

    #[test]
    fn manifest_matching_checks_content_but_never_infers_signing() {
        let (_helper_dir, mut helper) = write_artifact("helper", b"helper-v1", true);
        let (_guest_dir, mut guest) = write_artifact("guest.img", b"guest-v1", false);
        let (_config_dir, mut config) = write_artifact("config.json", b"{}", false);
        let measurements =
            measure_open_isolated_visual_artifacts(&mut helper, &mut guest, &mut config).unwrap();
        let mut manifest = IsolatedVisualManifest {
            schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
            backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
            guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
            helper_content_sha256: measurements.helper.content_sha256.clone(),
            helper_signing_requirement_sha256: "a".repeat(64),
            guest_image_sha256: measurements.guest_image.content_sha256.clone(),
            configuration_sha256: measurements.configuration.content_sha256.clone(),
            security_profile: IsolatedVisualSecurityProfile::locked_down(),
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        };
        measurements
            .validate_content_against_manifest(&manifest)
            .unwrap();
        manifest.guest_image_sha256 = "b".repeat(64);
        assert_eq!(
            measurements
                .validate_content_against_manifest(&manifest)
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }
}
