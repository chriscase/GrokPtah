//! Launch admission for the isolated visual backend.
//!
//! Preflight answers one question: may a real Virtualization.framework guest
//! be launched right now? Every input is independently derived, and every
//! unknown denies:
//!
//! * identity comes from an OS code-signing probe, never a file in the bundle;
//! * expectations come from an operator trust root outside the artifact;
//! * an occupancy store that cannot be read counts as occupied, not clear.
//!
//! Eligibility is *not* a launch receipt. `evidence_class` stays
//! [`IsolatedEvidenceClass::SimulatorIneligible`] until a launch is actually
//! observed via [`IsolatedPreflight::with_observed_launch`], so nothing in this
//! repository can claim Virtualization.framework evidence it did not see.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::code_identity::{CodeIdentityProbe, SystemCodeIdentityProbe};
use crate::error::{IsolatedError, IsolatedResult};
use crate::lifecycle::IsolatedEvidenceClass;
use crate::occupancy::{OccupancyState, OccupancyStore};
use crate::packaged_authority::{
    admit_guest_image, admit_packaged_helper, inspect_guest_image, inspect_helper_bundle,
    AdmittedGuestImage, AdmittedHelperIdentity, HELPER_PRODUCT_NAME,
};
use crate::trust_root::PackagedTrustRoot;

pub const MIN_FREE_BYTES_FOR_GUEST_IMAGE: u64 = 25 * 1024 * 1024 * 1024;
pub const ARTIFACT_ROOT_ENV: &str = "GROKPTAH_ISOLATED_VISUAL_ARTIFACT_ROOT";

/// Why launch is not admitted. Every variant is a denial, never a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DenyReason {
    pub category: String,
    pub detail: String,
}

impl DenyReason {
    fn new(category: &str, detail: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedPreflight {
    pub hardware_supported: bool,
    pub virtualization_framework_present: bool,
    /// True only when an OS probe could run at all on this host.
    pub code_identity_probe_available: bool,
    pub trust_root_present: bool,
    pub helper_admitted: bool,
    pub image_admitted: bool,
    pub free_bytes: u64,
    pub occupancy_state: OccupancyState,
    pub occupancy_clear: bool,
    pub environmental_eligible: bool,
    pub launch_intent_admitted: bool,
    pub launch_observed: bool,
    pub boot_observed: bool,
    pub allowed_to_launch: bool,
    pub deny_reasons: Vec<DenyReason>,
    pub evidence_class: IsolatedEvidenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_identity: Option<AdmittedHelperIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_identity: Option<AdmittedGuestImage>,
    /// Self-attestation files found beside the artifacts. Never consulted.
    #[serde(default)]
    pub ignored_self_attestations: Vec<String>,
}

impl IsolatedPreflight {
    /// Production admission: the artifact root from the environment, the OS
    /// code-signing probe, and the operator trust root.
    pub fn inspect_production() -> Self {
        let artifact_root = std::env::var_os(ARTIFACT_ROOT_ENV).map(PathBuf::from);
        let trust_root = PackagedTrustRoot::from_env(artifact_root.as_deref());
        Self::inspect(
            artifact_root.as_deref(),
            trust_root.as_ref().ok(),
            &SystemCodeIdentityProbe,
            trust_root.as_ref().err().map(|error| error.message.clone()),
        )
    }

    /// Inspect with explicit inputs. `trust_root` of `None` always denies.
    pub fn inspect(
        artifact_root: Option<&Path>,
        trust_root: Option<&PackagedTrustRoot>,
        probe: &dyn CodeIdentityProbe,
        trust_root_error: Option<String>,
    ) -> Self {
        let mut deny = Vec::new();
        let free_bytes = match free_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                deny.push(DenyReason::new("disk", error.message));
                0
            }
        };
        if free_bytes < MIN_FREE_BYTES_FOR_GUEST_IMAGE {
            deny.push(DenyReason::new(
                "disk",
                format!("free disk {free_bytes} bytes is below the 25 GiB guest-image gate"),
            ));
        }
        let hardware_supported = hypervisor_supported();
        if !hardware_supported {
            deny.push(DenyReason::new(
                "hardware",
                "hypervisor / Virtualization.framework hardware is unsupported",
            ));
        }
        let virtualization_framework_present = virtualization_framework_present();
        if !virtualization_framework_present {
            deny.push(DenyReason::new(
                "hardware",
                "Virtualization.framework is not present",
            ));
        }

        let (occupancy_state, occupancy_problems) = occupancy_state(artifact_root);
        let occupancy_clear = occupancy_state == OccupancyState::Clear;
        if !occupancy_clear {
            deny.push(DenyReason::new(
                "occupancy",
                format!(
                    "occupancy is {occupancy_state:?}{}",
                    if occupancy_problems.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", occupancy_problems.join("; "))
                    }
                ),
            ));
        }

