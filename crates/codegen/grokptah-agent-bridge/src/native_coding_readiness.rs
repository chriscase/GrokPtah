//! Host-authored Native Coding readiness / admission projection.
//!
//! The host is the only authority for whether Execution or ManagerProposal
//! would currently be admitted for an exact provider/model. TypeScript and
//! other clients may format this record; they must not recreate the policy.
//! The projection never carries secrets, credential material, endpoints, or
//! cross-owner quota.

use serde::{Deserialize, Serialize};

use crate::gateway_config::{
    CapabilitySource, ComputerUseTier, ModelCapabilities, ModelSelection, ProviderModel,
    ProviderProfile, XAI_PROVIDER_ID,
};
use crate::orchestration::RunPurpose;

pub const NATIVE_CODING_READINESS_SCHEMA: &str = "grokptah.native-coding-readiness.v1";
pub const DESKTOP_OWNER_ID: &str = "primary";

const FORBIDDEN_PROJECTION_KEYS: &[&str] = &[
    "apiKey",
    "api_key",
    "authorization",
    "baseUrl",
    "base_url",
    "bearer",
    "credentialFingerprint",
    "credential_fingerprint",
    "credentialRef",
    "credential_ref",
    "endpointFingerprint",
    "endpoint_fingerprint",
    "password",
    "secret",
    "token",
];

/// Provenance of the capability record the host is currently using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationEvidence {
    Measured,
    Declared,
    Stale,
    Unknown,
}

