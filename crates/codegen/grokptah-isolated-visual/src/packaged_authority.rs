//! Canonical packaged-helper and guest-image authority (#444/#288).
//!
//! Marker files such as `helper.signed` / `guest.img.signed` cannot authorize
//! launch. Sidecar `codesign-display.txt` files are not identity. Admission
//! binds inspected cryptographic identity to a host-pinned expected contract.
//! Eligibility is not a launch receipt.

use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{sha256_hex, validate_digest, validate_id};

pub const PACKAGE_IDENTITY_SCHEMA: &str = "grokptah-computer-use-package-identity.v1";
pub const APP_PRODUCT_NAME: &str = "GrokPtah";
pub const APP_BUNDLE_ID: &str = "com.chriscase.grokptah";
pub const APP_EXECUTABLE: &str = "grokptah-desktop";
pub const APP_VERSION: &str = "0.1.0";
pub const APP_MINIMUM_OS: &str = "11.0";
pub const HELPER_PRODUCT_NAME: &str = "GrokPtah Computer Use Helper";
pub const HELPER_BUNDLE_ID: &str = "com.chriscase.grokptah.computer-use-helper";
pub const HELPER_EXECUTABLE: &str = "grokptah-computer-use-helper";
pub const HELPER_VERSION: &str = "0.1.0";
pub const HELPER_MINIMUM_OS: &str = "14.0";
pub const HELPER_NESTED_PATH: &str = "Contents/Helpers/GrokPtah Computer Use Helper.app";
pub const DEMO_TARGET_BUNDLE_ID: &str = "com.chriscase.grokptah.computer-use-demo";
pub const COMPUTER_USE_MINIMUM_OS: &str = "14.0";
pub const MAX_GUEST_IMAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_BUNDLE_FILES: usize = 4_096;
pub const MAX_HASHED_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const PACKAGED_HELPER_TEAM_ID_ENV: &str = "GROKPTAH_PACKAGED_HELPER_TEAM_ID";
pub const PACKAGED_HELPER_EXECUTABLE_DIGEST_ENV: &str =
    "GROKPTAH_PACKAGED_HELPER_EXECUTABLE_DIGEST";
pub const ISOLATED_GUEST_IMAGE_DIGEST_ENV: &str = "GROKPTAH_ISOLATED_GUEST_IMAGE_DIGEST";
pub const ISOLATED_GUEST_IMAGE_FORMAT_ENV: &str = "GROKPTAH_ISOLATED_GUEST_IMAGE_FORMAT";
pub const ISOLATED_GUEST_IMAGE_PROVENANCE_ENV: &str = "GROKPTAH_ISOLATED_GUEST_IMAGE_PROVENANCE";
pub const ISOLATED_GUEST_IMAGE_AUTHORIZATION_ENV: &str =
    "GROKPTAH_ISOLATED_GUEST_IMAGE_AUTHORIZATION_DIGEST";

const DOCUMENTED_IDENTITY_JSON: &str =
    include_str!("../../../../docs/schemas/grokptah-computer-use-package-identity.v1.json");
const CANONICAL_HELPER_ENTITLEMENTS: &str =
    include_str!("../../../../desktop/src-tauri/macos/ComputerUseHelper.entitlements");

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningClass {
    #[default]
    Uninspected,
    Unsigned,
    AdHoc,
    AppleDevelopment,
    DeveloperId,
    NotarizedDeveloperId,
}

impl SigningClass {
    pub fn counts_as_packaged_release(self) -> bool {
        self == Self::NotarizedDeveloperId
    }

