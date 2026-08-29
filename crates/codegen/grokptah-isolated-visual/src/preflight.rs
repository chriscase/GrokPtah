use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{validate_id, SCHEMA_VERSION};
use crate::lifecycle::IsolatedEvidenceClass;
use crate::occupancy::{OccupancyState, OccupancyStore};
use crate::packaged_authority::{
    admit_guest_image, admit_packaged_helper, inspect_artifact_root, ExpectedGuestImage,
    ExpectedHelper, GuestImageObservation, PackagedHelperObservation,
};

pub const MIN_FREE_BYTES_FOR_GUEST_IMAGE: u64 = 25 * 1024 * 1024 * 1024;

/// Authoritative Virtualization.framework launch/boot receipt.
/// Not deserializable. Fields are private. Only
/// [`VirtualizationLaunchAdapter::observe`] may construct this type after
/// verifying hypervisor instance, guest, and admitted helper/image identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualizationLaunchReceipt {
    schema_version: u32,
    launch_id: String,
    guest_id: String,
    hypervisor_instance_id: String,
    boot_observed: bool,
    observed_at: DateTime<Utc>,
    helper_executable_digest: String,
    image_digest: String,
}

/// Serialization-only view of a launch receipt. Cannot be deserialized into
/// [`VirtualizationLaunchReceipt`] or fed into admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualizationLaunchProjection {
    pub schema_version: u32,
    pub launch_id: String,
    pub guest_id: String,
    pub hypervisor_instance_id: String,
    pub boot_observed: bool,
    pub observed_at: DateTime<Utc>,
}

impl VirtualizationLaunchReceipt {
    fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::unauthorized(
                "virtualization launch receipt schema is unsupported",
            ));
        }
        validate_id("launch_id", &self.launch_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_hypervisor_instance(&self.hypervisor_instance_id)?;
        crate::ids::validate_digest("helper executable digest", &self.helper_executable_digest)?;
        crate::ids::validate_digest("guest image digest", &self.image_digest)?;
        Ok(())
    }

    pub fn projection(&self) -> VirtualizationLaunchProjection {
        VirtualizationLaunchProjection {
            schema_version: self.schema_version,
            launch_id: self.launch_id.clone(),
            guest_id: self.guest_id.clone(),
            hypervisor_instance_id: self.hypervisor_instance_id.clone(),
            boot_observed: self.boot_observed,
            observed_at: self.observed_at,
        }
    }
}

fn validate_hypervisor_instance(instance: &str) -> IsolatedResult<()> {
    validate_id("hypervisor_instance_id", instance)?;
    let lower = instance.to_ascii_lowercase();
    if lower.contains("simulat")
        || lower.contains("fake")
        || lower.contains("boolean")
        || lower == "true"
        || lower == "observed"
        || lower.contains("adhoc")
    {
        return Err(IsolatedError::unauthorized(
            "virtualization launch receipt is not an authoritative hypervisor instance",
        ));
    }
    Ok(())
}

/// Trusted Virtualization.framework adapter. Simulator tokens cannot mint a
/// launch receipt. This is not a public DTO constructor.
pub struct VirtualizationLaunchAdapter;

impl VirtualizationLaunchAdapter {
    /// Observe a hypervisor launch/boot bound to admitted helper and image
    /// identities. `boot_observed` is recorded only by this adapter.
    pub fn observe(
        preflight: &IsolatedPreflight,
        guest_id: &str,
        hypervisor_instance_id: &str,
        boot_observed: bool,
    ) -> IsolatedResult<VirtualizationLaunchReceipt> {
        if !preflight.launch_intent_admitted
            || !preflight.helper_admitted
            || !preflight.image_admitted
        {
            return Err(IsolatedError::unauthorized(
                "Virtualization.framework observation requires admitted launch intent, helper, and image",
            ));
        }
        let helper = preflight.helper_identity.as_ref().ok_or_else(|| {
            IsolatedError::unauthorized("admitted helper identity is missing from preflight")
        })?;
        let image = preflight.image_identity.as_ref().ok_or_else(|| {
            IsolatedError::unauthorized("admitted guest-image identity is missing from preflight")
        })?;
        validate_id("guest_id", guest_id)?;
        validate_hypervisor_instance(hypervisor_instance_id)?;
        let receipt = VirtualizationLaunchReceipt {
            schema_version: SCHEMA_VERSION,
            launch_id: format!("launch-{}", uuid::Uuid::new_v4()),
            guest_id: guest_id.to_string(),
            hypervisor_instance_id: hypervisor_instance_id.to_string(),
            boot_observed,
            observed_at: Utc::now(),
            helper_executable_digest: helper.executable_digest.clone(),
            image_digest: image.digest.clone(),
        };
        receipt.validate()?;
        Ok(receipt)
    }
}