/// What the host will currently permit for one purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionEligibility {
    MeasuredEligible,
    DeclaredUnmeasured,
    RequalifyRequired,
    DiscussionOnly,
    CredentialMissing,
    Unknown,
    ConfigurationIncomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionReasonCode {
    MeasuredEligible,
    DeclaredFirstUse,
    RequalifyRequired,
    ToolsNotPermitted,
    ChatEligible,
    UnknownCapabilities,
    ChatNotPermitted,
    CredentialMissing,
    ConfigurationIncomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurposeAdmission {
    pub eligibility: AdmissionEligibility,
    pub permitted: bool,
    pub reason_code: AdmissionReasonCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseAdmission {
    pub tier: ComputerUseTier,
    pub source: CapabilitySource,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCodingReadinessProjection {
    pub schema: String,
    pub owner_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub qualification_evidence: QualificationEvidence,
    pub execution: PurposeAdmission,
    pub manager_proposal: PurposeAdmission,
    pub computer_use: ComputerUseAdmission,
    pub credential_present: bool,
    pub configuration_complete: bool,
    pub managed_by_env: bool,
}

impl PurposeAdmission {
    fn blocked(eligibility: AdmissionEligibility, reason_code: AdmissionReasonCode) -> Self {
        Self {
            eligibility,
            permitted: false,
            reason_code,
        }
    }

    fn allowed(eligibility: AdmissionEligibility, reason_code: AdmissionReasonCode) -> Self {
        Self {
            eligibility,
            permitted: true,
            reason_code,
        }
    }

    /// Maps a capability-gate refusal onto the autonomous admission error.
    pub fn orch_error(self) -> Option<crate::orchestration::OrchError> {
        if self.permitted {
            return None;
        }
        let message = match self.reason_code {
            AdmissionReasonCode::UnknownCapabilities => {
                "provider route must be qualified or explicitly declared before autonomous admission"
            }
            AdmissionReasonCode::ChatNotPermitted => {
                "provider qualification does not allow chat generation"
            }
            AdmissionReasonCode::RequalifyRequired => {
                "provider qualification changed; requalify before autonomous execution"
            }
            AdmissionReasonCode::ToolsNotPermitted => {
                "provider qualification does not allow native coding tools"
            }
            AdmissionReasonCode::CredentialMissing
            | AdmissionReasonCode::ConfigurationIncomplete => {
                "provider route is not admitted"
            }
            AdmissionReasonCode::MeasuredEligible
            | AdmissionReasonCode::DeclaredFirstUse
            | AdmissionReasonCode::ChatEligible => return None,
        };
        Some(crate::orchestration::OrchError::new(
            crate::orchestration::OrchErrorCode::Conflict,
            message,
        ))
    }
}

/// Capability gate shared by autonomous admission and the operator projection.
pub fn admit_purpose(
    purpose: RunPurpose,
    source: CapabilitySource,
    chat: bool,
    tools: bool,
    stale_measured_qualification: bool,
) -> PurposeAdmission {
    if source == CapabilitySource::Unknown {
        return PurposeAdmission::blocked(
            AdmissionEligibility::Unknown,
            AdmissionReasonCode::UnknownCapabilities,
        );
    }
    if !chat {
        return PurposeAdmission::blocked(
            AdmissionEligibility::Unknown,
            AdmissionReasonCode::ChatNotPermitted,
        );
    }
    let eligible_from_source = if source == CapabilitySource::Measured {
        PurposeAdmission::allowed(
            AdmissionEligibility::MeasuredEligible,
            AdmissionReasonCode::MeasuredEligible,
        )
    } else {
        PurposeAdmission::allowed(
            AdmissionEligibility::DeclaredUnmeasured,
            AdmissionReasonCode::DeclaredFirstUse,
        )
    };
    match purpose {
        RunPurpose::ManagerProposal => PurposeAdmission::allowed(
            eligible_from_source.eligibility,
            AdmissionReasonCode::ChatEligible,
        ),
        RunPurpose::Execution => {
            if stale_measured_qualification && source == CapabilitySource::Declared {
                return PurposeAdmission::blocked(
                    AdmissionEligibility::RequalifyRequired,
                    AdmissionReasonCode::RequalifyRequired,
                );
            }
            if !tools {
                return PurposeAdmission::blocked(
                    AdmissionEligibility::DiscussionOnly,
                    AdmissionReasonCode::ToolsNotPermitted,
                );
            }
            eligible_from_source
        }
    }
}

/// Computer Use is projected independently of coding-tool readiness.
pub fn computer_use_admission(capabilities: &ModelCapabilities) -> ComputerUseAdmission {
    let source = capabilities.computer_capability_source;
    let mut tier = if source == CapabilitySource::Unknown {
        ComputerUseTier::None
    } else {
        capabilities.computer_use_tier
    };
    if tier == ComputerUseTier::VisualFallbackAct
        && (!capabilities.image_input
            || capabilities.image_media_types.is_empty()
            || capabilities.max_image_bytes.is_none())
    {
        tier = ComputerUseTier::SemanticAct;
    }
    ComputerUseAdmission {
        tier,
        source,
        enabled: source == CapabilitySource::Measured && tier > ComputerUseTier::None,
    }
}

pub fn qualification_evidence(
    source: CapabilitySource,
    stale_measured_qualification: bool,
) -> QualificationEvidence {
    match source {
        CapabilitySource::Measured => QualificationEvidence::Measured,
        CapabilitySource::Declared if stale_measured_qualification => QualificationEvidence::Stale,
        CapabilitySource::Declared => QualificationEvidence::Declared,
        CapabilitySource::Unknown => QualificationEvidence::Unknown,
    }
}

fn normalize_owner_id(owner_id: &str) -> String {
    let owner_id = owner_id.trim();
    if owner_id.is_empty() || owner_id.len() > 128 {
        DESKTOP_OWNER_ID.to_string()
    } else {
        owner_id.to_string()
    }
}

fn incomplete(
    owner_id: &str,
    provider_id: &str,
    model_id: &str,
    managed_by_env: bool,
) -> NativeCodingReadinessProjection {
    NativeCodingReadinessProjection {
        schema: NATIVE_CODING_READINESS_SCHEMA.into(),
        owner_id: normalize_owner_id(owner_id),
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        qualification_evidence: QualificationEvidence::Unknown,
        execution: PurposeAdmission::blocked(
            AdmissionEligibility::ConfigurationIncomplete,
            AdmissionReasonCode::ConfigurationIncomplete,
        ),
        manager_proposal: PurposeAdmission::blocked(
            AdmissionEligibility::ConfigurationIncomplete,
            AdmissionReasonCode::ConfigurationIncomplete,
        ),
        computer_use: ComputerUseAdmission {
            tier: ComputerUseTier::None,
            source: CapabilitySource::Unknown,
            enabled: false,
        },
        credential_present: false,
        configuration_complete: false,
        managed_by_env,
    }
}

fn apply_credential_gate(
    admission: PurposeAdmission,
    credential_present: bool,
) -> PurposeAdmission {
    if credential_present {
        return admission;
    }
    PurposeAdmission::blocked(
        AdmissionEligibility::CredentialMissing,
        AdmissionReasonCode::CredentialMissing,
    )
}

pub fn project_from_resolved(
    owner_id: &str,
    provider_id: &str,
    model_id: &str,
    profile: Option<&ProviderProfile>,
    model: Option<&ProviderModel>,
    credential_present: bool,
    stale_measured_qualification: bool,
) -> NativeCodingReadinessProjection {
    let owner_id = normalize_owner_id(owner_id);
    let Some(profile) = profile else {
        return incomplete(&owner_id, provider_id, model_id, false);
    };
    if provider_id.trim().is_empty() || model_id.trim().is_empty() {
        return incomplete(&owner_id, provider_id, model_id, profile.managed_by_env);
    }
    let capabilities = model
        .map(|model| model.capabilities.clone())
        .unwrap_or_default();
    let evidence = if model.is_none() {
        QualificationEvidence::Unknown
    } else {
        qualification_evidence(capabilities.source, stale_measured_qualification)
    };
    let source = if model.is_none() {
        CapabilitySource::Unknown
    } else {
        capabilities.source
    };
    let chat = model.is_some_and(|_| capabilities.chat);
    let tools = model.is_some_and(|_| capabilities.tools);
    let execution = apply_credential_gate(
        admit_purpose(
            RunPurpose::Execution,
            source,
            chat,
            tools,
            stale_measured_qualification,
        ),
        credential_present,
    );
    let manager_proposal = apply_credential_gate(
        admit_purpose(
            RunPurpose::ManagerProposal,
            source,
            chat,
            tools,
            stale_measured_qualification,
        ),
        credential_present,
    );
    NativeCodingReadinessProjection {
        schema: NATIVE_CODING_READINESS_SCHEMA.into(),
        owner_id,
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        qualification_evidence: evidence,
        execution,
        manager_proposal,
        computer_use: computer_use_admission(&capabilities),
        credential_present,
        configuration_complete: true,
        managed_by_env: profile.managed_by_env,
    }
}

fn resolve_route_inputs(
    provider_id: &str,
    model_id: &str,
) -> (Option<ProviderProfile>, Option<ProviderModel>, bool, bool) {
    let selection = ModelSelection {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
    };
    let (credential_present, oidc, fingerprint) =
        match crate::auth_store::resolve_provider_credentials(provider_id, Some(model_id)) {
            Ok(Some(credentials)) => {
                let fingerprint = credentials.qualification_identity_fingerprint();
                let oidc = credentials.oidc_token_auth;
                drop(credentials);
                (true, oidc, Some(fingerprint))
            }
            Ok(None) | Err(_) => (false, false, None),
        };
    let profile = crate::gateway_config::resolve_profile_for_selection(
        &selection,
        oidc,
        fingerprint.as_deref(),
    )
    .ok();
    let model = profile.as_ref().and_then(|profile| {
        profile
            .models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
            .or_else(|| {
                let trimmed = model_id.trim();
                profile
                    .models
                    .iter()
                    .find(|model| model.id == trimmed)
                    .cloned()
            })
    });
    let stale = crate::gateway_config::has_measured_xai_qualification(provider_id, model_id)
        .or_else(|_| {
            crate::gateway_config::has_measured_xai_qualification(provider_id, model_id.trim())
        })
        .unwrap_or(true);
    (profile, model, credential_present, stale)
}

/// Owner-scoped secret-free projection for one exact provider/model.
pub fn project_for_owner(
    owner_id: &str,
    provider_id: &str,
    model_id: &str,
) -> NativeCodingReadinessProjection {
    let provider_id_trimmed = provider_id.trim();
    if provider_id_trimmed.is_empty() || model_id.trim().is_empty() {
        return incomplete(owner_id, provider_id, model_id, false);
    }
    let lookup_provider = if provider_id_trimmed == XAI_PROVIDER_ID {
        XAI_PROVIDER_ID
    } else {
        provider_id_trimmed
    };
    let (profile, model, credential_present, stale) =
        resolve_route_inputs(lookup_provider, model_id);
    project_from_resolved(
        owner_id,
        provider_id,
        model_id,
        profile.as_ref(),
        model.as_ref(),
        credential_present,
        stale,
    )
}

pub fn projection_value(projection: &NativeCodingReadinessProjection) -> serde_json::Value {
    serde_json::to_value(projection).unwrap_or_else(|_| serde_json::json!({}))
}

pub fn projection_contains_forbidden_fields(value: &serde_json::Value) -> bool {
    contains_forbidden_key(value)
}

fn contains_forbidden_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.keys().any(|key| {
                FORBIDDEN_PROJECTION_KEYS
                    .iter()
                    .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
            }) || map.values().any(contains_forbidden_key)
        }
        serde_json::Value::Array(values) => values.iter().any(contains_forbidden_key),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certification::scan_value_for_forbidden_data;
    use crate::discover::{home_override_serial, set_grokptah_home_override};
    use crate::gateway_config::{
        model_selection_key, save, save_managed_profile_capabilities, GatewayConfig,
        ProviderDeadlineClass, ProviderDialect, ProviderKind, CAPABILITY_QUALIFICATION_SCHEMA,
        COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA,
    };
    use crate::orchestration::{
        OrchErrorCode, OrchStore, ProviderAttemptState, ProviderRouteSnapshot,
        ProviderSendCertainty, QuotaClass, QuotaLimits, QuotaReservation, QuotaReservationState,
        RunAggregates, RunBounds, RunPurpose, RunRecord, RunState,
        PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
    };
    use crate::types::EffortLevel;
    use chrono::Utc;
    use uuid::Uuid;

    fn coding_caps(source: CapabilitySource) -> ModelCapabilities {
        ModelCapabilities {
            chat: true,
            tools: true,
            stream: true,
            source,
            qualification_schema: (source == CapabilitySource::Measured)
                .then(|| CAPABILITY_QUALIFICATION_SCHEMA.into()),
            ..ModelCapabilities::default()
        }
    }

    fn chat_only_caps(source: CapabilitySource) -> ModelCapabilities {
        ModelCapabilities {
            chat: true,
            tools: false,
            source,
            qualification_schema: (source == CapabilitySource::Measured)
                .then(|| CAPABILITY_QUALIFICATION_SCHEMA.into()),
            ..ModelCapabilities::default()
        }
    }

    fn profile_with(
        id: &str,
        model_id: &str,
        caps: ModelCapabilities,
        managed_by_env: bool,
    ) -> (ProviderProfile, ProviderModel) {
        let mut profile = ProviderProfile::openai_compatible(id, id, "http://127.0.0.1:9/v1");
        profile.managed_by_env = managed_by_env;
        let mut model = ProviderModel::unqualified(model_id);
        model.capabilities = caps;
        profile.upsert_model(model.clone());
        (profile, model)
    }

    fn assert_secret_free(projection: &NativeCodingReadinessProjection) {
        let value = projection_value(projection);
        assert!(
            !projection_contains_forbidden_fields(&value),
            "forbidden field in {value}"
        );
        scan_value_for_forbidden_data(&value)
            .expect("projection must be certification-secret-free");
        let encoded = value.to_string();
        for needle in [
            "sk-",
            "Bearer ",
            "http://",
            "https://",
            "api_key",
            "credential_ref",
            "baseUrl",
            "xai-abc",
        ] {
            assert!(
                !encoded.contains(needle),
                "projection leaked {needle}: {encoded}"
            );
        }
    }

    #[test]
    fn measured_chat_tools_are_eligible_for_execution() {
        let admission = admit_purpose(
            RunPurpose::Execution,
            CapabilitySource::Measured,
            true,
            true,
            false,
        );
        assert!(admission.permitted);
        assert_eq!(
            admission.eligibility,
            AdmissionEligibility::MeasuredEligible
        );
        assert!(admission.orch_error().is_none());
    }

    #[test]
    fn declared_chat_tools_before_measurement_are_eligible_unmeasured() {
        let admission = admit_purpose(
            RunPurpose::Execution,
            CapabilitySource::Declared,
            true,
            true,
            false,
        );
        assert!(admission.permitted);
        assert_eq!(
            admission.eligibility,
            AdmissionEligibility::DeclaredUnmeasured
        );
        let (profile, model) = profile_with(
            "corp",
            "first-use",
            coding_caps(CapabilitySource::Declared),
            false,
        );
        let projection = project_from_resolved(
            "owner-a",
            "corp",
            "first-use",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        assert_eq!(
            projection.qualification_evidence,
            QualificationEvidence::Declared
        );
        assert!(projection.execution.permitted);
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::DeclaredUnmeasured
        );
        assert!(projection.manager_proposal.permitted);
        assert!(!projection.computer_use.enabled);
        assert_secret_free(&projection);
    }

    #[test]
    fn stale_measured_history_blocks_declared_execution_and_reports_requalify() {
        let execution = admit_purpose(
            RunPurpose::Execution,
            CapabilitySource::Declared,
            true,
            true,
            true,
        );
        assert!(!execution.permitted);
        assert_eq!(
            execution.eligibility,
            AdmissionEligibility::RequalifyRequired
        );
        let error = execution.orch_error().unwrap();
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert!(error.message.contains("requalify"));
        let proposal = admit_purpose(
            RunPurpose::ManagerProposal,
            CapabilitySource::Declared,
            true,
            true,
            true,
        );
        assert!(proposal.permitted);
        let (profile, model) = profile_with(
            "xai",
            "grok-route",
            coding_caps(CapabilitySource::Declared),
            false,
        );
        let projection = project_from_resolved(
            "owner-a",
            "xai",
            "grok-route",
            Some(&profile),
            Some(&model),
            true,
            true,
        );
        assert_eq!(
            projection.qualification_evidence,
            QualificationEvidence::Stale
        );
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::RequalifyRequired
        );
        assert!(!projection.execution.permitted);
        assert!(projection.manager_proposal.permitted);
        assert_secret_free(&projection);
    }

    #[test]
    fn unknown_capabilities_never_enter_execution_or_manager_proposal() {
        for purpose in [RunPurpose::Execution, RunPurpose::ManagerProposal] {
            let admission = admit_purpose(purpose, CapabilitySource::Unknown, true, true, false);
            assert!(!admission.permitted);
            assert_eq!(admission.eligibility, AdmissionEligibility::Unknown);
        }
        let (profile, model) = profile_with("corp", "mystery", ModelCapabilities::default(), false);
        let projection = project_from_resolved(
            "owner-a",
            "corp",
            "mystery",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        assert!(!projection.execution.permitted);
        assert!(!projection.manager_proposal.permitted);
        assert_eq!(
            projection.qualification_evidence,
            QualificationEvidence::Unknown
        );
        assert_secret_free(&projection);
    }

    #[test]
    fn chat_only_measured_permits_manager_proposal_and_refuses_execution() {
        let (profile, model) = profile_with(
            "corp",
            "chat-only",
            chat_only_caps(CapabilitySource::Measured),
            false,
        );
        let projection = project_from_resolved(
            "owner-a",
            "corp",
            "chat-only",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        assert!(!projection.execution.permitted);
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::DiscussionOnly
        );
        assert!(projection.manager_proposal.permitted);
        assert_eq!(
            projection.qualification_evidence,
            QualificationEvidence::Measured
        );
        assert_secret_free(&projection);
    }

    #[test]
    fn computer_use_never_enables_because_coding_tools_are_ready() {
        let mut caps = coding_caps(CapabilitySource::Measured);
        caps.computer_use_tier = ComputerUseTier::SemanticAct;
        caps.computer_capability_source = CapabilitySource::Unknown;
        let admission = computer_use_admission(&caps);
        assert!(!admission.enabled);
        assert_eq!(admission.tier, ComputerUseTier::None);

        caps.computer_capability_source = CapabilitySource::Declared;
        let declared = computer_use_admission(&caps);
        assert!(!declared.enabled);
        assert_eq!(declared.source, CapabilitySource::Declared);
        assert_eq!(declared.tier, ComputerUseTier::SemanticAct);

        caps.computer_capability_source = CapabilitySource::Measured;
        caps.computer_qualification_schema = Some(COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA.into());
        let measured = computer_use_admission(&caps);
        assert!(measured.enabled);
        assert_eq!(measured.tier, ComputerUseTier::SemanticAct);
    }

    #[test]
    fn missing_credential_blocks_admission_but_preserves_measured_evidence() {
        let (profile, model) = profile_with(
            "corp",
            "ready",
            coding_caps(CapabilitySource::Measured),
            false,
        );
        let projection = project_from_resolved(
            "owner-a",
            "corp",
            "ready",
            Some(&profile),
            Some(&model),
            false,
            false,
        );
        assert_eq!(
            projection.qualification_evidence,
            QualificationEvidence::Measured
        );
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::CredentialMissing
        );
        assert!(!projection.execution.permitted);
        assert!(!projection.manager_proposal.permitted);
        assert!(!projection.credential_present);
        assert_secret_free(&projection);
    }

    #[test]
    fn projection_is_owner_scoped_and_omits_unrelated_provider_identity() {
        let (profile, model) = profile_with(
            "corp-a",
            "model-a",
            coding_caps(CapabilitySource::Declared),
            false,
        );
        let for_a = project_from_resolved(
            "owner-a",
            "corp-a",
            "model-a",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        let for_b = project_from_resolved(
            "owner-b",
            "corp-a",
            "model-a",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        assert_eq!(for_a.owner_id, "owner-a");
        assert_eq!(for_b.owner_id, "owner-b");
        let encoded = projection_value(&for_a).to_string();
        assert!(!encoded.contains("owner-b"));
        assert!(!encoded.contains("corp-b"));
        assert_secret_free(&for_a);
        assert_secret_free(&for_b);
    }

    #[test]
    fn incomplete_configuration_is_not_admitted() {
        let projection = project_from_resolved("owner-a", "", "", None, None, false, false);
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::ConfigurationIncomplete
        );
        assert!(!projection.configuration_complete);
        assert_secret_free(&projection);
    }

    #[test]
    fn project_for_owner_sees_stale_xai_history_without_exposing_route_secrets() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(temp.path().join(".grokptah")));
        let mut measured = ProviderModel::unqualified("grok-route");
        measured.capabilities = coding_caps(CapabilitySource::Measured);
        let profile = ProviderProfile {
            id: XAI_PROVIDER_ID.into(),
            label: "xAI".into(),
            kind: ProviderKind::Xai,
            dialect: ProviderDialect::XaiChatCompletions,
            deadline_class: ProviderDeadlineClass::Standard,
            base_url: "https://api.x.ai/v1".into(),
            credential_ref: Some("managed:xai:api-key".into()),
            models: vec![measured.clone()],
            managed_by_env: false,
            managed_by_host: true,
        };
        save_managed_profile_capabilities(&profile, &measured, "v1-sha256:test-fingerprint")
            .unwrap();
        let mut stored = GatewayConfig::default();
        let (compat, model) = profile_with(
            "company-gateway",
            "Team/Code:Cheap",
            coding_caps(CapabilitySource::Declared),
            false,
        );
        stored.upsert_profile(compat.clone()).unwrap();
        save(&stored).unwrap();

        let stale = project_for_owner("owner-a", XAI_PROVIDER_ID, "grok-route");
        assert_eq!(stale.qualification_evidence, QualificationEvidence::Stale);
        if stale.credential_present {
            assert_eq!(
                stale.execution.eligibility,
                AdmissionEligibility::RequalifyRequired
            );
            assert!(!stale.execution.permitted);
            assert!(stale.manager_proposal.permitted);
        } else {
            assert_eq!(
                stale.execution.eligibility,
                AdmissionEligibility::CredentialMissing
            );
            assert!(!stale.execution.permitted);
            assert!(!stale.manager_proposal.permitted);
        }
        assert_secret_free(&stale);

        let declared = project_for_owner("owner-a", "company-gateway", "Team/Code:Cheap");
        assert_eq!(
            declared.qualification_evidence,
            QualificationEvidence::Declared
        );
        assert_eq!(declared.provider_id, "company-gateway");
        assert!(!projection_value(&declared)
            .to_string()
            .contains("grok-route"));
        assert_secret_free(&declared);
        let _ = model;
        set_grokptah_home_override(None);
    }

    #[test]
    fn measured_chat_tools_projection_is_eligible_for_execution() {
        let (profile, model) = profile_with(
            "corp",
            "ready",
            coding_caps(CapabilitySource::Measured),
            false,
        );
        let projection = project_from_resolved(
            "owner-a",
            "corp",
            "ready",
            Some(&profile),
            Some(&model),
            true,
            false,
        );
        assert!(projection.execution.permitted);
        assert_eq!(
            projection.execution.eligibility,
            AdmissionEligibility::MeasuredEligible
        );
        assert!(projection.manager_proposal.permitted);
        assert!(!projection.computer_use.enabled);
        assert_secret_free(&projection);
    }

    fn sealed_coding_route(model_id: &str) -> ProviderRouteSnapshot {
        ProviderRouteSnapshot {
            schema_version: PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
            provider_id: "company-gateway".into(),
            model_id: model_id.into(),
            wire_model_id: model_id.into(),
            selection_key: model_selection_key("company-gateway", model_id),
            kind: ProviderKind::OpenAiCompatible,
            dialect: ProviderDialect::OpenAiChatCompletions,
            base_url: "http://127.0.0.1:9/v1".into(),
            endpoint_fingerprint: String::new(),
            credential_ref: "keychain:provider/company-gateway/api-key".into(),
            credential_fingerprint: "v1-sha256:test-credential".into(),
            capabilities: coding_caps(CapabilitySource::Measured),
            deadline_class: ProviderDeadlineClass::Standard,
            effort: EffortLevel::Medium,
            qualification_record_id: None,
            quota_class: None,
            quota_reservation_id: None,
            snapshot_hash: String::new(),
        }
        .seal()
        .unwrap()
    }

    fn running_quota_backed_run(model_id: &str) -> (RunRecord, QuotaReservation) {
        let now = Utc::now();
        let mut run = RunRecord {
            run_id: "native-rc-run".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/native-rc".into(),
            request_id: "native-rc-req".into(),
            client_id: Some("native-executor".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            provider_route: None,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds {
                max_prompt_bytes: 1024,
                max_rounds: 4,
                max_duration_ms: 30_000,
                max_total_tokens: Some(1_000),
            },
            prompt_preview: "frozen route".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        run.provider_route = Some(
            sealed_coding_route(model_id)
                .bind_quota(QuotaClass::CodingExecution, "quota-native-rc-run")
                .unwrap(),
        );
        let reservation =
            QuotaReservation::for_run(&run, "owner-a", QuotaLimits::default(), now).unwrap();
        (run, reservation)
    }

    #[test]
    fn provider_spec_changes_do_not_alter_a_frozen_run_route_or_duplicate_on_restart() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(temp.path().join(".grokptah")));
        let (profile, model) = profile_with(
            "company-gateway",
            "Team/Code:Cheap",
            coding_caps(CapabilitySource::Declared),
            false,
        );
        let mut stored = GatewayConfig::default();
        stored.upsert_profile(profile.clone()).unwrap();
        save(&stored).unwrap();

        let store_root = temp.path().join("orch");
        let store = OrchStore::open(&store_root).unwrap();
        let (run, reservation) = running_quota_backed_run("Team/Code:Cheap");
        let frozen = run.provider_route.clone().unwrap();
        store.save_run_with_quota(&run, &reservation).unwrap();
        let attempt = store.begin_provider_attempt(&run.run_id).unwrap();
        assert_eq!(attempt.state, ProviderAttemptState::Admitted);

        let mut chat_only = profile;
        let mut changed = model;
        changed.capabilities = chat_only_caps(CapabilitySource::Declared);
        chat_only.upsert_model(changed);
        stored.upsert_profile(chat_only).unwrap();
        save(&stored).unwrap();

        let loaded = store.load_run(&run.run_id).unwrap().unwrap();
        assert_eq!(loaded.purpose, RunPurpose::Execution);
        assert_eq!(loaded.provider_route.as_ref(), Some(&frozen));
        assert_eq!(
            loaded.provider_route.as_ref().unwrap().capabilities.source,
            CapabilitySource::Measured
        );
        assert!(loaded.provider_route.as_ref().unwrap().capabilities.tools);
        let current = project_for_owner("owner-a", "company-gateway", "Team/Code:Cheap");
        assert_eq!(
            current.qualification_evidence,
            QualificationEvidence::Declared
        );
        assert_ne!(
            current.execution.eligibility,
            AdmissionEligibility::MeasuredEligible
        );
        assert_secret_free(&current);
        drop(store);

        let reopened = OrchStore::open(&store_root).unwrap();
        let runs = reopened.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, run.run_id);
        assert_eq!(runs[0].purpose, RunPurpose::Execution);
        assert_eq!(runs[0].provider_route.as_ref(), Some(&frozen));
        let reservations = reopened.list_quota_reservations().unwrap();
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].reservation_id, reservation.reservation_id);
        assert_eq!(reservations[0].run_id, run.run_id);
        assert_eq!(reservations[0].state, QuotaReservationState::Reserved);
        let attempts = reopened.list_provider_attempts().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].attempt_id, attempt.attempt_id);
        assert_eq!(attempts[0].run_id, run.run_id);
        assert_eq!(
            attempts[0].send_certainty,
            Some(ProviderSendCertainty::UncertainAccept)
        );
        let after_restart = project_for_owner("owner-a", "company-gateway", "Team/Code:Cheap");
        assert_eq!(after_restart, current);
        set_grokptah_home_override(None);
    }
}
