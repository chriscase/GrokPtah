use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{validate_id, SCHEMA_VERSION};
use crate::lifecycle::IsolatedEvidenceClass;
use crate::occupancy::{OccupancyState, OccupancyStore};
use crate::packaged_authority::{
    admit_guest_image, admit_packaged_helper, inspect_artifact_root, ExpectedGuestImage,
    ExpectedHelper, GuestImageObservation, PackagedHelperObservation,
};

pub const MIN_FREE_BYTES_FOR_GUEST_IMAGE: u64 = 25 * 1024 * 1024 * 1024;

/// Authoritative Virtualization.framework launch/boot receipt. A boolean
/// `with_observed_launch(true)` cannot mint this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VirtualizationLaunchReceipt {
    pub schema_version: u32,
    pub launch_id: String,
    pub guest_id: String,
    pub hypervisor_instance_id: String,
    pub boot_observed: bool,
    pub observed_at: DateTime<Utc>,
}

impl VirtualizationLaunchReceipt {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::unauthorized(
                "virtualization launch receipt schema is unsupported",
            ));
        }
        validate_id("launch_id", &self.launch_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_id("hypervisor_instance_id", &self.hypervisor_instance_id)?;
        let instance = self.hypervisor_instance_id.to_ascii_lowercase();
        if instance.contains("simulat")
            || instance.contains("fake")
            || instance.contains("boolean")
            || instance == "true"
            || instance == "observed"
        {
            return Err(IsolatedError::unauthorized(
                "virtualization launch receipt is not an authoritative hypervisor instance",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedPreflight {
    pub hardware_supported: bool,
    pub virtualization_framework_present: bool,
    pub helper_admitted: bool,
    pub image_admitted: bool,
    pub free_bytes: u64,
    pub occupancy_clear: bool,
    pub occupancy_state: OccupancyState,
    pub environmental_eligible: bool,
    pub launch_intent_admitted: bool,
    pub launch_observed: bool,
    pub boot_observed: bool,
    pub allowed_to_launch: bool,
    pub deny_reason: Option<String>,
    pub evidence_class: IsolatedEvidenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_identity: Option<PackagedHelperObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_identity: Option<GuestImageObservation>,
}

impl IsolatedPreflight {
    /// Production admission: inspect env/default artifact root. Never a
    /// permanent `inspect(None)` that guarantees helper/image absence.
    pub fn inspect_production() -> IsolatedResult<Self> {
        let root = std::env::var_os("GROKPTAH_ISOLATED_VISUAL_ARTIFACT_ROOT")
            .map(std::path::PathBuf::from);
        let expected_helper = ExpectedHelper::from_canonical_contract(None).ok();
        let expected_image = ExpectedGuestImage::from_canonical_contract().ok();
        Self::inspect_with_expected(
            root.as_deref(),
            expected_helper.as_ref(),
            expected_image.as_ref(),
        )
    }

    pub fn inspect(artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        Self::inspect_with_expected(artifact_root, None, None)
    }

    pub fn inspect_with_expected(
        artifact_root: Option<&Path>,
        expected_helper: Option<&ExpectedHelper>,
        expected_image: Option<&ExpectedGuestImage>,
    ) -> IsolatedResult<Self> {
        let free_bytes = free_bytes().unwrap_or(0);
        let hardware_supported = hypervisor_supported();
        let virtualization_framework_present = virtualization_framework_present();
        let (occupancy_state, occupancy_error) = match occupancy_state(artifact_root) {
            Ok(state) => (state, None),
            Err(error) => (OccupancyState::Recovery, Some(error.to_string())),
        };
        let occupancy_clear = occupancy_error.is_none() && occupancy_state == OccupancyState::Clear;

        let mut helper_identity = None;
        let mut image_identity = None;
        let mut helper_admitted = false;
        let mut image_admitted = false;
        let mut artifact_errors = Vec::new();
        if let Some(root) = artifact_root {
            match inspect_artifact_root(root) {
                Ok((helper, image)) => {
                    if let Some(helper) = helper {
                        match expected_helper {
                            Some(expected) => {
                                match admit_packaged_helper(&helper, expected) {
                                    Ok(()) => {
                                        helper_admitted = true;
                                        helper_identity = Some(helper);
                                    }
                                    Err(error) => artifact_errors.push(error.to_string()),
                                }
                            }
                            None => artifact_errors.push(
                                "canonical helper identity is not pinned; artifact self-description cannot admit"
                                    .into(),
                            ),
                        }
                    }
                    if let Some(image) = image {
                        match expected_image {
                            Some(expected) => match admit_guest_image(&image, expected) {
                                Ok(()) => {
                                    image_admitted = true;
                                    image_identity = Some(image);
                                }
                                Err(error) => artifact_errors.push(error.to_string()),
                            },
                            None => artifact_errors.push(
                                "canonical guest-image identity is not pinned; sidecar manifest cannot admit"
                                    .into(),
                            ),
                        }
                    }
                }
                Err(error) => artifact_errors.push(error.to_string()),
            }
        }

        let mut env_deny = Vec::new();
        if free_bytes < MIN_FREE_BYTES_FOR_GUEST_IMAGE {
            env_deny.push(format!(
                "free disk {free_bytes} bytes is below the 25 GiB guest-image gate"
            ));
        }
        if !hardware_supported {
            env_deny.push("hypervisor / Virtualization.framework hardware is unsupported".into());
        }
        if !virtualization_framework_present {
            env_deny.push("Virtualization.framework is not present".into());
        }
        if !occupancy_clear {
            env_deny.push("a durable occupancy lease is not clear".into());
        }
        if let Some(error) = occupancy_error {
            env_deny.push(error);
        }
        let environmental_eligible = env_deny.is_empty();
        if !helper_admitted || !image_admitted {
            artifact_errors
                .push("cryptographically admitted helper/image identities are absent".into());
        }
        let launch_intent_admitted = environmental_eligible && helper_admitted && image_admitted;
        let mut deny = env_deny;
        deny.extend(artifact_errors);
        let allowed_to_launch = launch_intent_admitted;
        Ok(Self {
            hardware_supported,
            virtualization_framework_present,
            helper_admitted,
            image_admitted,
            free_bytes,
            occupancy_clear,
            occupancy_state,
            environmental_eligible,
            launch_intent_admitted,
            launch_observed: false,
            boot_observed: false,
            allowed_to_launch,
            deny_reason: if deny.is_empty() {
                None
            } else {
                Some(deny.join("; "))
            },
            // Eligibility without an observed launch cannot mint VF evidence.
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            helper_identity,
            image_identity,
        })
    }

    pub fn observe_virtualization_launch(
        mut self,
        receipt: &VirtualizationLaunchReceipt,
    ) -> IsolatedResult<Self> {
        receipt.validate()?;
        if !self.launch_intent_admitted {
            return Err(IsolatedError::unauthorized(
                "Virtualization.framework launch cannot be claimed without admitted launch intent",
            ));
        }
        if !self.helper_admitted || !self.image_admitted {
            return Err(IsolatedError::unauthorized(
                "Virtualization.framework launch cannot be claimed without admitted helper and image identities",
            ));
        }
        self.launch_observed = true;
        self.boot_observed = receipt.boot_observed;
        self.evidence_class = IsolatedEvidenceClass::VirtualizationFramework;
        Ok(self)
    }

    pub fn fail_closed_launch(&self) -> IsolatedResult<()> {
        if self.allowed_to_launch {
            Ok(())
        } else {
            Err(IsolatedError::unavailable(
                self.deny_reason
                    .clone()
                    .unwrap_or_else(|| "isolated visual launch is not eligible".into()),
            ))
        }
    }

    pub fn virtualization_framework_launched_claim(&self) -> bool {
        self.launch_observed
            && self.boot_observed
            && self.evidence_class == IsolatedEvidenceClass::VirtualizationFramework
    }
}

fn occupancy_state(artifact_root: Option<&Path>) -> IsolatedResult<OccupancyState> {
    let Some(root) = artifact_root else {
        return Ok(OccupancyState::Clear);
    };
    let occupancy_root = root.join("occupancy");
    if !occupancy_root.exists() {
        return Ok(OccupancyState::Clear);
    }
    let metadata = fs::symlink_metadata(&occupancy_root).map_err(|error| {
        IsolatedError::uncertain(format!("occupancy root cannot be inspected ({error})"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(IsolatedError::unauthorized(
            "occupancy root must not be a symlink",
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(IsolatedError::uncertain(
            "occupancy root exists but is not a directory",
        ));
    }
    let store = OccupancyStore::open(&occupancy_root)?;
    store.inspect_any()
}

fn free_bytes() -> IsolatedResult<u64> {
    let output = Command::new("/bin/df")
        .args(["-k", "/"])
        .output()
        .map_err(|error| IsolatedError::internal(error.to_string()))?;
    if !output.status.success() {
        return Err(IsolatedError::internal("df failed"));
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
    use crate::packaged_authority::{
        write_guest_image_claim, write_planted_codesign_display, write_unsigned_helper_bundle,
    };
    use tempfile::tempdir;

    #[test]
    fn missing_artifacts_fail_closed() {
        let preflight = IsolatedPreflight::inspect(None).unwrap();
        assert!(!preflight.allowed_to_launch);
        assert!(!preflight.launch_intent_admitted);
        assert!(preflight.fail_closed_launch().is_err());
        assert!(!preflight.virtualization_framework_launched_claim());
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
    }

    #[test]
    fn empty_marker_files_do_not_admit_or_claim_vf() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("helper.signed"), b"").unwrap();
        std::fs::write(dir.path().join("guest.img.signed"), b"").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(dir.path())).unwrap();
        assert!(!preflight.helper_admitted);
        assert!(!preflight.image_admitted);
        assert!(!preflight.launch_intent_admitted);
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
        let receipt = VirtualizationLaunchReceipt {
            schema_version: SCHEMA_VERSION,
            launch_id: "launch-1".into(),
            guest_id: "guest-1".into(),
            hypervisor_instance_id: "vz-instance-1".into(),
            boot_observed: true,
            observed_at: Utc::now(),
        };
        assert!(preflight.observe_virtualization_launch(&receipt).is_err());
    }

    #[test]
    fn planted_display_and_unsigned_bundle_do_not_admit() {
        let planted = tempdir().unwrap();
        write_planted_codesign_display(planted.path(), "TEAMID1234").unwrap();
        write_guest_image_claim(planted.path(), b"guest-bytes").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(planted.path())).unwrap();
        assert!(!preflight.helper_admitted);
        assert!(!preflight.allowed_to_launch);
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );

        let unsigned = tempdir().unwrap();
        write_unsigned_helper_bundle(unsigned.path()).unwrap();
        write_guest_image_claim(unsigned.path(), b"guest-bytes").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(unsigned.path())).unwrap();
        assert!(!preflight.helper_admitted);
        assert!(!preflight.launch_intent_admitted);
        assert!(preflight.fail_closed_launch().is_err());
    }

    #[test]
    fn occupancy_inspect_errors_are_not_clear() {
        let dir = tempdir().unwrap();
        let occupancy = dir.path().join("occupancy");
        std::fs::create_dir_all(&occupancy).unwrap();
        std::fs::write(occupancy.join("not-a-key.json"), b"{corrupt").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(dir.path())).unwrap();
        assert!(!preflight.occupancy_clear);
        assert_ne!(preflight.occupancy_state, OccupancyState::Clear);
        assert!(!preflight.allowed_to_launch);
    }

    #[test]
    fn eligibility_without_launch_cannot_claim_vf() {
        let dir = tempdir().unwrap();
        write_unsigned_helper_bundle(dir.path()).unwrap();
        write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(dir.path())).unwrap();
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
        assert!(!preflight.virtualization_framework_launched_claim());
        let receipt = VirtualizationLaunchReceipt {
            schema_version: SCHEMA_VERSION,
            launch_id: "launch-1".into(),
            guest_id: "guest-1".into(),
            hypervisor_instance_id: "true".into(),
            boot_observed: true,
            observed_at: Utc::now(),
        };
        assert!(receipt.validate().is_err());
        let receipt = VirtualizationLaunchReceipt {
            hypervisor_instance_id: "vz-instance-1".into(),
            ..receipt
        };
        assert!(preflight.observe_virtualization_launch(&receipt).is_err());
    }
}