/// Authoritative preflight state. Serialize is intentionally omitted so a JSON
/// snapshot cannot be deserialized back into admission. Use
/// [`IsolatedPreflightProjection`] for read-only export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedPreflight {
    pub(crate) hardware_supported: bool,
    pub(crate) virtualization_framework_present: bool,
    pub(crate) helper_admitted: bool,
    pub(crate) image_admitted: bool,
    pub(crate) free_bytes: u64,
    pub(crate) occupancy_clear: bool,
    pub(crate) occupancy_state: OccupancyState,
    pub(crate) environmental_eligible: bool,
    pub(crate) launch_intent_admitted: bool,
    pub(crate) launch_observed: bool,
    pub(crate) boot_observed: bool,
    pub(crate) allowed_to_launch: bool,
    pub(crate) deny_reason: Option<String>,
    pub(crate) evidence_class: IsolatedEvidenceClass,
    pub(crate) helper_identity: Option<PackagedHelperObservation>,
    pub(crate) image_identity: Option<GuestImageObservation>,
}

/// Serialization-only preflight view. Not an admission input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedPreflightProjection {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_identity: Option<PackagedHelperObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_identity: Option<GuestImageObservation>,
}

impl IsolatedPreflight {
    /// Explicit fail-closed status. Cannot mint Virtualization.framework evidence.
    pub fn fail_closed(reason: impl Into<String>) -> Self {
        Self {
            hardware_supported: false,
            virtualization_framework_present: false,
            helper_admitted: false,
            image_admitted: false,
            free_bytes: 0,
            occupancy_clear: false,
            occupancy_state: OccupancyState::Recovery,
            environmental_eligible: false,
            launch_intent_admitted: false,
            launch_observed: false,
            boot_observed: false,
            allowed_to_launch: false,
            deny_reason: Some(reason.into()),
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            helper_identity: None,
            image_identity: None,
        }
    }

    pub fn projection(&self) -> IsolatedPreflightProjection {
        IsolatedPreflightProjection {
            hardware_supported: self.hardware_supported,
            virtualization_framework_present: self.virtualization_framework_present,
            helper_admitted: self.helper_admitted,
            image_admitted: self.image_admitted,
            free_bytes: self.free_bytes,
            occupancy_clear: self.occupancy_clear,
            occupancy_state: self.occupancy_state,
            environmental_eligible: self.environmental_eligible,
            launch_intent_admitted: self.launch_intent_admitted,
            launch_observed: self.launch_observed,
            boot_observed: self.boot_observed,
            allowed_to_launch: self.allowed_to_launch,
            deny_reason: self.deny_reason.clone(),
            evidence_class: self.evidence_class,
            helper_identity: self.helper_identity.clone(),
            image_identity: self.image_identity.clone(),
        }
    }

    pub fn allowed_to_launch(&self) -> bool {
        self.allowed_to_launch
    }

    pub fn helper_admitted(&self) -> bool {
        self.helper_admitted
    }

    pub fn image_admitted(&self) -> bool {
        self.image_admitted
    }

    pub fn evidence_class(&self) -> IsolatedEvidenceClass {
        self.evidence_class
    }

    pub fn deny_reason(&self) -> Option<&str> {
        self.deny_reason.as_deref()
    }

    /// Production admission: inspect env/default artifact root. Never a
    /// permanent `inspect(None)` that guarantees helper/image absence.
    pub fn inspect_production() -> IsolatedResult<Self> {
        let root = std::env::var_os("GROKPTAH_ISOLATED_VISUAL_ARTIFACT_ROOT")
            .map(std::path::PathBuf::from);
        Self::inspect(root.as_deref())
    }

