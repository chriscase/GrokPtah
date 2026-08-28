//! Packaged helper and guest-image admission.
//!
//! Two rules govern everything here:
//!
//! 1. Identity is what the operating system verifies ([`CodeIdentityProbe`]),
//!    never what a file inside the artifact claims.
//! 2. Expectations come from an operator [`PackagedTrustRoot`] that lives
//!    outside the artifact, never from the artifact being inspected.
//!
//! Together those make admission a real comparison rather than a tautology.
//! Every failure is fail-closed; there is no "admit with what was found".

use std::fs;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::code_identity::{CodeIdentityProbe, ObservedCodeIdentity, SigningClass};
use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{hex_encode, sha256_hex, validate_digest, validate_id};
use crate::trust_root::{validate_provenance, PackagedTrustRoot};

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

/// Filenames that have historically been mistaken for signing evidence. They
/// are never read, and their presence is surfaced so a reviewer can see the
/// artifact tried to attest to itself.
pub const SELF_ATTESTATION_FILENAMES: &[&str] = &[
    "codesign-display.txt",
    "codesign.txt",
    "helper.signed",
    "guest.img.signed",
    "notarization.txt",
    "signature.txt",
];

const DOCUMENTED_IDENTITY_JSON: &str =
    include_str!("../../../../docs/schemas/grokptah-computer-use-package-identity.v1.json");

pub fn documented_identity_json() -> &'static str {
    DOCUMENTED_IDENTITY_JSON
}

/// What was observed about the helper bundle. Every field either comes from
/// the OS probe or is computed by hashing bytes on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedHelperObservation {
    /// Which probe produced [`Self::code_identity`].
    pub probe_id: String,
    pub bundle_path: String,
    pub executable_digest: String,
    pub entitlements_digest: String,
    pub bundle_manifest_digest: String,
    pub info_plist_bundle_id: Option<String>,
    pub code_identity: ObservedCodeIdentity,
    /// Self-attestation files found in the tree. Never consulted; recorded so
    /// their presence is visible.
    #[serde(default)]
    pub ignored_self_attestations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestImageObservation {
    pub image_path: String,
    pub digest: String,
    pub format: String,
    pub size_bytes: u64,
}

/// Admitted helper identity. Constructing one is only possible through
/// [`admit_packaged_helper`], so its existence is itself the admission proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmittedHelperIdentity {
    pub bundle_id: String,
    pub team_id: String,
    pub designated_requirement: String,
    pub signing_class: SigningClass,
    pub executable_digest: String,
    pub entitlements_digest: String,
    pub bundle_manifest_digest: String,
    /// Digest over the trust root's helper anchor, so evidence records which
    /// expectation was satisfied.
    pub trust_anchor_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmittedGuestImage {
    pub digest: String,
    pub format: String,
    pub provenance: String,
    pub authorization_digest: String,
    pub size_bytes: u64,
    pub trust_anchor_digest: String,
}

