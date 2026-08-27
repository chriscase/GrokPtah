use std::fs;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::lifecycle::IsolatedEvidenceClass;
use crate::occupancy::{OccupancyState, OccupancyStore};
use crate::packaged_authority::{
    admit_guest_image, admit_packaged_helper, inspect_artifact_root, ExpectedHelper,
    GuestImageObservation, PackagedHelperObservation,
};

pub const MIN_FREE_BYTES_FOR_GUEST_IMAGE: u64 = 25 * 1024 * 1024 * 1024;

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
        Self::inspect(root.as_deref())
    }

    pub fn inspect(artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        Self::inspect_with_team(artifact_root, None)
    }

    pub fn inspect_with_team(
        artifact_root: Option<&Path>,
        expected_team_id: Option<&str>,
    ) -> IsolatedResult<Self> {
        let free_bytes = free_bytes().unwrap_or(0);
        let hardware_supported = hypervisor_supported();
        let virtualization_framework_present = virtualization_framework_present();
        let occupancy_state = occupancy_state(artifact_root);
        let occupancy_clear = occupancy_state == OccupancyState::Clear;

        let mut helper_identity = None;
        let mut image_identity = None;
        let mut helper_admitted = false;
        let mut image_admitted = false;
        let mut artifact_errors = Vec::new();
        if let Some(root) = artifact_root {
            match inspect_artifact_root(root) {
                Ok((helper, image, expected_image)) => {
                    if let Some(helper) = helper {
                        if let Some(team) = expected_team_id
                            .or(Some(helper.team_id.as_str()))
                            .filter(|team| !team.is_empty())
                        {
                            match admit_packaged_helper(&helper, &ExpectedHelper::canonical(team)) {
                                Ok(()) => {
                                    helper_admitted = true;
                                    helper_identity = Some(helper);
                                }
                                Err(error) => artifact_errors.push(error.to_string()),
                            }
                        } else {
                            artifact_errors.push(
                                "helper Team ID is missing from cryptographic inspection".into(),
                            );
                        }
                    }
                    if let (Some(image), Some(expected)) = (image, expected_image) {
                        match admit_guest_image(&image, &expected) {
                            Ok(()) => {
                                image_admitted = true;
                                image_identity = Some(image);
                            }
                            Err(error) => artifact_errors.push(error.to_string()),
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
            && self.evidence_class == IsolatedEvidenceClass::VirtualizationFramework
    }
}

fn occupancy_state(artifact_root: Option<&Path>) -> OccupancyState {
    let Some(root) = artifact_root else {
        return OccupancyState::Clear;
    };
    let occupancy_root = root.join("occupancy");
    if !occupancy_root.exists() {
        return OccupancyState::Clear;
    }
    OccupancyStore::open(occupancy_root)
        .ok()
        .and_then(|store| {
            fs::read_dir(root.join("occupancy")).ok().map(|entries| {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if let Some(key) = name.strip_suffix(".json") {
                        if let Ok(state) = store.inspect(key) {
                            if state != OccupancyState::Clear {
                                return state;
                            }
                        }
                    }
                }
                OccupancyState::Clear
            })
        })
        .unwrap_or(OccupancyState::Clear)
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
    use crate::packaged_authority::write_admitted_fixture;
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
        assert!(preflight.with_observed_launch(true).is_err());
    }

    #[test]
    fn eligibility_without_launch_cannot_claim_vf() {
        let dir = tempdir().unwrap();
        write_admitted_fixture(dir.path(), "TEAMID1234", b"guest-bytes").unwrap();
        let preflight =
            IsolatedPreflight::inspect_with_team(Some(dir.path()), Some("TEAMID1234")).unwrap();
        assert!(preflight.helper_admitted);
        assert!(preflight.image_admitted);
        assert_eq!(
            preflight.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
        assert!(!preflight.virtualization_framework_launched_claim());
        if preflight.launch_intent_admitted {
            let launched = preflight.clone().with_observed_launch(true).unwrap();
            assert!(launched.virtualization_framework_launched_claim());
        }
    }
}
