//! Operator / release trust root for packaged Computer Use admission.
//!
//! # Why this module exists
//!
//! Admission compares an *observation* of an artifact against an *expectation*.
//! The expectation must originate somewhere the artifact under inspection
//! cannot reach. Deriving the expected designated requirement, Team ID, bundle
//! identifier, guest-image digest, authorization digest, or provenance from a
//! file that ships beside (or inside) the artifact makes the comparison a
//! tautology: whoever can write the artifact also writes what it is compared
//! against, and admission always succeeds.
//!
//! So the trust root is supplied out of band by an operator or a release
//! process, is loaded from a path that must lie **outside** the artifact root,
//! and every field is required. There is no defaulting, no inference from the
//! observation, and no partially populated trust root. Absent or unreadable
//! means fail closed, never "admit with whatever was found".

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{validate_digest, validate_id};

pub const TRUST_ROOT_SCHEMA: &str = "grokptah-computer-use-trust-root.v1";
pub const TRUST_ROOT_ENV: &str = "GROKPTAH_COMPUTER_USE_TRUST_ROOT";
const MAX_TRUST_ROOT_BYTES: u64 = 64 * 1024;
const MAX_REQUIREMENT_BYTES: usize = 512;

/// Team IDs are Apple 10-character alphanumeric identifiers.
const TEAM_ID_LEN: usize = 10;

/// The expected packaged-helper code identity, as an operator declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperTrustAnchor {
    /// Expected `Identifier=` of the helper bundle.
    pub bundle_id: String,
    /// Expected `TeamIdentifier=` of the helper bundle.
    pub team_id: String,
    /// Expected designated requirement, verbatim, as emitted by
    /// `codesign -d -r- <bundle>`. Compared for exact equality; it is never
    /// synthesized from an observed Team ID.
    pub designated_requirement: String,
    /// Expected SHA-256 of the helper's entitlements plist.
    pub entitlements_sha256: String,
}

/// The expected guest image, as an operator declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestImageTrustAnchor {
    pub digest_sha256: String,
    pub format: String,
    pub provenance: String,
    /// Digest of the release-side authorization for this image. Read from the
    /// trust root only; a manifest shipped next to the image cannot supply it.
    pub authorization_sha256: String,
}

/// The expected application bundle identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppTrustAnchor {
    pub bundle_id: String,
    pub team_id: String,
    pub designated_requirement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackagedTrustRoot {
    pub schema: String,
    /// Free-form operator label (release channel, build id). Advisory only;
    /// nothing is admitted on the strength of this string.
    pub issuer: String,
    pub app: AppTrustAnchor,
    pub helper: HelperTrustAnchor,
    pub guest_image: GuestImageTrustAnchor,
}

impl PackagedTrustRoot {
    /// Load the trust root named by [`TRUST_ROOT_ENV`].
    ///
    /// `artifact_root`, when supplied, is the tree that will be *inspected*.
    /// A trust root located inside it is refused: it would be under the
    /// control of whoever produced the artifact.
    pub fn from_env(artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        let raw = std::env::var_os(TRUST_ROOT_ENV).ok_or_else(|| {
            IsolatedError::unauthorized(format!(
                "{TRUST_ROOT_ENV} is not set; packaged admission has no operator trust root"
            ))
        })?;
        Self::load(Path::new(&raw), artifact_root)
    }