/// Admit the observed helper against the operator trust root.
///
/// The expected designated requirement, Team ID, bundle identifier, and
/// entitlements digest all come from `trust_root`. Nothing in `observation`
/// supplies its own expectation.
pub fn admit_packaged_helper(
    observation: &PackagedHelperObservation,
    trust_root: &PackagedTrustRoot,
) -> IsolatedResult<AdmittedHelperIdentity> {
    trust_root.validate()?;
    let expected = &trust_root.helper;
    validate_digest("helper executable digest", &observation.executable_digest)?;
    validate_digest(
        "helper entitlements digest",
        &observation.entitlements_digest,
    )?;
    validate_digest(
        "helper bundle manifest digest",
        &observation.bundle_manifest_digest,
    )?;
    if observation.probe_id.trim().is_empty() {
        return Err(IsolatedError::unauthorized(
            "helper code identity has no probe attribution",
        ));
    }

    let identity = &observation.code_identity;
    let observed_identifier = identity.identifier.as_deref().ok_or_else(|| {
        IsolatedError::unauthorized("OS did not report a code-signing identifier for the helper")
    })?;
    let observed_team = identity.team_id.as_deref().ok_or_else(|| {
        IsolatedError::unauthorized("OS did not report a Team Identifier for the helper")
    })?;
    let observed_requirement = identity.designated_requirement.as_deref().ok_or_else(|| {
        IsolatedError::unauthorized(
            "OS did not derive a designated requirement for the helper; \
             a requirement is never synthesized from an observed Team ID",
        )
    })?;

    validate_id("observed helper identifier", observed_identifier)?;
    if observed_identifier != expected.bundle_id {
        return Err(IsolatedError::unauthorized(
            "helper code-signing identifier does not match the trust root",
        ));
    }
    if observed_team != expected.team_id {
        return Err(IsolatedError::unauthorized(
            "helper Team ID does not match the trust root",
        ));
    }
    // Exact equality against the operator-declared requirement. A `contains`
    // check would admit any requirement that merely mentions the identifier.
    if normalize_requirement(observed_requirement)
        != normalize_requirement(&expected.designated_requirement)
    {
        return Err(IsolatedError::unauthorized(
            "helper designated requirement does not match the trust root",
        ));
    }
    if observation.entitlements_digest != expected.entitlements_sha256 {
        return Err(IsolatedError::unauthorized(
            "helper entitlements digest does not match the trust root",
        ));
    }
    if let Some(declared) = &observation.info_plist_bundle_id {
        if declared != &expected.bundle_id {
            return Err(IsolatedError::unauthorized(
                "helper Info.plist declares a different bundle identifier",
            ));
        }
    }
    if !identity.signing_class.counts_as_packaged_release() {
        return Err(IsolatedError::unauthorized(format!(
            "helper signing class {} is not notarized Developer ID",
            identity.signing_class.as_str()
        )));
    }
    if !identity.gatekeeper_accepted || !identity.stapled || !identity.captured.verify_ok {
        return Err(IsolatedError::unauthorized(
            "helper Gatekeeper assessment, stapling, or codesign verification is incomplete",
        ));
    }

    Ok(AdmittedHelperIdentity {
        bundle_id: expected.bundle_id.clone(),
        team_id: expected.team_id.clone(),
        designated_requirement: expected.designated_requirement.clone(),
        signing_class: identity.signing_class,
        executable_digest: observation.executable_digest.clone(),
        entitlements_digest: observation.entitlements_digest.clone(),
        bundle_manifest_digest: observation.bundle_manifest_digest.clone(),
        trust_anchor_digest: anchor_digest(expected),
    })
}

/// Admit the observed guest image against the operator trust root.
///
/// The expected digest, format, provenance, and authorization digest come from
/// `trust_root`. A manifest shipped beside the image cannot supply any of them.
pub fn admit_guest_image(
    observation: &GuestImageObservation,
    trust_root: &PackagedTrustRoot,
) -> IsolatedResult<AdmittedGuestImage> {
    trust_root.validate()?;
    let expected = &trust_root.guest_image;
    validate_digest("guest image digest", &observation.digest)?;
    if observation.size_bytes == 0 || observation.size_bytes > MAX_GUEST_IMAGE_BYTES {
        return Err(IsolatedError::limit("guest-image size is outside bounds"));
    }
    if observation.digest != expected.digest_sha256 {
        return Err(IsolatedError::unauthorized(
            "guest-image digest does not match the trust root",
        ));
    }
    if observation.format != expected.format {
        return Err(IsolatedError::unauthorized(
            "guest-image format does not match the trust root",
        ));
    }
    validate_provenance(&expected.provenance)?;
    Ok(AdmittedGuestImage {
        digest: expected.digest_sha256.clone(),
        format: expected.format.clone(),
        provenance: expected.provenance.clone(),
        authorization_digest: expected.authorization_sha256.clone(),
        size_bytes: observation.size_bytes,
        trust_anchor_digest: sha256_hex(
            serde_json::to_vec(expected).unwrap_or_default().as_slice(),
        ),
    })
}

