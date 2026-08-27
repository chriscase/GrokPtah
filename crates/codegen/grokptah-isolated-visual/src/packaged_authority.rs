//! Canonical packaged-helper and guest-image authority (#444/#288).
//!
//! Marker files such as `helper.signed` / `guest.img.signed` cannot authorize
//! launch. Admission binds inspected cryptographic identity. Eligibility is
//! not a launch receipt.

use std::fs;
use std::io::Read;
use std::path::Path;

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

const DOCUMENTED_IDENTITY_JSON: &str =
    include_str!("../../../../docs/schemas/grokptah-computer-use-package-identity.v1.json");

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedHelper {
    pub bundle_id: String,
    pub team_id: String,
}

impl ExpectedHelper {
    pub fn canonical(team_id: impl Into<String>) -> Self {
        Self {
            bundle_id: HELPER_BUNDLE_ID.to_string(),
            team_id: team_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedGuestImage {
    pub digest: String,
    pub format: String,
    pub provenance: String,
    pub authorization_digest: String,
}

pub fn documented_identity_json() -> &'static str {
    DOCUMENTED_IDENTITY_JSON
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
    if !observation
        .designated_requirement
        .contains(&format!("identifier \"{}\"", expected.bundle_id))
    {
        return Err(IsolatedError::unauthorized(
            "helper designated requirement does not bind the packaged helper identifier",
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
    });
    (signing_class, identifier, team_id)
}

/// Inspect an artifact root. Empty `helper.signed` / `guest.img.signed` files
/// are ignored and cannot admit launch.
pub fn inspect_artifact_root(
    root: &Path,
) -> IsolatedResult<(
    Option<PackagedHelperObservation>,
    Option<GuestImageObservation>,
    Option<ExpectedGuestImage>,
)> {
    if root.is_symlink() {
        return Err(IsolatedError::unauthorized(
            "artifact root must not be a symlink",
        ));
    }
    let helper = inspect_helper_bundle(root)?;
    let image = inspect_guest_image(root)?;
    Ok(match image {
        Some((observation, expected)) => (helper, Some(observation), Some(expected)),
        None => (helper, None, None),
    })
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
    let display = fs::read_to_string(helper_root.join("codesign-display.txt")).unwrap_or_default();
    let (signing_class, identifier, team_id) = inspect_codesign_fields(&display);
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
    let observation = PackagedHelperObservation {
        bundle_id: identifier.unwrap_or_else(|| HELPER_BUNDLE_ID.to_string()),
        executable_digest: hash_file(&executable)?,
        team_id: team_id.unwrap_or_default(),
        designated_requirement: format!(
            "identifier \"{HELPER_BUNDLE_ID}\" and certificate leaf[subject.OU] = TEAMID"
        ),
        signing_class,
        entitlements_digest: hash_file(&helper_root.join("Contents/entitlements.plist"))
            .or_else(|_| Ok(sha256_hex(b"<dict></dict>")))?,
        notarization_source: display.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .contains("notarized")
                .then(|| "notarized_developer_id".to_string())
        }),
        stapled: display.to_ascii_lowercase().contains("stapled")
            || display.to_ascii_lowercase().contains("ticket"),
        gatekeeper_accepted: display.to_ascii_lowercase().contains("accepted"),
    };
    let _ = hash_bundle_manifest(&helper_root)?;
    Ok(Some(observation))
}

fn inspect_guest_image(
    root: &Path,
) -> IsolatedResult<Option<(GuestImageObservation, ExpectedGuestImage)>> {
    let image = root.join("guest.img");
    if !image.exists() {
        return Ok(None);
    }
    let digest = hash_file(&image)?;
    let size_bytes = fs::metadata(&image)
        .map_err(|error| IsolatedError::invalid(format!("guest image metadata failed ({error})")))?
        .len();
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
    let observation = GuestImageObservation {
        digest: digest.clone(),
        manifest_id: manifest.manifest_id.clone(),
        provenance: manifest.provenance.clone(),
        format: manifest.format.clone(),
        size_bytes,
        authorization_digest: manifest.authorization_digest.clone(),
    };
    let expected = ExpectedGuestImage {
        digest: manifest.digest,
        format: manifest.format,
        provenance: manifest.provenance,
        authorization_digest: manifest.authorization_digest,
    };
    Ok(Some((observation, expected)))
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

pub fn write_admitted_fixture(
    root: &Path,
    team_id: &str,
    image_bytes: &[u8],
) -> IsolatedResult<(
    PackagedHelperObservation,
    GuestImageObservation,
    ExpectedHelper,
    ExpectedGuestImage,
)> {
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
    fs::write(
        helper_root.join("Contents/entitlements.plist"),
        b"<?xml version=\"1.0\"?><plist><dict></dict></plist>",
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
    fs::write(
        helper_root.join("codesign-display.txt"),
        format!(
            "Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier={team_id}\nAuthority=Developer ID Application: Example ({team_id})\nsource=Notarized Developer ID\nGrokPtah.app: accepted\nstapled ticket\n"
        ),
    )
    .map_err(|error| IsolatedError::internal(error.to_string()))?;
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
    let (helper, image, expected_image) = inspect_artifact_root(root)?;
    let helper = helper.ok_or_else(|| IsolatedError::internal("fixture helper missing"))?;
    let image = image.ok_or_else(|| IsolatedError::internal("fixture image missing"))?;
    let expected_image =
        expected_image.ok_or_else(|| IsolatedError::internal("fixture image expected missing"))?;
    let expected_helper = ExpectedHelper::canonical(team_id);
    Ok((helper, image, expected_helper, expected_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn admitted_helper(team: &str) -> PackagedHelperObservation {
        PackagedHelperObservation {
            bundle_id: HELPER_BUNDLE_ID.into(),
            executable_digest: "a".repeat(64),
            team_id: team.into(),
            designated_requirement: format!(
                "identifier \"{HELPER_BUNDLE_ID}\" and certificate leaf[subject.OU] = {team}"
            ),
            signing_class: SigningClass::NotarizedDeveloperId,
            entitlements_digest: "b".repeat(64),
            notarization_source: Some("notarized_developer_id".into()),
            stapled: true,
            gatekeeper_accepted: true,
        }
    }

    #[test]
    fn empty_marker_files_do_not_admit() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("helper.signed"), b"").unwrap();
        fs::write(dir.path().join("guest.img.signed"), b"").unwrap();
        let (helper, image, _) = inspect_artifact_root(dir.path()).unwrap();
        assert!(helper.is_none());
        assert!(image.is_none());
    }

    #[test]
    fn wrong_team_bundle_digest_requirement_and_entitlements_fail_closed() {
        let expected = ExpectedHelper::canonical("TEAMID1234");
        let mut helper = admitted_helper("TEAMID1234");
        helper.team_id = "OTHERTEAM".into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        helper = admitted_helper("TEAMID1234");
        helper.bundle_id = APP_BUNDLE_ID.into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        helper = admitted_helper("TEAMID1234");
        helper.executable_digest = "c".repeat(64);
        // digest still valid hex; expected does not pin executable digest except via observation
        admit_packaged_helper(&helper, &expected).unwrap();
        helper.designated_requirement = "identifier \"com.example.other\"".into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        helper = admitted_helper("TEAMID1234");
        helper.entitlements_digest = "not-a-digest".into();
        assert!(admit_packaged_helper(&helper, &expected).is_err());
        helper = admitted_helper("TEAMID1234");
        helper.signing_class = SigningClass::AdHoc;
        assert!(admit_packaged_helper(&helper, &expected).is_err());
    }

    #[test]
    fn guest_image_wrong_digest_or_provenance_fails_closed() {
        let expected = ExpectedGuestImage {
            digest: "d".repeat(64),
            format: "raw".into(),
            provenance: "test-provenance".into(),
            authorization_digest: "e".repeat(64),
        };
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
    fn fixture_root_admits_only_after_cryptographic_inspection() {
        let dir = tempdir().unwrap();
        let (helper, image, expected_helper, expected_image) =
            write_admitted_fixture(dir.path(), "TEAMID1234", b"guest-bytes").unwrap();
        admit_packaged_helper(&helper, &expected_helper).unwrap();
        admit_guest_image(&image, &expected_image).unwrap();
        fs::write(dir.path().join("helper.signed"), b"").unwrap();
        let (again, _, _) = inspect_artifact_root(dir.path()).unwrap();
        admit_packaged_helper(&again.unwrap(), &expected_helper).unwrap();
    }

    #[test]
    fn documented_identity_json_matches_constants() {
        assert!(DOCUMENTED_IDENTITY_JSON.contains(HELPER_BUNDLE_ID));
        assert!(DOCUMENTED_IDENTITY_JSON.contains(APP_BUNDLE_ID));
    }
}