    pub fn parse_codesign_display(output: &str) -> Self {
        let lower = output.to_ascii_lowercase();
        if lower.contains("source=notarized developer id")
            || (lower.contains("authority=developer id application") && lower.contains("notarized"))
        {
            return Self::NotarizedDeveloperId;
        }
        if lower.contains("authority=developer id application") {
            return Self::DeveloperId;
        }
        if lower.contains("authority=apple development") {
            return Self::AppleDevelopment;
        }
        if lower.contains("flags=0x2(adhoc)")
            || lower.contains("signature=adhoc")
            || lower.contains("authority=adhoc")
        {
            return Self::AdHoc;
        }
        if lower.contains("code has no signature")
            || lower.contains("code object is not signed")
            || lower.contains("not signed")
            || lower.contains("unsigned")
        {
            return Self::Unsigned;
        }
        Self::Uninspected
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedHelperObservation {
    pub bundle_id: String,
    pub executable_digest: String,
    pub team_id: String,
    pub designated_requirement: String,
    pub signing_class: SigningClass,
    pub entitlements_digest: String,
    pub notarization_source: Option<String>,
    pub stapled: bool,
    pub gatekeeper_accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestImageObservation {
    pub digest: String,
    pub manifest_id: String,
    pub provenance: String,
    pub format: String,
    pub size_bytes: u64,
    pub authorization_digest: String,
}

/// Host-pinned helper identity. Never copied from the inspected artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedHelper {
    pub bundle_id: String,
    pub team_id: String,
    pub executable_digest: String,
    pub designated_requirement: String,
    pub entitlements_digest: String,
}

impl ExpectedHelper {
    pub fn pinned(
        team_id: impl Into<String>,
        executable_digest: impl Into<String>,
    ) -> IsolatedResult<Self> {
        let team_id = team_id.into();
        let executable_digest = executable_digest.into();
        validate_id("team_id", &team_id)?;
        validate_digest("helper executable digest", &executable_digest)?;
        Ok(Self {
            bundle_id: HELPER_BUNDLE_ID.to_string(),
            designated_requirement: designated_requirement_for(&team_id),
            entitlements_digest: canonical_helper_entitlements_digest(),
            team_id,
            executable_digest,
        })
    }

    /// Pins from the canonical contract and operator-supplied env values.
    /// Missing pins fail closed; the inspected artifact cannot supply them.
    pub fn from_canonical_contract(team_id: Option<&str>) -> IsolatedResult<Self> {
        let team = team_id
            .map(str::to_string)
            .or_else(|| env::var(PACKAGED_HELPER_TEAM_ID_ENV).ok())
            .filter(|team| !team.is_empty())
            .ok_or_else(|| IsolatedError::unauthorized("canonical helper Team ID is not pinned"))?;
        let executable_digest = env::var(PACKAGED_HELPER_EXECUTABLE_DIGEST_ENV)
            .ok()
            .filter(|digest| !digest.is_empty())
            .ok_or_else(|| {
                IsolatedError::unauthorized("canonical helper executable digest is not pinned")
            })?;
        Self::pinned(team, executable_digest)
    }
}

/// Host-pinned guest-image identity. Never copied from the image sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedGuestImage {
    pub digest: String,
    pub format: String,
    pub provenance: String,
    pub authorization_digest: String,
}

impl ExpectedGuestImage {
    pub fn pinned(
        digest: impl Into<String>,
        format: impl Into<String>,
        provenance: impl Into<String>,
        authorization_digest: impl Into<String>,
    ) -> IsolatedResult<Self> {
        let expected = Self {
            digest: digest.into(),
            format: format.into(),
            provenance: provenance.into(),
            authorization_digest: authorization_digest.into(),
        };
        validate_digest("guest image digest", &expected.digest)?;
        validate_digest("guest authorization digest", &expected.authorization_digest)?;
        Ok(expected)
    }

    pub fn from_canonical_contract() -> IsolatedResult<Self> {
        let digest = env::var(ISOLATED_GUEST_IMAGE_DIGEST_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                IsolatedError::unauthorized("canonical guest-image digest is not pinned")
            })?;
        let format = env::var(ISOLATED_GUEST_IMAGE_FORMAT_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "raw".to_string());
        let provenance = env::var(ISOLATED_GUEST_IMAGE_PROVENANCE_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                IsolatedError::unauthorized("canonical guest-image provenance is not pinned")
            })?;
        let authorization_digest = env::var(ISOLATED_GUEST_IMAGE_AUTHORIZATION_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                IsolatedError::unauthorized(
                    "canonical guest-image authorization digest is not pinned",
                )
            })?;
        Self::pinned(digest, format, provenance, authorization_digest)
    }
}

pub fn documented_identity_json() -> &'static str {
    DOCUMENTED_IDENTITY_JSON
}