        let code_identity_probe_available = probe.available();
        if !code_identity_probe_available {
            deny.push(DenyReason::new(
                "identity",
                "no OS code-signing probe is available on this host",
            ));
        }
        let trust_root_present = trust_root.is_some();
        if let Some(error) = trust_root_error {
            deny.push(DenyReason::new("trust-root", error));
        } else if !trust_root_present {
            deny.push(DenyReason::new(
                "trust-root",
                "no operator trust root was supplied; expectations cannot come from the artifact",
            ));
        }

        let mut helper_identity = None;
        let mut image_identity = None;
        let mut ignored_self_attestations = Vec::new();

        if let (Some(root), Some(trust)) = (artifact_root, trust_root) {
            ignored_self_attestations = crate::packaged_authority::find_self_attestations(root);
            let helper_root = root.join(format!("{HELPER_PRODUCT_NAME}.app"));
            if helper_root.is_dir() {
                match inspect_helper_bundle(&helper_root, probe) {
                    Ok(observation) => {
                        ignored_self_attestations
                            .extend(observation.ignored_self_attestations.iter().cloned());
                        match admit_packaged_helper(&observation, trust) {
                            Ok(admitted) => helper_identity = Some(admitted),
                            Err(error) => {
                                deny.push(DenyReason::new("helper", error.message));
                            }
                        }
                    }
                    Err(error) => deny.push(DenyReason::new("helper", error.message)),
                }
            } else {
                deny.push(DenyReason::new(
                    "helper",
                    "no packaged helper bundle is present in the artifact root",
                ));
            }

            let image_path = root.join("guest.img");
            if image_path.exists() {
                match inspect_guest_image(&image_path) {
                    Ok(observation) => match admit_guest_image(&observation, trust) {
                        Ok(admitted) => image_identity = Some(admitted),
                        Err(error) => deny.push(DenyReason::new("guest-image", error.message)),
                    },
                    Err(error) => deny.push(DenyReason::new("guest-image", error.message)),
                }
            } else {
                deny.push(DenyReason::new(
                    "guest-image",
                    "no guest image is present in the artifact root",
                ));
            }
        } else if artifact_root.is_none() {
            deny.push(DenyReason::new(
                "artifacts",
                "no artifact root is configured",
            ));
        }

        ignored_self_attestations.sort();
        ignored_self_attestations.dedup();

        let helper_admitted = helper_identity.is_some();
        let image_admitted = image_identity.is_some();
        let environmental_eligible = !deny
            .iter()
            .any(|reason| matches!(reason.category.as_str(), "disk" | "hardware" | "occupancy"));
        let launch_intent_admitted = deny.is_empty() && helper_admitted && image_admitted;

