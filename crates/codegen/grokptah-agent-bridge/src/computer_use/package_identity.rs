//! Packaged macOS Computer Use identity contract (#444).
//!
//! This is the single source of truth for app/helper bundle IDs, version
//! compatibility, signing class, and packaged-qualification eligibility.
//! Simulator, cargo-run, and ad-hoc identities cannot count as packaged proof.

use serde::{Deserialize, Serialize};

use super::types::{validate_id, ComputerError, ComputerErrorCode, ComputerResult};

pub const PACKAGE_IDENTITY_SCHEMA: &str = "grokptah-computer-use-package-identity.v1";
pub const PACKAGE_AUTHORITY_EVIDENCE_SCHEMA: &str = "grokptah-computer-use-package-authority.v1";

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

const DOCUMENTED_IDENTITY_JSON: &str =
    include_str!("../../../../../docs/schemas/grokptah-computer-use-package-identity.v1.json");

/// How the running code identity was signed. Ad-hoc is never packaged proof.
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
            || lower.contains("not signed")
            || lower.contains("unsigned")
        {
            return Self::Unsigned;
        }
        Self::Uninspected
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    #[default]
    InProcessHost,
    PackagedHelper,
}

/// Code identity that TCC Screen Recording / Accessibility attach to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerExecutorIdentity {
    pub kind: ExecutorKind,
    pub bundle_id: String,
    pub helper_bundle_id: String,
    pub version: String,
    pub helper_version: String,
    pub signing_class: SigningClass,
    pub tcc_principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub designated_requirement: Option<String>,
}

impl ComputerExecutorIdentity {
    pub fn in_process_host(signing_class: SigningClass) -> Self {
        Self {
            kind: ExecutorKind::InProcessHost,
            bundle_id: APP_BUNDLE_ID.to_string(),
            helper_bundle_id: HELPER_BUNDLE_ID.to_string(),
            version: APP_VERSION.to_string(),
            helper_version: HELPER_VERSION.to_string(),
            signing_class,
            tcc_principal: APP_BUNDLE_ID.to_string(),
            team_id: None,
            designated_requirement: None,
        }
    }

    pub fn packaged_helper(signing_class: SigningClass) -> Self {
        Self {
            kind: ExecutorKind::PackagedHelper,
            bundle_id: APP_BUNDLE_ID.to_string(),
            helper_bundle_id: HELPER_BUNDLE_ID.to_string(),
            version: APP_VERSION.to_string(),
            helper_version: HELPER_VERSION.to_string(),
            signing_class,
            tcc_principal: HELPER_BUNDLE_ID.to_string(),
            team_id: None,
            designated_requirement: None,
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("bundle_id", &self.bundle_id)?;
        validate_id("helper_bundle_id", &self.helper_bundle_id)?;
        validate_id("tcc_principal", &self.tcc_principal)?;
        if self.bundle_id != APP_BUNDLE_ID {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use executor bundle id is not the packaged GrokPtah identity",
            ));
        }
        if self.helper_bundle_id != HELPER_BUNDLE_ID {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "computer-use helper bundle id is not the declared helper identity",
            ));
        }
        versions_compatible(&self.version, &self.helper_version)?;
        match self.kind {
            ExecutorKind::InProcessHost if self.tcc_principal != APP_BUNDLE_ID => {
                Err(ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "in-process Computer Use must use the app bundle as the TCC principal",
                ))
            }
            ExecutorKind::PackagedHelper if self.tcc_principal != HELPER_BUNDLE_ID => {
                Err(ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "packaged helper Computer Use must use the helper bundle as the TCC principal",
                ))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageIdentity {
    pub schema: String,
    pub app_bundle_id: String,
    pub helper_bundle_id: String,
    pub app_version: String,
    pub helper_version: String,
    pub demo_target_bundle_id: String,
    pub computer_use_minimum_os: String,
    pub helper_nested_path: String,
}