pub fn canonical_helper_entitlements_digest() -> String {
    sha256_hex(CANONICAL_HELPER_ENTITLEMENTS.as_bytes())
}

pub fn designated_requirement_for(team_id: &str) -> String {
    format!("identifier \"{HELPER_BUNDLE_ID}\" and certificate leaf[subject.OU] = {team_id}")
}

pub fn versions_compatible(app_version: &str, helper_version: &str) -> IsolatedResult<()> {
    let app = parse_semver(app_version)?;
    let helper = parse_semver(helper_version)?;
    if helper.0 != app.0 || helper.1 != app.1 {
        return Err(IsolatedError::unauthorized(
            "computer-use helper major.minor is incompatible with the app",
        ));
    }
    Ok(())
}

pub fn parse_semver(version: &str) -> IsolatedResult<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parse_semver_component(parts.next())?;
    let minor = parse_semver_component(parts.next())?;
    let patch = parse_semver_component(parts.next())?;
    if parts.next().is_some() {
        return Err(IsolatedError::invalid(
            "computer-use version must be exactly major.minor.patch",
        ));
    }
    Ok((major, minor, patch))
}

fn parse_semver_component(part: Option<&str>) -> IsolatedResult<u64> {
    let part = part.ok_or_else(|| {
        IsolatedError::invalid("computer-use version must be exactly major.minor.patch")
    })?;
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(IsolatedError::invalid(
            "computer-use version components must be decimal digits",
        ));
    }
    part.parse()
        .map_err(|_| IsolatedError::invalid("computer-use version component overflow"))
}

pub fn admit_packaged_helper(
    observation: &PackagedHelperObservation,
    expected: &ExpectedHelper,
) -> IsolatedResult<()> {
    validate_id("helper_bundle_id", &observation.bundle_id)?;
    validate_digest("helper executable digest", &observation.executable_digest)?;
    validate_id("team_id", &observation.team_id)?;
    validate_digest(
        "helper entitlements digest",
        &observation.entitlements_digest,
    )?;
    if observation.designated_requirement.trim().is_empty()
        || observation.designated_requirement.len() > 512
        || observation.designated_requirement.contains('\0')
    {
        return Err(IsolatedError::unauthorized(
            "helper designated requirement is missing",
        ));
    }
    if observation.bundle_id != expected.bundle_id {
        return Err(IsolatedError::unauthorized(
            "helper bundle id does not match the packaged helper identity",
        ));
    }
    if observation.team_id != expected.team_id {
        return Err(IsolatedError::unauthorized(
            "helper Team ID does not match the packaged helper identity",
        ));
    }
    if observation.executable_digest != expected.executable_digest {
        return Err(IsolatedError::unauthorized(
            "helper executable digest does not match the pinned helper identity",
        ));
    }
    if observation.entitlements_digest != expected.entitlements_digest {
        return Err(IsolatedError::unauthorized(
            "helper entitlements digest does not match the canonical helper entitlements",
        ));
    }
    if !observation
        .designated_requirement
        .contains(&expected.designated_requirement)
        && observation.designated_requirement != expected.designated_requirement
    {
        return Err(IsolatedError::unauthorized(
            "helper designated requirement does not match the pinned helper identity",
        ));
    }
    if !observation.signing_class.counts_as_packaged_release() {
        return Err(IsolatedError::unauthorized(
            "helper signing class is not notarized Developer ID",
        ));
    }
    if !observation.stapled || !observation.gatekeeper_accepted {
        return Err(IsolatedError::unauthorized(
            "helper notarization/stapling/Gatekeeper evidence is incomplete",
        ));
    }
    Ok(())
}