        Self {
            hardware_supported,
            virtualization_framework_present,
            code_identity_probe_available,
            trust_root_present,
            helper_admitted,
            image_admitted,
            free_bytes,
            occupancy_state,
            occupancy_clear,
            environmental_eligible,
            launch_intent_admitted,
            launch_observed: false,
            boot_observed: false,
            allowed_to_launch: launch_intent_admitted,
            deny_reasons: deny,
            // Admissibility is not observation. Only a real launch upgrades this.
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            helper_identity,
            image_identity,
            ignored_self_attestations,
        }
    }

    /// Record that a real Virtualization.framework launch was observed.
    /// Refused unless launch intent was admitted, so evidence cannot be minted.
    pub fn with_observed_launch(mut self, boot_ready: bool) -> IsolatedResult<Self> {
        if !self.launch_intent_admitted {
            return Err(IsolatedError::unauthorized(
                "Virtualization.framework launch cannot be claimed without admitted launch intent",
            ));
        }
        self.launch_observed = true;
        self.boot_observed = boot_ready;
        self.evidence_class = IsolatedEvidenceClass::VirtualizationFramework;
        Ok(self)
    }

    pub fn fail_closed_launch(&self) -> IsolatedResult<()> {
        if self.allowed_to_launch {
            return Ok(());
        }
        Err(IsolatedError::unavailable(self.deny_summary()))
    }

    pub fn deny_summary(&self) -> String {
        if self.deny_reasons.is_empty() {
            return "isolated visual launch is not eligible".into();
        }
        self.deny_reasons
            .iter()
            .map(|reason| format!("{}: {}", reason.category, reason.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn virtualization_framework_launched_claim(&self) -> bool {
        self.launch_observed
            && self.evidence_class == IsolatedEvidenceClass::VirtualizationFramework
    }

    /// A preflight that denies everything, for hosts where inspection itself
    /// could not run. Used instead of an `unwrap_or_default` that might read
    /// as "clear".
    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            hardware_supported: false,
            virtualization_framework_present: false,
            code_identity_probe_available: false,
            trust_root_present: false,
            helper_admitted: false,
            image_admitted: false,
            free_bytes: 0,
            occupancy_state: OccupancyState::Conflicting,
            occupancy_clear: false,
            environmental_eligible: false,
            launch_intent_admitted: false,
            launch_observed: false,
            boot_observed: false,
            allowed_to_launch: false,
            deny_reasons: vec![DenyReason::new("preflight", reason)],
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            helper_identity: None,
            image_identity: None,
            ignored_self_attestations: Vec::new(),
        }
    }
}

/// Occupancy state for the artifact root. An unreadable store is
/// [`OccupancyState::Conflicting`], never `Clear`.
fn occupancy_state(artifact_root: Option<&Path>) -> (OccupancyState, Vec<String>) {
    let Some(root) = artifact_root else {
        return (OccupancyState::Clear, Vec::new());
    };
    let occupancy_root = root.join("occupancy");
    if !occupancy_root.exists() {
        return (OccupancyState::Clear, Vec::new());
    }
    match OccupancyStore::open(&occupancy_root) {
        Ok(store) => store.sweep(),
        Err(error) => (
            OccupancyState::Conflicting,
            vec![format!(
                "occupancy store cannot be opened: {}",
                error.message
            )],
        ),
    }
}