    /// Inspect an artifact root against host-pinned canonical identity.
    /// Missing env pins cannot be replaced by artifact self-description.
    pub fn inspect(artifact_root: Option<&Path>) -> IsolatedResult<Self> {
        let expected_helper = ExpectedHelper::from_canonical_contract(None).ok();
        let expected_image = ExpectedGuestImage::from_canonical_contract().ok();
        Self::inspect_with_expected(
            artifact_root,
            expected_helper.as_ref(),
            expected_image.as_ref(),
        )
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
        let helper = self.helper_identity.as_ref().ok_or_else(|| {
            IsolatedError::unauthorized("admitted helper identity is missing from preflight")
        })?;
        let image = self.image_identity.as_ref().ok_or_else(|| {
            IsolatedError::unauthorized("admitted guest-image identity is missing from preflight")
        })?;
        if receipt.helper_executable_digest != helper.executable_digest
            || receipt.image_digest != image.digest
        {
            return Err(IsolatedError::unauthorized(
                "launch receipt is not bound to the admitted helper and image identities",
            ));
        }
        if !receipt.boot_observed {
            return Err(IsolatedError::unauthorized(
                "Virtualization.framework evidence requires an adapter-observed boot",
            ));
        }
        self.launch_observed = true;
        self.boot_observed = true;
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
        assert!(
            VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "vz-instance-1", true)
                .is_err()
        );
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
        assert!(VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "true", true).is_err());
        assert!(
            VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "vz-instance-1", true)
                .is_err()
        );
    }

    #[test]
    fn canonical_env_pins_admit_guest_image_without_self_description() {
        let dir = tempdir().unwrap();
        let observation = write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
        let preflight = IsolatedPreflight::inspect(Some(dir.path())).unwrap();
        assert!(!preflight.image_admitted);
        assert!(preflight
            .deny_reason
            .as_deref()
            .unwrap_or("")
            .contains("canonical guest-image identity is not pinned"));

        let digest_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_DIGEST_ENV;
        let provenance_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_PROVENANCE_ENV;
        let auth_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_AUTHORIZATION_ENV;
        let previous = [
            (
                digest_key,
                std::env::var(digest_key).ok(),
                observation.digest.clone(),
            ),
            (
                provenance_key,
                std::env::var(provenance_key).ok(),
                observation.provenance.clone(),
            ),
            (
                auth_key,
                std::env::var(auth_key).ok(),
                observation.authorization_digest.clone(),
            ),
        ];
        for (key, _, value) in &previous {
            std::env::set_var(key, value);
        }
        let pinned = IsolatedPreflight::inspect(Some(dir.path())).unwrap();
        for (key, previous, _) in previous {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        assert!(pinned.image_admitted);
        assert!(!pinned.helper_admitted);
        assert!(!pinned.allowed_to_launch);
        assert_eq!(
            pinned.evidence_class,
            IsolatedEvidenceClass::SimulatorIneligible
        );
    }

    fn admitted_intent_fixture() -> IsolatedPreflight {
        IsolatedPreflight {
            hardware_supported: true,
            virtualization_framework_present: true,
            helper_admitted: true,
            image_admitted: true,
            free_bytes: MIN_FREE_BYTES_FOR_GUEST_IMAGE,
            occupancy_clear: true,
            occupancy_state: OccupancyState::Clear,
            environmental_eligible: true,
            launch_intent_admitted: true,
            launch_observed: false,
            boot_observed: false,
            allowed_to_launch: true,
            deny_reason: None,
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            helper_identity: Some(PackagedHelperObservation {
                bundle_id: crate::packaged_authority::HELPER_BUNDLE_ID.into(),
                executable_digest: "a".repeat(64),
                team_id: "TEAMID1234".into(),
                designated_requirement: crate::packaged_authority::designated_requirement_for(
                    "TEAMID1234",
                ),
                signing_class: crate::packaged_authority::SigningClass::NotarizedDeveloperId,
                entitlements_digest:
                    crate::packaged_authority::canonical_helper_entitlements_digest(),
                notarization_source: Some("notarized_developer_id".into()),
                stapled: true,
                gatekeeper_accepted: true,
            }),
            image_identity: Some(GuestImageObservation {
                digest: "d".repeat(64),
                manifest_id: "guest-manifest-1".into(),
                provenance: "test-provenance".into(),
                format: "raw".into(),
                size_bytes: 16,
                authorization_digest: "e".repeat(64),
            }),
        }
    }

    #[test]
    fn fabricated_json_cannot_promote_virtualization_evidence() {
        let json = serde_json::json!({
            "hardwareSupported": true,
            "virtualizationFrameworkPresent": true,
            "helperAdmitted": true,
            "imageAdmitted": true,
            "freeBytes": MIN_FREE_BYTES_FOR_GUEST_IMAGE,
            "occupancyClear": true,
            "occupancyState": "clear",
            "environmentalEligible": true,
            "launchIntentAdmitted": true,
            "launchObserved": true,
            "bootObserved": true,
            "allowedToLaunch": true,
            "evidenceClass": "virtualization_framework",
        });
        let encoded = serde_json::to_string(&json).unwrap();
        let snapshot: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(snapshot["evidenceClass"], "virtualization_framework");
        let fail = IsolatedPreflight::fail_closed("fabricated");
        assert_ne!(
            fail.evidence_class,
            IsolatedEvidenceClass::VirtualizationFramework
        );
        assert!(!fail.virtualization_framework_launched_claim());
        assert!(
            VirtualizationLaunchAdapter::observe(&fail, "guest-1", "vz-instance-1", true).is_err()
        );
        let projected = serde_json::to_value(fail.projection()).unwrap();
        assert_eq!(projected["evidenceClass"], "simulator_ineligible");
        assert_eq!(projected["launchObserved"], false);
        assert_eq!(projected["bootObserved"], false);
    }

    #[test]
    fn adapter_rejects_simulator_hypervisor_tokens() {
        let preflight = admitted_intent_fixture();
        for instance in ["simulator-1", "fake-vz", "true", "observed", "boolean-boot"] {
            assert!(
                VirtualizationLaunchAdapter::observe(&preflight, "guest-1", instance, true)
                    .is_err(),
                "expected simulator token {instance} to fail"
            );
        }
        assert!(!preflight.virtualization_framework_launched_claim());
    }

    #[test]
    fn adapter_boot_false_cannot_promote_vf() {
        let preflight = admitted_intent_fixture();
        let receipt =
            VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "vz-instance-1", false)
                .unwrap();
        assert!(!receipt.boot_observed);
        assert_eq!(
            preflight
                .clone()
                .observe_virtualization_launch(&receipt)
                .unwrap_err()
                .code,
            crate::error::IsolatedErrorCode::Unauthorized
        );
    }

    #[test]
    fn trusted_adapter_observation_promotes_vf_in_simulator() {
        let preflight = admitted_intent_fixture();
        let receipt =
            VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "vz-instance-1", true)
                .unwrap();
        assert_eq!(receipt.helper_executable_digest, "a".repeat(64));
        assert_eq!(receipt.image_digest, "d".repeat(64));
        let launched = preflight.observe_virtualization_launch(&receipt).unwrap();
        assert_eq!(
            launched.evidence_class,
            IsolatedEvidenceClass::VirtualizationFramework
        );
        assert!(launched.virtualization_framework_launched_claim());
        let projected = launched.projection();
        assert_eq!(
            projected.evidence_class,
            IsolatedEvidenceClass::VirtualizationFramework
        );
        let encoded = serde_json::to_string(&projected).unwrap();
        assert!(encoded.contains("virtualization_framework"));
        assert!(launched
            .clone()
            .observe_virtualization_launch(&receipt)
            .is_ok());
    }

    #[test]
    fn receipt_projection_cannot_be_fed_back_as_receipt() {
        let preflight = admitted_intent_fixture();
        let receipt =
            VirtualizationLaunchAdapter::observe(&preflight, "guest-1", "vz-instance-1", true)
                .unwrap();
        let json = serde_json::to_string(&receipt.projection()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["bootObserved"], true);
        assert!(value.get("helperExecutableDigest").is_none());
    }
}
