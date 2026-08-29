//! Packaged macOS Computer Use identity as exposed to the bridge.
//!
//! This module is a thin, honest projection of what the isolated-visual crate
//! established. It deliberately offers no constructor that manufactures a
//! packaged-helper identity from a Team ID: a designated requirement is
//! something the operating system derives from a signature and an operator
//! declares in a trust root, never something this process formats into a
//! string. The only way to obtain a `PackagedHelper` executor identity is to
//! pass an [`AdmittedHelperIdentity`], which cannot be constructed outside
//! `admit_packaged_helper`.

use serde::{Deserialize, Serialize};

use super::types::{validate_id, ComputerError, ComputerErrorCode, ComputerResult};

pub use grokptah_isolated_visual::{
    documented_identity_json, AdmittedHelperIdentity, SigningClass, APP_BUNDLE_ID, APP_EXECUTABLE,
    APP_MINIMUM_OS, APP_PRODUCT_NAME, APP_VERSION, COMPUTER_USE_MINIMUM_OS, DEMO_TARGET_BUNDLE_ID,
    HELPER_BUNDLE_ID, HELPER_EXECUTABLE, HELPER_MINIMUM_OS, HELPER_NESTED_PATH,
    HELPER_PRODUCT_NAME, HELPER_VERSION, PACKAGE_IDENTITY_SCHEMA,
};

pub const PACKAGE_AUTHORITY_EVIDENCE_SCHEMA: &str = "grokptah-computer-use-package-authority.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// Computer Use running inside the app process. TCC attaches to the app
    /// bundle, and this is never packaged-helper qualification.
    #[default]
    InProcessHost,
    /// The separately signed helper bundle. Reachable only from an admitted
    /// identity.
    PackagedHelper,
}

/// The code identity that TCC Screen Recording / Accessibility attach to.
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
    /// The honest identity of Computer Use running in-process: the app bundle,
    /// with no signing claim of its own.
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

    /// Build a packaged-helper identity from an identity the authority already
    /// admitted. The Team ID and designated requirement are copied from the
    /// admitted record; nothing here formats a requirement string.
    pub fn from_admitted_helper(admitted: &AdmittedHelperIdentity) -> ComputerResult<Self> {
        let identity = Self {
            kind: ExecutorKind::PackagedHelper,
            bundle_id: APP_BUNDLE_ID.to_string(),
            helper_bundle_id: admitted.bundle_id.clone(),
            version: APP_VERSION.to_string(),
            helper_version: HELPER_VERSION.to_string(),
            signing_class: admitted.signing_class,
            tcc_principal: admitted.bundle_id.clone(),
            team_id: Some(admitted.team_id.clone()),
            designated_requirement: Some(admitted.designated_requirement.clone()),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("bundle_id", &self.bundle_id)?;
        validate_id("helper_bundle_id", &self.helper_bundle_id)?;
        validate_id("tcc_principal", &self.tcc_principal)?;
        if self.bundle_id != APP_BUNDLE_ID {
            return Err(unauthorized(
                "computer-use executor bundle id is not the packaged GrokPtah identity",
            ));
        }
        if self.helper_bundle_id != HELPER_BUNDLE_ID {
            return Err(unauthorized(
                "computer-use helper bundle id is not the declared helper identity",
            ));
        }
        versions_compatible(&self.version, &self.helper_version)?;
        match self.kind {
            ExecutorKind::InProcessHost => {
                if self.tcc_principal != APP_BUNDLE_ID {
                    return Err(unauthorized(
                        "in-process Computer Use must use the app bundle as the TCC principal",
                    ));
                }
                // In-process identity must not carry packaged-helper claims.
                if self.team_id.is_some() || self.designated_requirement.is_some() {
                    return Err(unauthorized(
                        "in-process Computer Use cannot claim a helper signing identity",
                    ));
                }
                Ok(())
            }
            ExecutorKind::PackagedHelper => {
                if self.tcc_principal != HELPER_BUNDLE_ID {
                    return Err(unauthorized(
                        "packaged helper Computer Use must use the helper bundle as the TCC principal",
                    ));
                }
                let has_team = self.team_id.as_deref().is_some_and(|team| !team.is_empty());
                let has_requirement = self
                    .designated_requirement
                    .as_deref()
                    .is_some_and(|requirement| !requirement.is_empty());
                if !has_team || !has_requirement || !self.signing_class.counts_as_packaged_release()
                {
                    return Err(unauthorized(
                        "packaged helper identity is not backed by an admitted notarized signature",
                    ));
                }
                Ok(())
            }
        }
    }
}