fn free_bytes() -> IsolatedResult<u64> {
    let output = Command::new("/bin/df")
        .args(["-k", "/"])
        .output()
        .map_err(|error| IsolatedError::internal(format!("df failed ({error})")))?;
    if !output.status.success() {
        return Err(IsolatedError::internal("df exited nonzero"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .nth(1)
        .ok_or_else(|| IsolatedError::internal("df output is empty"))?;
    let avail_k = line
        .split_whitespace()
        .nth(3)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| IsolatedError::internal("df avail parse failed"))?;
    Ok(avail_k.saturating_mul(1024))
}

fn hypervisor_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/sbin/sysctl")
            .args(["-n", "kern.hv_support"])
            .output();
        matches!(output, Ok(output) if output.status.success() && output.stdout.starts_with(b"1"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn virtualization_framework_present() -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new("/System/Library/Frameworks/Virtualization.framework").is_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packaged_authority::fixtures::{
        notarized_identity, write_helper_bundle, FixtureProbe,
    };
    use crate::packaged_authority::{hash_file, HELPER_BUNDLE_ID};
    use crate::trust_root::{AppTrustAnchor, GuestImageTrustAnchor, HelperTrustAnchor};
    use tempfile::tempdir;

    const ENTITLEMENTS: &[u8] = b"<?xml version=\"1.0\"?><plist><dict></dict></plist>";

    fn probe(available: bool) -> FixtureProbe {
        FixtureProbe {
            identity: notarized_identity("TEAMID1234", HELPER_BUNDLE_ID),
            available,
        }
    }

    fn artifact_root(dir: &Path) -> (PathBuf, PackagedTrustRoot) {
        let root = dir.join("artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let helper = write_helper_bundle(&root, ENTITLEMENTS);
        std::fs::write(root.join("guest.img"), b"guest-bytes").unwrap();
        let trust = PackagedTrustRoot {
            schema: crate::trust_root::TRUST_ROOT_SCHEMA.into(),
            issuer: "preflight-test".into(),
            app: AppTrustAnchor {
                bundle_id: crate::packaged_authority::APP_BUNDLE_ID.into(),
                team_id: "TEAMID1234".into(),
                designated_requirement: format!(
                    "identifier \"{}\" and anchor apple generic",
                    crate::packaged_authority::APP_BUNDLE_ID
                ),
            },
            helper: HelperTrustAnchor {
                bundle_id: HELPER_BUNDLE_ID.into(),
                team_id: "TEAMID1234".into(),
                designated_requirement: format!(
                    "identifier \"{HELPER_BUNDLE_ID}\" and anchor apple generic and certificate leaf[subject.OU] = TEAMID1234"
                ),
                entitlements_sha256: hash_file(&helper.join("Contents/entitlements.plist")).unwrap(),
            },
            guest_image: GuestImageTrustAnchor {
                digest_sha256: hash_file(&root.join("guest.img")).unwrap(),
                format: "raw".into(),
                provenance: "preflight-test-image".into(),
                authorization_sha256: crate::ids::sha256_hex(b"authorization"),
            },
        };
        (root, trust)
    }

    #[test]
    fn missing_everything_fails_closed() {
        let preflight = IsolatedPreflight::inspect(None, None, &probe(true), None);
        assert!(!preflight.allowed_to_launch);
        assert!(!preflight.launch_intent_admitted);
        assert!(preflight.fail_closed_launch().is_err());
        assert!(!preflight.virtualization_framework_launched_claim());
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
        assert!(preflight
            .deny_reasons
            .iter()
            .any(|r| r.category == "trust-root"));
    }

    #[test]
    fn a_trust_root_alone_does_not_admit_without_an_os_probe() {
        let dir = tempdir().unwrap();
        let (root, trust) = artifact_root(dir.path());
        let preflight = IsolatedPreflight::inspect(Some(&root), Some(&trust), &probe(false), None);
        assert!(!preflight.code_identity_probe_available);
        assert!(!preflight.helper_admitted);
        assert!(!preflight.allowed_to_launch);
        assert!(preflight.with_observed_launch(true).is_err());
    }

    #[test]
    fn marker_files_are_reported_and_never_admit() {
        let dir = tempdir().unwrap();
        let (root, trust) = artifact_root(dir.path());
        std::fs::write(root.join("helper.signed"), b"").unwrap();
        std::fs::write(root.join("guest.img.signed"), b"").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(&root), Some(&trust), &probe(true), None);
        assert!(preflight
            .ignored_self_attestations
            .contains(&"helper.signed".to_string()));
        // Admission still turns entirely on the OS verdict + trust root.
        assert!(preflight.helper_admitted);
        assert!(preflight.image_admitted);
    }

    #[test]
    fn admitted_identity_still_does_not_claim_virtualization_framework() {
        let dir = tempdir().unwrap();
        let (root, trust) = artifact_root(dir.path());
        let preflight = IsolatedPreflight::inspect(Some(&root), Some(&trust), &probe(true), None);
        assert!(preflight.helper_admitted && preflight.image_admitted);
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
        assert!(!preflight.virtualization_framework_launched_claim());
        // On CI hosts the hardware gate denies, which is the honest outcome.
        if !preflight.launch_intent_admitted {
            assert!(preflight.with_observed_launch(true).is_err());
        }
    }

    #[test]
    fn a_forged_artifact_root_cannot_supply_its_own_expectations() {
        let dir = tempdir().unwrap();
        let (root, trust) = artifact_root(dir.path());
        // Attacker rewrites the image and drops in a matching manifest.
        std::fs::write(root.join("guest.img"), b"attacker-image").unwrap();
        std::fs::write(
            root.join("guest.img.manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "manifestId": "forged",
                "digest": crate::ids::sha256_hex(b"attacker-image"),
                "provenance": "forged",
                "format": "raw",
                "authorizationDigest": crate::ids::sha256_hex(b"forged"),
            }))
            .unwrap(),
        )
        .unwrap();
        let preflight = IsolatedPreflight::inspect(Some(&root), Some(&trust), &probe(true), None);
        assert!(!preflight.image_admitted);
        assert!(preflight
            .deny_reasons
            .iter()
            .any(|r| r.category == "guest-image"));
    }

    #[test]
    fn denied_constructor_never_reads_as_clear() {
        let denied = IsolatedPreflight::denied("inspection failed");
        assert!(!denied.allowed_to_launch);
        assert!(!denied.occupancy_clear);
        assert_eq!(denied.occupancy_state, OccupancyState::Conflicting);
    }
}