    pub fn load(path: &Path, artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            IsolatedError::unauthorized(format!("trust root is unreadable ({error})"))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(IsolatedError::unauthorized(
                "trust root must not be a symlink",
            ));
        }
        if !metadata.file_type().is_file() {
            return Err(IsolatedError::unauthorized(
                "trust root must be a regular file",
            ));
        }
        if metadata.len() > MAX_TRUST_ROOT_BYTES {
            return Err(IsolatedError::limit("trust root exceeds size bound"));
        }
        let canonical = dunce::canonicalize(path).map_err(|error| {
            IsolatedError::unauthorized(format!("trust root cannot be canonicalized ({error})"))
        })?;
        if let Some(root) = artifact_root {
            // Only enforce containment when the artifact root actually
            // resolves; a nonexistent root cannot contain anything.
            if let Ok(artifact) = dunce::canonicalize(root) {
                if canonical.starts_with(&artifact) {
                    return Err(IsolatedError::unauthorized(
                        "trust root lives inside the artifact root it would authorize",
                    ));
                }
            }
        }
        let bytes = fs::read(&canonical).map_err(|error| {
            IsolatedError::unauthorized(format!("trust root cannot be read ({error})"))
        })?;
        let parsed: Self = serde_json::from_slice(&bytes).map_err(|error| {
            IsolatedError::invalid(format!("trust root is not valid ({error})"))
        })?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn path_from_env() -> Option<PathBuf> {
        std::env::var_os(TRUST_ROOT_ENV).map(PathBuf::from)
    }

    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema != TRUST_ROOT_SCHEMA {
            return Err(IsolatedError::invalid("trust root schema is unsupported"));
        }
        if self.issuer.trim().is_empty() || self.issuer.len() > 256 {
            return Err(IsolatedError::invalid("trust root issuer is missing"));
        }
        validate_id("app bundle_id", &self.app.bundle_id)?;
        validate_id("helper bundle_id", &self.helper.bundle_id)?;
        validate_team_id("app team_id", &self.app.team_id)?;
        validate_team_id("helper team_id", &self.helper.team_id)?;
        validate_requirement(
            "app designated_requirement",
            &self.app.designated_requirement,
            &self.app.bundle_id,
        )?;
        validate_requirement(
            "helper designated_requirement",
            &self.helper.designated_requirement,
            &self.helper.bundle_id,
        )?;
        validate_digest(
            "helper entitlements_sha256",
            &self.helper.entitlements_sha256,
        )?;
        validate_digest("guest image digest_sha256", &self.guest_image.digest_sha256)?;
        validate_digest(
            "guest image authorization_sha256",
            &self.guest_image.authorization_sha256,
        )?;
        if !matches!(
            self.guest_image.format.as_str(),
            "raw" | "rawdisk" | "apple-diskimage"
        ) {
            return Err(IsolatedError::invalid(
                "trust root guest-image format is not an admitted format",
            ));
        }
        validate_provenance(&self.guest_image.provenance)?;
        Ok(())
    }
}

/// A designated requirement must at minimum bind the identifier it claims to
/// authorize, so an operator cannot paste in a requirement for another bundle.
fn validate_requirement(name: &str, requirement: &str, bundle_id: &str) -> IsolatedResult<()> {
    let trimmed = requirement.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_REQUIREMENT_BYTES
        || trimmed.contains('\0')
        || !trimmed.is_ascii()
    {
        return Err(IsolatedError::invalid(format!(
            "{name} is missing or invalid"
        )));
    }
    if !trimmed.contains(&format!("identifier \"{bundle_id}\"")) {
        return Err(IsolatedError::invalid(format!(
            "{name} does not bind the declared bundle identifier"
        )));
    }
    if !trimmed.contains("certificate leaf") && !trimmed.contains("anchor apple generic") {
        return Err(IsolatedError::invalid(format!(
            "{name} does not pin a certificate anchor"
        )));
    }
    Ok(())
}

pub fn validate_team_id(name: &str, team_id: &str) -> IsolatedResult<()> {
    if team_id.len() != TEAM_ID_LEN || !team_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(IsolatedError::invalid(format!(
            "{name} must be a 10-character alphanumeric Apple Team ID"
        )));
    }
    Ok(())
}