pub fn admit_guest_image(
    observation: &GuestImageObservation,
    expected: &ExpectedGuestImage,
) -> IsolatedResult<()> {
    validate_digest("guest image digest", &observation.digest)?;
    validate_id("guest manifest id", &observation.manifest_id)?;
    validate_digest(
        "guest authorization digest",
        &observation.authorization_digest,
    )?;
    if observation.provenance.trim().is_empty()
        || observation.provenance.contains('\0')
        || observation.provenance.contains("..")
        || observation.provenance.contains('/')
        || observation.provenance.contains('\\')
    {
        return Err(IsolatedError::unauthorized(
            "guest-image provenance is not a bound identity token",
        ));
    }
    if !matches!(
        observation.format.as_str(),
        "raw" | "rawdisk" | "apple-diskimage"
    ) {
        return Err(IsolatedError::unauthorized(
            "guest-image format is not an admitted isolated-visual format",
        ));
    }
    if observation.size_bytes == 0 || observation.size_bytes > MAX_GUEST_IMAGE_BYTES {
        return Err(IsolatedError::limit("guest-image size is outside bounds"));
    }
    if observation.digest != expected.digest {
        return Err(IsolatedError::unauthorized(
            "guest-image digest does not match the admitted manifest",
        ));
    }
    if observation.format != expected.format {
        return Err(IsolatedError::unauthorized(
            "guest-image format does not match the admitted manifest",
        ));
    }
    if observation.provenance != expected.provenance {
        return Err(IsolatedError::unauthorized(
            "guest-image provenance does not match the admitted manifest",
        ));
    }
    if observation.authorization_digest != expected.authorization_digest {
        return Err(IsolatedError::unauthorized(
            "guest-image authorization evidence does not match the admitted manifest",
        ));
    }
    Ok(())
}

/// Hash a bundle as a sorted file manifest. Directories and empty marker files
/// are not an authority digest. Symlinks fail closed.
pub fn hash_bundle_manifest(root: &Path) -> IsolatedResult<String> {
    if root.is_symlink() {
        return Err(IsolatedError::unauthorized(
            "packaged bundle root must not be a symlink",
        ));
    }
    if !root.is_dir() {
        return Err(IsolatedError::invalid(
            "packaged bundle path is not a directory",
        ));
    }
    let canonical = dunce::canonicalize(root).map_err(|error| {
        IsolatedError::invalid(format!("packaged bundle cannot be canonicalized ({error})"))
    })?;
    let mut entries = Vec::new();
    collect_bundle_files(&canonical, &canonical, &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, digest) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update([0]);
    }
    Ok(sha256_hex(&hasher.finalize()))
}