impl PackageIdentity {
    pub fn canonical() -> Self {
        Self {
            schema: PACKAGE_IDENTITY_SCHEMA.to_string(),
            app_bundle_id: APP_BUNDLE_ID.to_string(),
            helper_bundle_id: HELPER_BUNDLE_ID.to_string(),
            app_version: APP_VERSION.to_string(),
            helper_version: HELPER_VERSION.to_string(),
            demo_target_bundle_id: DEMO_TARGET_BUNDLE_ID.to_string(),
            computer_use_minimum_os: COMPUTER_USE_MINIMUM_OS.to_string(),
            helper_nested_path: HELPER_NESTED_PATH.to_string(),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentedIdentity {
    schema: String,
    app: DocumentedBundle,
    helper: DocumentedHelper,
    demo_target: DocumentedBundle,
    computer_use_minimum_os_version: String,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentedBundle {
    product_name: String,
    bundle_id: String,
    #[serde(default)]
    executable: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentedHelper {
    product_name: String,
    bundle_id: String,
    executable: String,
    version: String,
    nested_path: String,
}

/// Major.minor must match. Older helper minor/major and newer helper major fail closed.
pub fn versions_compatible(app_version: &str, helper_version: &str) -> ComputerResult<()> {
    let app = parse_major_minor(app_version)?;
    let helper = parse_major_minor(helper_version)?;
    if helper.0 != app.0 {
        return Err(ComputerError::new(
            ComputerErrorCode::Unauthorized,
            "computer-use helper major version is incompatible with the app",
        ));
    }
    if helper.1 != app.1 {
        return Err(ComputerError::new(
            ComputerErrorCode::Unauthorized,
            "computer-use helper minor version is incompatible with the app",
        ));
    }
    Ok(())
}

fn parse_major_minor(version: &str) -> ComputerResult<(u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|part| part.parse().ok());
    let minor = parts.next().and_then(|part| part.parse().ok());
    match (major, minor) {
        (Some(major), Some(minor)) => Ok((major, minor)),
        _ => Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "computer-use version must be major.minor[.patch]",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedEligibility {
    pub packaged_qualification: bool,
    pub reasons: Vec<String>,
}

impl PackagedEligibility {
    pub fn evaluate(input: EligibilityInput) -> Self {
        let mut reasons = Vec::new();
        if input.disk_free_gib < 20.0 {
            reasons.push(format!("disk_below_20_gib:{:.1}", input.disk_free_gib));
        }
        if input.target_occupied {
            reasons.push("protected_or_shared_target_occupied".into());
        }
        if !input.signing_class.counts_as_packaged_release() {
            reasons.push(format!(
                "signing_class_{}_is_not_packaged_qualification",
                serde_json::to_value(input.signing_class)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".into())
            ));
        }
        if input.executor_kind != ExecutorKind::PackagedHelper {
            reasons.push("executor_is_not_the_packaged_helper".into());
        }
        if !input.helper_assembled {
            reasons.push("helper_binary_not_assembled_in_bundle".into());
        }
        if !input.screen_recording_granted || !input.accessibility_granted {
            reasons.push("packaged_tcc_grants_not_proven_for_helper_identity".into());
        }
        if !input.real_hardware_action_ran {
            reasons.push("real_packaged_semantic_hardware_action_did_not_run".into());
        }
        if input.simulator_or_fixture_only {
            reasons.push("simulator_or_synthetic_fixture_is_not_packaged_qualification".into());
        }
        Self {
            packaged_qualification: reasons.is_empty(),
            reasons,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EligibilityInput {
    pub disk_free_gib: f64,
    pub target_occupied: bool,
    pub signing_class: SigningClass,
    pub executor_kind: ExecutorKind,
    pub helper_assembled: bool,
    pub screen_recording_granted: bool,
    pub accessibility_granted: bool,
    pub real_hardware_action_ran: bool,
    pub simulator_or_fixture_only: bool,
}

pub fn documented_identity_json() -> &'static str {
    DOCUMENTED_IDENTITY_JSON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_identity_matches_constants() {
        let documented: DocumentedIdentity =
            serde_json::from_str(DOCUMENTED_IDENTITY_JSON).expect("identity json");
        assert_eq!(documented.schema, PACKAGE_IDENTITY_SCHEMA);
        assert_eq!(documented.app.product_name, APP_PRODUCT_NAME);
        assert_eq!(documented.app.bundle_id, APP_BUNDLE_ID);
        assert_eq!(documented.app.executable.as_deref(), Some(APP_EXECUTABLE));
        assert_eq!(documented.app.version.as_deref(), Some(APP_VERSION));
        assert_eq!(documented.helper.product_name, HELPER_PRODUCT_NAME);
        assert_eq!(documented.helper.bundle_id, HELPER_BUNDLE_ID);
        assert_eq!(documented.helper.executable, HELPER_EXECUTABLE);
        assert_eq!(documented.helper.version, HELPER_VERSION);
        assert_eq!(documented.helper.nested_path, HELPER_NESTED_PATH);
        assert_eq!(documented.demo_target.bundle_id, DEMO_TARGET_BUNDLE_ID);
        assert_eq!(
            documented.computer_use_minimum_os_version,
            COMPUTER_USE_MINIMUM_OS
        );
    }

    #[test]
    fn ad_hoc_and_unsigned_cannot_count_as_packaged() {
        for class in [
            SigningClass::Uninspected,
            SigningClass::Unsigned,
            SigningClass::AdHoc,
            SigningClass::AppleDevelopment,
            SigningClass::DeveloperId,
        ] {
            assert!(!class.counts_as_packaged_release(), "{class:?}");
        }
        assert!(SigningClass::NotarizedDeveloperId.counts_as_packaged_release());
    }

    #[test]
    fn codesign_parser_classifies_common_outputs() {
        assert_eq!(
            SigningClass::parse_codesign_display("Signature=adhoc\nflags=0x2(adhoc)"),
            SigningClass::AdHoc
        );
        assert_eq!(
            SigningClass::parse_codesign_display(
                "Authority=Developer ID Application: Example (ABCDE12345)"
            ),
            SigningClass::DeveloperId
        );
        assert_eq!(
            SigningClass::parse_codesign_display(
                "GrokPtah.app: accepted\nsource=Notarized Developer ID"
            ),
            SigningClass::NotarizedDeveloperId
        );
        assert_eq!(
            SigningClass::parse_codesign_display("code object is not signed at all"),
            SigningClass::Unsigned
        );
    }

    #[test]
    fn helper_version_skew_is_rejected() {
        versions_compatible("0.1.0", "0.1.9").unwrap();
        assert_eq!(
            versions_compatible("0.1.0", "0.2.0").unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            versions_compatible("0.1.0", "1.1.0").unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn in_process_host_is_not_packaged_qualification() {
        let eligibility = PackagedEligibility::evaluate(EligibilityInput {
            disk_free_gib: 64.0,
            target_occupied: false,
            signing_class: SigningClass::NotarizedDeveloperId,
            executor_kind: ExecutorKind::InProcessHost,
            helper_assembled: true,
            screen_recording_granted: true,
            accessibility_granted: true,
            real_hardware_action_ran: true,
            simulator_or_fixture_only: false,
        });
        assert!(!eligibility.packaged_qualification);
        assert!(eligibility
            .reasons
            .iter()
            .any(|reason| reason.contains("packaged_helper")));
    }

    #[test]
    fn checked_in_entitlements_are_empty_of_privilege_surfaces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for relative in [
            "desktop/src-tauri/macos/GrokPtah.entitlements",
            "desktop/src-tauri/macos/ComputerUseHelper.entitlements",
        ] {
            let body = std::fs::read_to_string(root.join(relative)).expect(relative);
            for forbidden in [
                "com.apple.security.app-sandbox",
                "com.apple.security.automation.apple-events",
                "keychain-access-groups",
                "CGEvent",
                "NSPasteboard",
            ] {
                assert!(!body.contains(forbidden), "{relative} contains {forbidden}");
            }
            assert!(body.contains("<dict>"));
        }
        let helper_info = std::fs::read_to_string(
            root.join("desktop/src-tauri/macos/ComputerUseHelper.Info.plist"),
        )
        .unwrap();
        assert!(helper_info.contains(HELPER_BUNDLE_ID));
        assert!(helper_info.contains(HELPER_EXECUTABLE));
        let tauri: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("desktop/src-tauri/tauri.conf.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tauri["identifier"], APP_BUNDLE_ID);
        assert_eq!(
            tauri["bundle"]["macOS"]["entitlements"],
            "macos/GrokPtah.entitlements"
        );
    }

    #[test]
    fn executor_identities_validate_tcc_principals() {
        ComputerExecutorIdentity::in_process_host(SigningClass::Uninspected)
            .validate()
            .unwrap();
        ComputerExecutorIdentity::packaged_helper(SigningClass::NotarizedDeveloperId)
            .validate()
            .unwrap();
        let mut mismatched = ComputerExecutorIdentity::packaged_helper(SigningClass::AdHoc);
        mismatched.tcc_principal = APP_BUNDLE_ID.to_string();
        assert_eq!(
            mismatched.validate().unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }
}