fn anchor_digest(anchor: &crate::trust_root::HelperTrustAnchor) -> String {
    sha256_hex(serde_json::to_vec(anchor).unwrap_or_default().as_slice())
}

/// Requirement strings differ only in incidental whitespace between tokens.
fn normalize_requirement(requirement: &str) -> String {
    requirement.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Inspect the helper bundle at `helper_root` using an OS-verifiable probe.
///
/// Nothing in the bundle is treated as an assertion about its own signature.
pub fn inspect_helper_bundle(
    helper_root: &Path,
    probe: &dyn CodeIdentityProbe,
) -> IsolatedResult<PackagedHelperObservation> {
    if helper_root.is_symlink() {
        return Err(IsolatedError::unauthorized(
            "helper bundle root must not be a symlink",
        ));
    }
    if !helper_root.is_dir() {
        return Err(IsolatedError::invalid(
            "helper bundle path is not a directory",
        ));
    }
    if !probe.available() {
        return Err(IsolatedError::unsupported(
            "no OS code-signing probe is available; helper identity cannot be established",
        ));
    }
    let executable = helper_root.join("Contents/MacOS").join(HELPER_EXECUTABLE);
    if !executable.is_file() {
        return Err(IsolatedError::unauthorized(
            "helper executable is missing from the inspected bundle",
        ));
    }
    // A missing or symlinked entitlements file is fail-closed. It must never
    // fall back to the digest of a synthesized empty plist.
    let entitlements_digest = hash_file(&helper_root.join("Contents/entitlements.plist"))?;
    let executable_digest = hash_file(&executable)?;
    let bundle_manifest_digest = hash_bundle_manifest(helper_root)?;
    let info_plist_bundle_id = read_info_plist_bundle_id(&helper_root.join("Contents/Info.plist"));
    let code_identity = probe.inspect(helper_root)?;
    Ok(PackagedHelperObservation {
        probe_id: probe.probe_id().to_string(),
        bundle_path: helper_root.to_string_lossy().into_owned(),
        executable_digest,
        entitlements_digest,
        bundle_manifest_digest,
        info_plist_bundle_id,
        code_identity,
        ignored_self_attestations: find_self_attestations(helper_root),
    })
}

pub fn inspect_guest_image(image_path: &Path) -> IsolatedResult<GuestImageObservation> {
    let metadata = fs::symlink_metadata(image_path)
        .map_err(|error| IsolatedError::invalid(format!("guest image is unreadable ({error})")))?;
    if metadata.file_type().is_symlink() {
        return Err(IsolatedError::unauthorized(
            "guest image must not be a symlink",
        ));
    }
    Ok(GuestImageObservation {
        image_path: image_path.to_string_lossy().into_owned(),
        digest: hash_file(image_path)?,
        // Format is not inferable from bytes; the trust root declares it and
        // the observation simply echoes the declared value for comparison.
        format: "raw".into(),
        size_bytes: metadata.len(),
    })
}

/// Locate self-attestation files so their presence is reported, never read.
pub fn find_self_attestations(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if SELF_ATTESTATION_FILENAMES
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&name))
        {
            found.push(name);
        }
    }
    found.sort();
    found
}

fn read_info_plist_bundle_id(path: &Path) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let key = body.find("<key>CFBundleIdentifier</key>")?;
    let rest = &body[key..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].trim().to_string())
}

/// Hash a bundle as a sorted file manifest. Directories and marker files are
/// not an authority digest; symlinks fail closed.
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
    Ok(hex_encode(&hasher.finalize()))
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
    out.push((relative, hash_file(current)?));
    Ok(())
}

