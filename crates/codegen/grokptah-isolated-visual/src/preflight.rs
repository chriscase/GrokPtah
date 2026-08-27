use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::lifecycle::IsolatedEvidenceClass;

pub const MIN_FREE_BYTES_FOR_GUEST_IMAGE: u64 = 25 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedPreflight {
    pub hardware_supported: bool,
    pub virtualization_framework_present: bool,
    pub signed_helper_present: bool,
    pub signed_image_present: bool,
    pub free_bytes: u64,
    pub occupancy_clear: bool,
    pub allowed_to_launch: bool,
    pub deny_reason: Option<String>,
    pub evidence_class_if_launched: IsolatedEvidenceClass,
}

impl IsolatedPreflight {
    pub fn inspect(artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        let free_bytes = free_bytes().unwrap_or(0);
        let hardware_supported = hypervisor_supported();
        let virtualization_framework_present = virtualization_framework_present();
        let (signed_helper_present, signed_image_present) = match artifact_root {
            Some(root) => (
                root.join("helper.signed").is_file(),
                root.join("guest.img.signed").is_file(),
            ),
            None => (false, false),
        };
        let occupancy_clear = occupancy_clear();
        let mut deny = Vec::new();
        if free_bytes < MIN_FREE_BYTES_FOR_GUEST_IMAGE {
            deny.push(format!(
                "free disk {} bytes is below the 25 GiB guest-image gate",
                free_bytes
            ));
        }
        if !hardware_supported {
            deny.push("hypervisor / Virtualization.framework hardware is unsupported".into());
        }
        if !virtualization_framework_present {
            deny.push("Virtualization.framework is not present".into());
        }
        if !signed_helper_present || !signed_image_present {
            deny.push("signed helper/image artifacts are absent".into());
        }
        if !occupancy_clear {
            deny.push("a shared computer-use or VM target is occupied".into());
        }
        let allowed_to_launch = deny.is_empty();
        Ok(Self {
            hardware_supported,
            virtualization_framework_present,
            signed_helper_present,
            signed_image_present,
            free_bytes,
            occupancy_clear,
            allowed_to_launch,
            deny_reason: if deny.is_empty() {
                None
            } else {
                Some(deny.join("; "))
            },
            evidence_class_if_launched: if allowed_to_launch {
                IsolatedEvidenceClass::VirtualizationFramework
            } else {
                IsolatedEvidenceClass::SimulatorIneligible
            },
        })
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

fn occupancy_clear() -> bool {
    // Never kill or reuse an occupied shared target. Presence of a running
    // GrokPtah isolated-visual helper or vz virtual machine is occupancy.
    let occupied = Command::new("/bin/ps")
        .args(["-axo", "command="])
        .output()
        .ok()
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines().any(|line| {
                line.contains("grokptah-isolated-visual-helper")
                    || line.contains("com.grokptah.isolated-visual")
            })
        })
        .unwrap_or(true);
    !occupied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_artifacts_fail_closed() {
        let preflight = IsolatedPreflight::inspect(None).unwrap();
        assert!(!preflight.allowed_to_launch);
        assert!(preflight.fail_closed_launch().is_err());
        assert!(!preflight.virtualization_framework_launched_claim());
    }
}

impl IsolatedPreflight {
    pub fn virtualization_framework_launched_claim(&self) -> bool {
        false
    }
}