fn collect_bundle_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, String)>,
) -> IsolatedResult<()> {
    if out.len() >= MAX_BUNDLE_FILES {
        return Err(IsolatedError::limit("packaged bundle has too many files"));
    }
    let metadata = fs::symlink_metadata(current).map_err(|error| {
        IsolatedError::invalid(format!("packaged bundle member is unreadable ({error})"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(IsolatedError::unauthorized(
            "packaged bundle members must not be symlinks",
        ));
    }
    if metadata.file_type().is_dir() {
        for child in fs::read_dir(current).map_err(|error| {
            IsolatedError::invalid(format!("packaged bundle directory is unreadable ({error})"))
        })? {
            let child = child.map_err(|error| {
                IsolatedError::invalid(format!("packaged bundle entry is unreadable ({error})"))
            })?;
            collect_bundle_files(root, &child.path(), out)?;
        }
        return Ok(());
    }
    if !metadata.file_type().is_file() {
        return Err(IsolatedError::unauthorized(
            "packaged bundle contains a non-file member",
        ));
    }
    if metadata.len() > MAX_HASHED_FILE_BYTES {
        return Err(IsolatedError::limit(
            "packaged bundle file exceeds hash bound",
        ));
    }
    let relative = current
        .strip_prefix(root)
        .map_err(|_| IsolatedError::unauthorized("packaged bundle path escaped its root"))?
        .to_string_lossy()
        .replace('\\', "/");
    let mut file = fs::File::open(current).map_err(|error| {
        IsolatedError::invalid(format!("packaged bundle file cannot be opened ({error})"))
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file.read(&mut buf).map_err(|error| {
            IsolatedError::invalid(format!("packaged bundle file cannot be read ({error})"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    out.push((relative, sha256_hex(&hasher.finalize())));
    Ok(())
}

pub fn hash_file(path: &Path) -> IsolatedResult<String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        IsolatedError::invalid(format!("artifact file is unreadable ({error})"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(IsolatedError::unauthorized(
            "artifact path must not be a symlink",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(IsolatedError::invalid(
            "artifact path is not a regular file",
        ));
    }
    let max = if path.extension().is_some_and(|ext| ext == "img") {
        MAX_GUEST_IMAGE_BYTES
    } else {
        MAX_HASHED_FILE_BYTES
    };
    if metadata.len() > max {
        return Err(IsolatedError::limit("artifact file exceeds hash bound"));
    }
    let mut file = fs::File::open(path).map_err(|error| {
        IsolatedError::invalid(format!("artifact file cannot be opened ({error})"))
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file.read(&mut buf).map_err(|error| {
            IsolatedError::invalid(format!("artifact file cannot be read ({error})"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(sha256_hex(&hasher.finalize()))
}

pub fn inspect_codesign_fields(output: &str) -> (SigningClass, Option<String>, Option<String>) {
    let signing_class = SigningClass::parse_codesign_display(output);
    let identifier = output.lines().find_map(|line| {
        line.strip_prefix("Identifier=")
            .map(|value| value.trim().to_string())
    });
    let team_id = output.lines().find_map(|line| {
        line.strip_prefix("TeamIdentifier=")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value != "not set")
    });
    (signing_class, identifier, team_id)
}

/// Inspect an artifact root. Empty `helper.signed` / `guest.img.signed` files
/// are ignored and cannot admit launch. Expected identity is never returned
/// from the artifact; callers must pin it separately.
pub fn inspect_artifact_root(
    root: &Path,
) -> IsolatedResult<(
    Option<PackagedHelperObservation>,
    Option<GuestImageObservation>,
)> {
    if root.is_symlink() {
        return Err(IsolatedError::unauthorized(
            "artifact root must not be a symlink",
        ));
    }
    let helper = inspect_helper_bundle(root)?;
    let image = inspect_guest_image(root)?;
    Ok((helper, image))
}

fn inspect_helper_bundle(root: &Path) -> IsolatedResult<Option<PackagedHelperObservation>> {
    let nested = root.join("GrokPtah Computer Use Helper.app");
    let helper_root = if nested.is_dir() {
        nested
    } else {
        root.join("helper.app")
    };
    if !helper_root.exists() {
        return Ok(None);
    }
    if helper_root.join("codesign-display.txt").exists()
        || helper_root.join("Contents/codesign-display.txt").exists()
    {
        return Err(IsolatedError::unauthorized(
            "sidecar codesign-display.txt is not helper identity",
        ));
    }
    let executable = helper_root.join("Contents/MacOS").join(HELPER_EXECUTABLE);
    if !executable.is_file() {
        return Err(IsolatedError::unauthorized(
            "helper executable is missing from the inspected bundle",
        ));
    }
    let plist = fs::read_to_string(helper_root.join("Contents/Info.plist")).unwrap_or_default();
    if !plist.contains(HELPER_BUNDLE_ID) {
        return Err(IsolatedError::unauthorized(
            "helper Info.plist does not declare the packaged helper bundle id",
        ));
    }
    let probe = probe_helper_codesign(&helper_root)?;
    let combined = format!("{}\n{}", probe.display, probe.requirement);
    let (signing_class, identifier, team_id) = inspect_codesign_fields(&combined);
    let bundle_id = identifier.unwrap_or_default();
    if bundle_id != HELPER_BUNDLE_ID {
        return Err(IsolatedError::unauthorized(
            "codesign identifier does not match the packaged helper bundle id",
        ));
    }
    let designated_requirement =
        parse_designated_requirement(&probe.requirement).ok_or_else(|| {
            IsolatedError::unauthorized("helper designated requirement was not observed")
        })?;
    let entitlements_digest = if probe.entitlements.trim().is_empty() {
        sha256_hex(b"")
    } else {
        sha256_hex(probe.entitlements.trim().as_bytes())
    };
    let stapled = probe.stapled;
    let observation = PackagedHelperObservation {
        bundle_id,
        executable_digest: hash_file(&executable)?,
        team_id: team_id.unwrap_or_default(),
        designated_requirement,
        signing_class,
        entitlements_digest,
        notarization_source: combined
            .to_ascii_lowercase()
            .contains("notarized")
            .then(|| "notarized_developer_id".to_string()),
        stapled,
        gatekeeper_accepted: stapled && signing_class.counts_as_packaged_release(),
    };
    let _ = hash_bundle_manifest(&helper_root)?;
    Ok(Some(observation))
}

struct CodesignProbe {
    display: String,
    requirement: String,
    entitlements: String,
    stapled: bool,
}

fn probe_helper_codesign(helper_root: &Path) -> IsolatedResult<CodesignProbe> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = helper_root;
        return Err(IsolatedError::unavailable(
            "codesign inspection requires macOS",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let codesign = Path::new("/usr/bin/codesign");
        if !codesign.is_file() {
            return Err(IsolatedError::unavailable(
                "codesign is required for packaged helper inspection",
            ));
        }
        let helper_path = helper_root.to_string_lossy().into_owned();
        let display = run_readonly(codesign, &["--display", "--verbose=4", &helper_path])?;
        let requirement = run_readonly(codesign, &["-d", "-r", "-", &helper_path])?;
        let entitlements = run_readonly(
            codesign,
            &["--display", "--entitlements", ":-", &helper_path],
        )?;
        let stapled = stapler_ticket_present(helper_root);
        Ok(CodesignProbe {
            display,
            requirement,
            entitlements,
            stapled,
        })
    }
}

fn stapler_ticket_present(helper_root: &Path) -> bool {
    let stapler = Path::new("/usr/bin/stapler");
    if !stapler.is_file() {
        return false;
    }
    let helper_path = helper_root.to_string_lossy().into_owned();
    run_readonly(stapler, &["validate", &helper_path])
        .map(|output| {
            let lower = output.to_ascii_lowercase();
            lower.contains("the validate action worked") || lower.contains("ticket")
        })
        .unwrap_or(false)
}

fn run_readonly(binary: &Path, args: &[&str]) -> IsolatedResult<String> {
    if args.iter().any(|arg| {
        *arg == "--sign"
            || *arg == "-s"
            || *arg == "--remove-signature"
            || arg.starts_with("--sign=")
    }) {
        return Err(IsolatedError::forbidden(
            "codesign mutation is not permitted during inspection",
        ));
    }
    let output = Command::new(binary).args(args).output().map_err(|error| {
        IsolatedError::unavailable(format!("{} cannot be executed ({error})", binary.display()))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!("{stdout}{stderr}"))
}

fn parse_designated_requirement(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("designated =>")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn inspect_guest_image(root: &Path) -> IsolatedResult<Option<GuestImageObservation>> {
    let image = root.join("guest.img");
    if !image.exists() {
        return Ok(None);
    }
    let digest = hash_file(&image)?;
    let size_bytes = fs::metadata(&image)
        .map_err(|error| IsolatedError::invalid(format!("guest image metadata failed ({error})")))?
        .len();
    let format = infer_guest_format(&image)?;
    let manifest_path = root.join("guest.img.manifest.json");
    if !manifest_path.is_file() {
        return Err(IsolatedError::unauthorized(
            "guest-image manifest is missing; marker files are not identity",
        ));
    }
    let raw = fs::read_to_string(&manifest_path).map_err(|error| {
        IsolatedError::invalid(format!("guest-image manifest is unreadable ({error})"))
    })?;
    let manifest: GuestImageManifest = serde_json::from_str(&raw)
        .map_err(|_| IsolatedError::invalid("guest-image manifest is not valid JSON"))?;
    if manifest.digest != digest {
        return Err(IsolatedError::unauthorized(
            "guest-image sidecar digest does not match the hashed image",
        ));
    }
    if manifest.format != format {
        return Err(IsolatedError::unauthorized(
            "guest-image sidecar format does not match the inferred image format",
        ));
    }
    Ok(Some(GuestImageObservation {
        digest,
        manifest_id: manifest.manifest_id,
        provenance: manifest.provenance,
        format,
        size_bytes,
        authorization_digest: manifest.authorization_digest,
    }))
}

fn infer_guest_format(path: &Path) -> IsolatedResult<String> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("img") | Some("raw") => Ok("raw".into()),
        Some("dmg") => Ok("apple-diskimage".into()),
        _ => Err(IsolatedError::unauthorized(
            "guest-image format cannot be inferred from the artifact",
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GuestImageManifest {
    manifest_id: String,
    digest: String,
    provenance: String,
    format: String,
    authorization_digest: String,
}

pub fn write_unsigned_helper_bundle(root: &Path) -> IsolatedResult<std::path::PathBuf> {
    let helper_root = root.join("GrokPtah Computer Use Helper.app");
    fs::create_dir_all(helper_root.join("Contents/MacOS")).map_err(|error| {
        IsolatedError::internal(format!("fixture helper cannot be created ({error})"))
    })?;
    fs::write(
        helper_root.join("Contents/Info.plist"),
        format!("<string>{HELPER_BUNDLE_ID}</string>"),
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
    fs::write(
        helper_root.join("Contents/MacOS").join(HELPER_EXECUTABLE),
        b"helper-executable-bytes",
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
    Ok(helper_root)
}

pub fn write_planted_codesign_display(root: &Path, team_id: &str) -> IsolatedResult<()> {
    let helper_root = write_unsigned_helper_bundle(root)?;
    fs::write(
        helper_root.join("codesign-display.txt"),
        format!(
            "Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier={team_id}\nAuthority=Developer ID Application: Example ({team_id})\nsource=Notarized Developer ID\nGrokPtah.app: accepted\nstapled ticket\n"
        ),
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
    Ok(())
}

pub fn write_guest_image_claim(
    root: &Path,
    image_bytes: &[u8],
) -> IsolatedResult<GuestImageObservation> {
    fs::write(root.join("guest.img"), image_bytes)
        .map_err(|error| IsolatedError::internal(error.to_string()))?;
    let digest = hash_file(&root.join("guest.img"))?;
    let authorization = sha256_hex(b"guest-authorization-v1");
    let manifest = serde_json::json!({
        "manifestId": "guest-manifest-1",
        "digest": digest,
        "provenance": "test-provenance",
        "format": "raw",
        "authorizationDigest": authorization,
    });
    fs::write(
        root.join("guest.img.manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
    inspect_guest_image(root)?.ok_or_else(|| IsolatedError::internal("fixture image missing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn admitted_pair(team: &str) -> (PackagedHelperObservation, ExpectedHelper) {
        let executable_digest = "a".repeat(64);
        let expected = ExpectedHelper::pinned(team, executable_digest.clone()).unwrap();
        let observation = PackagedHelperObservation {
            bundle_id: HELPER_BUNDLE_ID.into(),
            executable_digest,
            team_id: team.into(),
            designated_requirement: expected.designated_requirement.clone(),
            signing_class: SigningClass::NotarizedDeveloperId,
            entitlements_digest: expected.entitlements_digest.clone(),
            notarization_source: Some("notarized_developer_id".into()),
            stapled: true,
            gatekeeper_accepted: true,
        };
        (observation, expected)
    }

    #[test]
    fn empty_marker_files_do_not_admit() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("helper.signed"), b"").unwrap();
        fs::write(dir.path().join("guest.img.signed"), b"").unwrap();
        let (helper, image) = inspect_artifact_root(dir.path()).unwrap();
        assert!(helper.is_none());
        assert!(image.is_none());
    }

    #[test]
    fn planted_codesign_display_is_rejected() {
        let dir = tempdir().unwrap();
        write_planted_codesign_display(dir.path(), "TEAMID1234").unwrap();
        let error = inspect_artifact_root(dir.path()).unwrap_err();
        assert_eq!(error.code, crate::error::IsolatedErrorCode::Unauthorized);
        assert!(error.message.contains("codesign-display.txt"));
    }

    #[test]
    fn wrong_team_bundle_digest_requirement_and_entitlements_fail_closed() {
        let (mut helper, expected) = admitted_pair("TEAMID1234");
        helper.team_id = "OTHERTEAM".into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        let (mut helper, expected) = admitted_pair("TEAMID1234");
        helper.bundle_id = APP_BUNDLE_ID.into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        let (mut helper, expected) = admitted_pair("TEAMID1234");
        helper.executable_digest = "c".repeat(64);
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        helper.executable_digest = expected.executable_digest.clone();
        helper.designated_requirement = "identifier \"com.example.other\"".into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        let (mut helper, expected) = admitted_pair("TEAMID1234");
        helper.entitlements_digest = "b".repeat(64);
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        let (mut helper, expected) = admitted_pair("TEAMID1234");
        helper.signing_class = SigningClass::AdHoc;
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        admit_packaged_helper(&admitted_pair("TEAMID1234").0, &expected).unwrap();
    }

    #[test]
    fn guest_image_wrong_digest_or_provenance_fails_closed() {
        let expected =
            ExpectedGuestImage::pinned("d".repeat(64), "raw", "test-provenance", "e".repeat(64))
                .unwrap();
        let mut image = GuestImageObservation {
            digest: "d".repeat(64),
            manifest_id: "guest-manifest-1".into(),
            provenance: "test-provenance".into(),
            format: "raw".into(),
            size_bytes: 16,
            authorization_digest: "e".repeat(64),
        };
        admit_guest_image(&image, &expected).unwrap();
        image.digest = "f".repeat(64);
        assert!(admit_guest_image(&image, &expected).is_err());
        image.digest = "d".repeat(64);
        image.provenance = "other-provenance".into();
        assert!(admit_guest_image(&image, &expected).is_err());
        image.provenance = "/tmp/escape".into();
        assert!(admit_guest_image(&image, &expected).is_err());
    }

    #[test]
    fn guest_sidecar_cannot_pin_its_own_expected_identity() {
        let dir = tempdir().unwrap();
        let observation = write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
        let expected = ExpectedGuestImage::from_canonical_contract();
        assert!(expected.is_err());
        let pinned = ExpectedGuestImage::pinned(
            "0".repeat(64),
            "raw",
            "test-provenance",
            observation.authorization_digest.clone(),
        )
        .unwrap();
        assert!(admit_guest_image(&observation, &pinned).is_err());
    }

    #[test]
    fn hash_bundle_rejects_symlinks_and_does_not_hash_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("Contents")).unwrap();
        fs::write(dir.path().join("Contents/Info.plist"), b"ok").unwrap();
        hash_bundle_manifest(dir.path()).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Contents/Info.plist", dir.path().join("link")).unwrap();
            assert!(hash_bundle_manifest(dir.path()).is_err());
        }
        assert!(hash_file(dir.path()).is_err());
    }

    #[test]
    fn malformed_semver_is_rejected() {
        assert!(parse_semver("0.1.bad.extra").is_err());
        assert!(parse_semver("0.1").is_err());
        assert!(parse_semver("0.1.0.1").is_err());
        parse_semver("0.1.0").unwrap();
        versions_compatible("0.1.0", "0.1.9").unwrap();
        assert!(versions_compatible("0.1.0", "0.2.0").is_err());
    }

    #[test]
    fn unsigned_or_planted_fixture_cannot_admit() {
        let dir = tempdir().unwrap();
        write_unsigned_helper_bundle(dir.path()).unwrap();
        write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
        match inspect_artifact_root(dir.path()) {
            Ok((helper, image)) => {
                assert!(image.is_some());
                if let Some(helper) = helper {
                    let expected =
                        ExpectedHelper::pinned("TEAMID1234", helper.executable_digest.clone())
                            .unwrap();
                    assert!(admit_packaged_helper(&helper, &expected).is_err());
                    assert!(!helper.signing_class.counts_as_packaged_release());
                }
            }
            Err(error) => {
                assert!(
                    error.code == crate::error::IsolatedErrorCode::Unauthorized
                        || error.code == crate::error::IsolatedErrorCode::BackendUnavailable
                );
            }
        }
        let planted = tempdir().unwrap();
        write_planted_codesign_display(planted.path(), "TEAMID1234").unwrap();
        assert!(inspect_artifact_root(planted.path()).is_err());
    }

    #[test]
    fn documented_identity_json_matches_constants() {
        assert!(DOCUMENTED_IDENTITY_JSON.contains(HELPER_BUNDLE_ID));
        assert!(DOCUMENTED_IDENTITY_JSON.contains(APP_BUNDLE_ID));
        assert!(!canonical_helper_entitlements_digest().is_empty());
    }

    #[test]
    fn canonical_contract_does_not_read_the_artifact() {
        let dir = tempdir().unwrap();
        write_planted_codesign_display(dir.path(), "TEAMID1234").unwrap();
        assert!(ExpectedHelper::from_canonical_contract(None).is_err());
        assert!(ExpectedGuestImage::from_canonical_contract().is_err());
    }
}