/// SHA-256 of a regular file. Symlinks and non-files fail closed; there is no
/// fallback digest for a file that could not be read.
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
    Ok(hex_encode(&hasher.finalize()))
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

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::code_identity::{captured_fixture, parse_observed_identity};

    /// A probe that returns a caller-supplied identity. Used to drive
    /// admission without running codesign; it is `cfg(test)` only, so no
    /// production path can substitute an attestation for an OS verdict.
    #[derive(Debug)]
    pub struct FixtureProbe {
        pub identity: ObservedCodeIdentity,
        pub available: bool,
    }

    impl CodeIdentityProbe for FixtureProbe {
        fn probe_id(&self) -> &'static str {
            "fixture_probe_v1"
        }
        fn available(&self) -> bool {
            self.available
        }
        fn inspect(&self, _bundle: &Path) -> IsolatedResult<ObservedCodeIdentity> {
            if !self.available {
                return Err(IsolatedError::unsupported("fixture probe unavailable"));
            }
            Ok(self.identity.clone())
        }
    }

    pub fn notarized_identity(team: &str, bundle_id: &str) -> ObservedCodeIdentity {
        let display = format!(
            "Identifier={bundle_id}\nTeamIdentifier={team}\n\
             Authority=Developer ID Application: Example Corp ({team})\n"
        );
        let requirement = format!(
            "designated => identifier \"{bundle_id}\" and anchor apple generic and \
             certificate leaf[subject.OU] = {team}\n"
        );
        let gatekeeper = "accepted\nsource=Notarized Developer ID\n";
        parse_observed_identity(
            captured_fixture(&display, &requirement, gatekeeper),
            true,
            true,
            true,
        )
    }

    /// Materialize a helper bundle whose bytes match `entitlements`.
    pub fn write_helper_bundle(root: &Path, entitlements: &[u8]) -> std::path::PathBuf {
        let helper = root.join("GrokPtah Computer Use Helper.app");
        fs::create_dir_all(helper.join("Contents/MacOS")).unwrap();
        fs::write(
            helper.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{HELPER_BUNDLE_ID}</string></dict></plist>"
            ),
        )
        .unwrap();
        fs::write(
            helper.join("Contents/MacOS").join(HELPER_EXECUTABLE),
            b"helper-executable-bytes",
        )
        .unwrap();
        fs::write(helper.join("Contents/entitlements.plist"), entitlements).unwrap();
        helper
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::trust_root::{GuestImageTrustAnchor, HelperTrustAnchor, PackagedTrustRoot};
    use tempfile::tempdir;

    const ENTITLEMENTS: &[u8] = b"<?xml version=\"1.0\"?><plist><dict></dict></plist>";

    fn trust_root(team: &str, entitlements_digest: &str) -> PackagedTrustRoot {
        PackagedTrustRoot {
            schema: crate::trust_root::TRUST_ROOT_SCHEMA.into(),
            issuer: "unit-test".into(),
            app: crate::trust_root::AppTrustAnchor {
                bundle_id: APP_BUNDLE_ID.into(),
                team_id: team.into(),
                designated_requirement: format!(
                    "identifier \"{APP_BUNDLE_ID}\" and anchor apple generic and certificate leaf[subject.OU] = {team}"
                ),
            },
            helper: HelperTrustAnchor {
                bundle_id: HELPER_BUNDLE_ID.into(),
                team_id: team.into(),
                designated_requirement: format!(
                    "identifier \"{HELPER_BUNDLE_ID}\" and anchor apple generic and certificate leaf[subject.OU] = {team}"
                ),
                entitlements_sha256: entitlements_digest.into(),
            },
            guest_image: GuestImageTrustAnchor {
                digest_sha256: sha256_hex(b"guest-bytes"),
                format: "raw".into(),
                provenance: "unit-test-image".into(),
                authorization_sha256: sha256_hex(b"authorization"),
            },
        }
    }

    fn observe(dir: &Path, identity: ObservedCodeIdentity) -> PackagedHelperObservation {
        let helper = write_helper_bundle(dir, ENTITLEMENTS);
        let probe = FixtureProbe {
            identity,
            available: true,
        };
        inspect_helper_bundle(&helper, &probe).unwrap()
    }

    #[test]
    fn admits_only_when_the_os_verdict_matches_the_trust_root() {
        let dir = tempdir().unwrap();
        let observation = observe(
            dir.path(),
            notarized_identity("TEAMID1234", HELPER_BUNDLE_ID),
        );
        let root = trust_root("TEAMID1234", &observation.entitlements_digest);
        let admitted = admit_packaged_helper(&observation, &root).unwrap();
        assert_eq!(admitted.team_id, "TEAMID1234");
        assert_eq!(admitted.signing_class, SigningClass::NotarizedDeveloperId);
    }

    #[test]
    fn a_planted_codesign_text_file_cannot_change_the_verdict() {
        let dir = tempdir().unwrap();
        let helper = write_helper_bundle(dir.path(), ENTITLEMENTS);
        fs::write(
            helper.join("codesign-display.txt"),
            format!(
                "Identifier={HELPER_BUNDLE_ID}\nTeamIdentifier=TEAMID1234\n\
                 Authority=Developer ID Application: Example ()\nsource=Notarized Developer ID\naccepted\n"
            ),
        )
        .unwrap();
        // The probe reports the truth: this bundle is unsigned.
        let unsigned = crate::code_identity::parse_observed_identity(
            crate::code_identity::captured_fixture(
                "/x: code object is not signed at all\n",
                "",
                "",
            ),
            false,
            false,
            false,
        );
        let probe = FixtureProbe {
            identity: unsigned,
            available: true,
        };
        let observation = inspect_helper_bundle(&helper, &probe).unwrap();
        assert!(observation
            .ignored_self_attestations
            .contains(&"codesign-display.txt".to_string()));
        let root = trust_root("TEAMID1234", &observation.entitlements_digest);
        let error = admit_packaged_helper(&observation, &root).unwrap_err();
        assert_eq!(error.code, crate::error::IsolatedErrorCode::Unauthorized);
    }

    #[test]
    fn a_synthesized_requirement_for_the_observed_team_is_refused() {
        let dir = tempdir().unwrap();
        // The bundle is signed by a different team than the operator expects.
        let observation = observe(
            dir.path(),
            notarized_identity("ATTACKER99", HELPER_BUNDLE_ID),
        );
        let root = trust_root("TEAMID1234", &observation.entitlements_digest);
        let error = admit_packaged_helper(&observation, &root).unwrap_err();
        assert!(error.message.contains("Team ID"), "{error}");
    }

    #[test]
    fn a_requirement_that_merely_mentions_the_identifier_is_refused() {
        let dir = tempdir().unwrap();
        let mut identity = notarized_identity("TEAMID1234", HELPER_BUNDLE_ID);
        identity.designated_requirement = Some(format!(
            "identifier \"{HELPER_BUNDLE_ID}\" or anchor trusted"
        ));
        let observation = observe(dir.path(), identity);
        let root = trust_root("TEAMID1234", &observation.entitlements_digest);
        let error = admit_packaged_helper(&observation, &root).unwrap_err();
        assert!(error.message.contains("designated requirement"), "{error}");
    }

    #[test]
    fn a_missing_os_requirement_is_never_synthesized() {
        let dir = tempdir().unwrap();
        let mut identity = notarized_identity("TEAMID1234", HELPER_BUNDLE_ID);
        identity.designated_requirement = None;
        let observation = observe(dir.path(), identity);
        let root = trust_root("TEAMID1234", &observation.entitlements_digest);
        let error = admit_packaged_helper(&observation, &root).unwrap_err();
        assert!(error.message.contains("never synthesized"), "{error}");
    }

    #[test]
    fn symlinked_entitlements_fail_closed_instead_of_defaulting() {
        let dir = tempdir().unwrap();
        let helper = write_helper_bundle(dir.path(), ENTITLEMENTS);
        let entitlements = helper.join("Contents/entitlements.plist");
        fs::remove_file(&entitlements).unwrap();
        #[cfg(unix)]
        {
            let outside = dir.path().join("outside.plist");
            fs::write(&outside, ENTITLEMENTS).unwrap();
            std::os::unix::fs::symlink(&outside, &entitlements).unwrap();
            let probe = FixtureProbe {
                identity: notarized_identity("TEAMID1234", HELPER_BUNDLE_ID),
                available: true,
            };
            let error = inspect_helper_bundle(&helper, &probe).unwrap_err();
            assert_eq!(error.code, crate::error::IsolatedErrorCode::Unauthorized);
        }
    }

    #[test]
    fn missing_entitlements_fail_closed_instead_of_defaulting() {
        let dir = tempdir().unwrap();
        let helper = write_helper_bundle(dir.path(), ENTITLEMENTS);
        fs::remove_file(helper.join("Contents/entitlements.plist")).unwrap();
        let probe = FixtureProbe {
            identity: notarized_identity("TEAMID1234", HELPER_BUNDLE_ID),
            available: true,
        };
        assert!(inspect_helper_bundle(&helper, &probe).is_err());
    }

    #[test]
    fn an_unavailable_probe_cannot_admit() {
        let dir = tempdir().unwrap();
        let helper = write_helper_bundle(dir.path(), ENTITLEMENTS);
        let probe = FixtureProbe {
            identity: notarized_identity("TEAMID1234", HELPER_BUNDLE_ID),
            available: false,
        };
        let error = inspect_helper_bundle(&helper, &probe).unwrap_err();
        assert_eq!(
            error.code,
            crate::error::IsolatedErrorCode::UnsupportedPlatform
        );
    }

    #[test]
    fn guest_image_is_admitted_only_against_the_trust_root() {
        let dir = tempdir().unwrap();
        let image = dir.path().join("guest.img");
        fs::write(&image, b"guest-bytes").unwrap();
        let observation = inspect_guest_image(&image).unwrap();
        let root = trust_root("TEAMID1234", &sha256_hex(ENTITLEMENTS));
        admit_guest_image(&observation, &root).unwrap();

        // A manifest sitting beside the image supplies nothing.
        fs::write(
            dir.path().join("guest.img.manifest.json"),
            br#"{"digest":"00","provenance":"forged"}"#,
        )
        .unwrap();
        fs::write(&image, b"tampered-guest-bytes").unwrap();
        let tampered = inspect_guest_image(&image).unwrap();
        let error = admit_guest_image(&tampered, &root).unwrap_err();
        assert!(error.message.contains("digest"), "{error}");
    }

    #[test]
    fn bundle_manifest_rejects_symlinks_and_hashes_only_files() {
        let dir = tempdir().unwrap();
        let helper = write_helper_bundle(dir.path(), ENTITLEMENTS);
        hash_bundle_manifest(&helper).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Contents/Info.plist", helper.join("link")).unwrap();
            assert!(hash_bundle_manifest(&helper).is_err());
        }
        assert!(hash_file(&helper).is_err());
    }

    #[test]
    fn malformed_semver_is_rejected() {
        assert!(parse_semver("0.1.bad.extra").is_err());
        assert!(parse_semver("0.1").is_err());
        assert!(parse_semver("0.1.0.1").is_err());
        parse_semver("0.1.0").unwrap();
        versions_compatible("0.1.0", "0.1.9").unwrap();
        assert!(versions_compatible("0.1.0", "0.2.0").is_err());
        assert!(versions_compatible("0.1.0", "1.1.0").is_err());
    }

    #[test]
    fn documented_identity_json_matches_constants() {
        assert!(DOCUMENTED_IDENTITY_JSON.contains(HELPER_BUNDLE_ID));
        assert!(DOCUMENTED_IDENTITY_JSON.contains(APP_BUNDLE_ID));
    }
}