fn unauthorized(message: &str) -> ComputerError {
    ComputerError::new(ComputerErrorCode::Unauthorized, message)
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

/// Major.minor must match, with strict major.minor.patch parsing.
pub fn versions_compatible(app_version: &str, helper_version: &str) -> ComputerResult<()> {
    grokptah_isolated_visual::versions_compatible(app_version, helper_version).map_err(|error| {
        ComputerError::new(
            match error.code {
                grokptah_isolated_visual::IsolatedErrorCode::Unauthorized => {
                    ComputerErrorCode::Unauthorized
                }
                _ => ComputerErrorCode::InvalidRequest,
            },
            error.message,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DocumentedIdentity {
        schema: String,
        app: DocumentedBundle,
        helper: DocumentedHelper,
        demo_target: DocumentedBundle,
        computer_use_minimum_os_version: String,
    }

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

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DocumentedHelper {
        product_name: String,
        bundle_id: String,
        executable: String,
        version: String,
        nested_path: String,
    }

    fn admitted(team: &str) -> AdmittedHelperIdentity {
        AdmittedHelperIdentity {
            bundle_id: HELPER_BUNDLE_ID.into(),
            team_id: team.into(),
            designated_requirement: format!(
                "identifier \"{HELPER_BUNDLE_ID}\" and anchor apple generic and \
                 certificate leaf[subject.OU] = {team}"
            ),
            signing_class: SigningClass::NotarizedDeveloperId,
            executable_digest: "a".repeat(64),
            entitlements_digest: "b".repeat(64),
            bundle_manifest_digest: "c".repeat(64),
            trust_anchor_digest: "d".repeat(64),
        }
    }

    #[test]
    fn documented_identity_matches_constants() {
        let documented: DocumentedIdentity =
            serde_json::from_str(documented_identity_json()).expect("identity json");
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
    fn only_notarized_developer_id_counts_as_packaged() {
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
    fn a_packaged_identity_can_only_come_from_an_admitted_signature() {
        let identity = ComputerExecutorIdentity::from_admitted_helper(&admitted("TEAMID1234"))
            .expect("admitted identity");
        assert_eq!(identity.kind, ExecutorKind::PackagedHelper);
        assert_eq!(identity.tcc_principal, HELPER_BUNDLE_ID);

        // A non-notarized admitted record cannot exist in practice, but if one
        // were forged in memory the identity still refuses to validate.
        let mut weak = admitted("TEAMID1234");
        weak.signing_class = SigningClass::AdHoc;
        assert!(ComputerExecutorIdentity::from_admitted_helper(&weak).is_err());
    }

    #[test]
    fn in_process_identity_cannot_claim_helper_signing() {
        let mut identity = ComputerExecutorIdentity::in_process_host(SigningClass::Uninspected);
        identity.validate().expect("honest in-process identity");
        identity.team_id = Some("TEAMID1234".into());
        identity.designated_requirement = Some("identifier \"anything\"".into());
        assert_eq!(
            identity.validate().unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn in_process_host_tcc_principal_is_the_app_bundle() {
        let mut identity = ComputerExecutorIdentity::in_process_host(SigningClass::Uninspected);
        identity.tcc_principal = HELPER_BUNDLE_ID.into();
        assert_eq!(
            identity.validate().unwrap_err().code,
            ComputerErrorCode::Unauthorized
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
}