pub fn validate_provenance(provenance: &str) -> IsolatedResult<()> {
    if provenance.trim().is_empty()
        || provenance.len() > 256
        || provenance.contains('\0')
        || provenance.contains("..")
        || provenance.contains('/')
        || provenance.contains('\\')
        || !provenance.is_ascii()
    {
        return Err(IsolatedError::invalid(
            "provenance is not a bound identity token",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    pub(crate) fn fixture_json(team: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": TRUST_ROOT_SCHEMA,
            "issuer": "release-fixture",
            "app": {
                "bundleId": "com.chriscase.grokptah",
                "teamId": team,
                "designatedRequirement":
                    format!("identifier \"com.chriscase.grokptah\" and anchor apple generic and certificate leaf[subject.OU] = {team}"),
            },
            "helper": {
                "bundleId": "com.chriscase.grokptah.computer-use-helper",
                "teamId": team,
                "designatedRequirement":
                    format!("identifier \"com.chriscase.grokptah.computer-use-helper\" and anchor apple generic and certificate leaf[subject.OU] = {team}"),
                "entitlementsSha256": "b".repeat(64),
            },
            "guestImage": {
                "digestSha256": "c".repeat(64),
                "format": "raw",
                "provenance": "release-fixture-image",
                "authorizationSha256": "d".repeat(64),
            },
        })
    }

    fn write(dir: &Path, value: &serde_json::Value) -> PathBuf {
        let path = dir.join("trust-root.json");
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }

    #[test]
    fn well_formed_trust_root_loads() {
        let dir = tempdir().unwrap();
        let path = write(dir.path(), &fixture_json("TEAMID1234"));
        let root = PackagedTrustRoot::load(&path, None).unwrap();
        assert_eq!(root.helper.team_id, "TEAMID1234");
    }

    #[test]
    fn trust_root_inside_the_artifact_root_is_refused() {
        let dir = tempdir().unwrap();
        let artifacts = dir.path().join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        let path = write(&artifacts, &fixture_json("TEAMID1234"));
        let error = PackagedTrustRoot::load(&path, Some(&artifacts)).unwrap_err();
        assert!(
            error.message.contains("inside the artifact root"),
            "{error}"
        );
    }

    #[test]
    fn requirement_for_another_bundle_is_refused() {
        let dir = tempdir().unwrap();
        let mut json = fixture_json("TEAMID1234");
        json["helper"]["designatedRequirement"] =
            serde_json::json!("identifier \"com.example.other\" and anchor apple generic");
        let path = write(dir.path(), &json);
        assert!(PackagedTrustRoot::load(&path, None).is_err());
    }

    #[test]
    fn requirement_without_a_certificate_anchor_is_refused() {
        let dir = tempdir().unwrap();
        let mut json = fixture_json("TEAMID1234");
        json["helper"]["designatedRequirement"] =
            serde_json::json!("identifier \"com.chriscase.grokptah.computer-use-helper\"");
        let path = write(dir.path(), &json);
        assert!(PackagedTrustRoot::load(&path, None).is_err());
    }

    #[test]
    fn malformed_team_id_and_partial_roots_are_refused() {
        let dir = tempdir().unwrap();
        let mut json = fixture_json("SHORT");
        let path = write(dir.path(), &json);
        assert!(PackagedTrustRoot::load(&path, None).is_err());

        json = fixture_json("TEAMID1234");
        json["guestImage"]
            .as_object_mut()
            .unwrap()
            .remove("authorizationSha256");
        let path = write(dir.path(), &json);
        assert!(PackagedTrustRoot::load(&path, None).is_err());

        json = fixture_json("TEAMID1234");
        json["unexpected"] = serde_json::json!(true);
        let path = write(dir.path(), &json);
        assert!(PackagedTrustRoot::load(&path, None).is_err());
    }

    #[test]
    fn symlinked_trust_root_is_refused() {
        let dir = tempdir().unwrap();
        let real = write(dir.path(), &fixture_json("TEAMID1234"));
        #[cfg(unix)]
        {
            let link = dir.path().join("link.json");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(PackagedTrustRoot::load(&link, None).is_err());
        }
        assert!(PackagedTrustRoot::load(&real, None).is_ok());
    }
}
