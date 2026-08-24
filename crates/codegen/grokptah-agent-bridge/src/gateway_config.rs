//! Provider-scoped gateway profiles (#278).
//!
//! Config contains endpoint/model metadata and credential references only. The
//! legacy top-level `api_key` remains readable for one safe migration, but new
//! writes reject it so a bearer cannot be persisted back to disk.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;

pub const PROVIDER_CONFIG_VERSION: u32 = 2;
pub const XAI_PROVIDER_ID: &str = "xai";
pub const MODEL_SELECTION_PREFIX: &str = "ptah.model.v1:";
pub const CAPABILITY_QUALIFICATION_SCHEMA: &str = "grokptah.provider-qualification.v1";
pub const COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA: &str = "grokptah.computer-qualification.v1";
/// Separate qualification evidence is required before a model may receive
/// visual fallback authority. The ordinary computer qualification only
/// measures semantic observation/action against the in-process fixture.
pub const ISOLATED_VISUAL_COMPUTER_QUALIFICATION_SCHEMA: &str =
    "grokptah.isolated-visual-computer-qualification.v1";
pub const MAX_QUALIFIED_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MANAGED_QUALIFICATIONS_VERSION: u32 = 1;
const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp"];

fn current_version() -> u32 {
    PROVIDER_CONFIG_VERSION
}

/// Provider family. This selects authentication and transport behavior; it is
/// never inferred from a model name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Xai,
    OpenAiCompatible,
}

/// Exact request dialect used by a provider profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDialect {
    XaiChatCompletions,
    OpenAiChatCompletions,
}

/// Bounded request budgets selected per provider profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDeadlineClass {
    Interactive,
    #[default]
    Standard,
    Extended,
}

impl ProviderDeadlineClass {
    pub fn agent_timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Self::Interactive => 90,
            Self::Standard => 180,
            Self::Extended => 300,
        })
    }

    pub fn chat_timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Self::Interactive => 60,
            Self::Standard => 120,
            Self::Extended => 240,
        })
    }
}

/// Validated input used to create or update one compatible provider profile.
#[derive(Clone)]
pub struct ProviderProfileUpdate {
    pub provider_id: String,
    pub label: String,
    pub base_url: String,
    pub model_id: String,
    pub deadline_class: ProviderDeadlineClass,
    pub effort_options: Vec<String>,
    pub api_key: Option<String>,
}

/// Provenance of a model capability statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    Declared,
    Measured,
    #[default]
    Unknown,
}

/// Maximum Computer Use authority qualified for one exact provider/model.
/// Unknown and legacy profiles deserialize to `None` and receive no tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseTier {
    #[default]
    None,
    Observe,
    SemanticAct,
    VisualFallbackAct,
}

impl ComputerUseTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Observe => "observe",
            Self::SemanticAct => "semantic_act",
            Self::VisualFallbackAct => "visual_fallback_act",
        }
    }

    pub fn allows_observation(self) -> bool {
        self >= Self::Observe
    }

    pub fn allows_semantic_action(self) -> bool {
        self >= Self::SemanticAct
    }

    pub fn allows_visual_fallback(self) -> bool {
        self >= Self::VisualFallbackAct
    }
}

/// Capabilities for one exact provider/model pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default = "default_true")]
    pub chat: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub image_media_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_image_bytes: Option<u64>,
    #[serde(default)]
    pub computer_use_tier: ComputerUseTier,
    #[serde(default)]
    pub computer_capability_source: CapabilitySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_qualification_schema: Option<String>,
    #[serde(default)]
    pub effort_options: Vec<String>,
    #[serde(default)]
    pub source: CapabilitySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualification_schema: Option<String>,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            chat: true,
            tools: false,
            stream: false,
            parallel_tool_calls: false,
            image_input: false,
            image_media_types: Vec::new(),
            max_image_bytes: None,
            computer_use_tier: ComputerUseTier::None,
            computer_capability_source: CapabilitySource::Unknown,
            computer_qualification_schema: None,
            effort_options: Vec::new(),
            source: CapabilitySource::Unknown,
            qualification_schema: None,
        }
    }
}

impl ModelCapabilities {
    /// Returns only authority supported by a coherent, source-attributed
    /// capability record. Callers must use this value for tool discovery.
    pub fn effective_computer_use_tier(&self) -> ComputerUseTier {
        if !self.tools || self.computer_capability_source == CapabilitySource::Unknown {
            return ComputerUseTier::None;
        }
        if self.computer_use_tier == ComputerUseTier::VisualFallbackAct
            && (!self.image_input
                || self.image_media_types.is_empty()
                || self.max_image_bytes.is_none())
        {
            return ComputerUseTier::SemanticAct;
        }
        if self.computer_use_tier == ComputerUseTier::VisualFallbackAct
            && self.computer_capability_source != CapabilitySource::Measured
        {
            return ComputerUseTier::SemanticAct;
        }
        if self.computer_use_tier == ComputerUseTier::VisualFallbackAct
            && self.computer_qualification_schema.as_deref()
                != Some(ISOLATED_VISUAL_COMPUTER_QUALIFICATION_SCHEMA)
        {
            return ComputerUseTier::SemanticAct;
        }
        self.computer_use_tier
    }
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One exact model exposed by a provider profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_model_id: Option<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

impl ProviderModel {
    pub fn unqualified(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            id,
            wire_model_id: None,
            capabilities: ModelCapabilities::default(),
        }
    }

    pub fn wire_model_id(&self) -> &str {
        self.wire_model_id.as_deref().unwrap_or(&self.id)
    }
}

/// Named provider profile. `credential_ref` is an opaque trusted-host
/// reference such as `keychain:provider/corp/api-key`, never the credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: String,
    pub label: String,
    pub kind: ProviderKind,
    pub dialect: ProviderDialect,
    #[serde(default)]
    pub deadline_class: ProviderDeadlineClass,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    /// Runtime-only profile synthesized from paired endpoint/credential env vars.
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed_by_env: bool,
    /// Runtime-only profile synthesized and owned by the native host.
    #[serde(default, skip_serializing_if = "is_false")]
    pub managed_by_host: bool,
}

impl ProviderProfile {
    pub fn openai_compatible(
        id: impl Into<String>,
        label: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: ProviderKind::OpenAiCompatible,
            dialect: ProviderDialect::OpenAiChatCompletions,
            deadline_class: ProviderDeadlineClass::Standard,
            base_url: base_url.into(),
            credential_ref: None,
            models: Vec::new(),
            managed_by_env: false,
            managed_by_host: false,
        }
    }

    pub fn upsert_model(&mut self, mut model: ProviderModel) {
        model.display_name = model.display_name.trim().to_string();
        if model.display_name.is_empty() {
            model.display_name = model.id.clone();
        }
        if let Some(existing) = self.models.iter_mut().find(|item| item.id == model.id) {
            *existing = model;
        } else if !model.id.trim().is_empty() {
            self.models.push(model);
        }
        self.models.sort_by(|a, b| a.id.cmp(&b.id));
    }

    pub fn set_base_url(&mut self, base_url: &str) {
        let base_url = base_url.trim().trim_end_matches('/');
        if self.base_url.trim().trim_end_matches('/') != base_url {
            for model in &mut self.models {
                if model.capabilities.source == CapabilitySource::Measured {
                    let effort_options = model.capabilities.effort_options.clone();
                    model.capabilities = ModelCapabilities::default();
                    model.capabilities.effort_options = effort_options;
                } else if model.capabilities.computer_capability_source
                    == CapabilitySource::Measured
                {
                    model.capabilities.computer_use_tier = ComputerUseTier::None;
                    model.capabilities.computer_capability_source = CapabilitySource::Unknown;
                    model.capabilities.computer_qualification_schema = None;
                }
            }
        }
        self.base_url = base_url.to_string();
    }

    pub fn accepts_effort(&self, model_id: &str, effort: &str) -> bool {
        if self.dialect == ProviderDialect::XaiChatCompletions {
            return true;
        }
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .is_some_and(|model| {
                model
                    .capabilities
                    .effort_options
                    .iter()
                    .any(|value| value == effort)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedQualification {
    provider_id: String,
    model_id: String,
    base_url: String,
    wire_model_id: String,
    credential_ref: String,
    #[serde(default)]
    credential_fingerprint: String,
    capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedQualificationStore {
    version: u32,
    #[serde(default)]
    qualifications: Vec<ManagedQualification>,
}

impl Default for ManagedQualificationStore {
    fn default() -> Self {
        Self {
            version: MANAGED_QUALIFICATIONS_VERSION,
            qualifications: Vec::new(),
        }
    }
}

/// Versioned provider profile store.
///
/// The final three fields read the v1 single-gateway shape. They are omitted
/// from all serialization and must be cleared only after credential migration
/// succeeds and is verified.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile_id: Option<String>,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default, skip_serializing)]
    pub provider_id: String,
    #[serde(default, skip_serializing)]
    pub base_url: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
}

impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayConfig")
            .field("version", &self.version)
            .field("active_profile_id", &self.active_profile_id)
            .field("profiles", &self.profiles)
            .field("legacy_secret_pending", &self.has_pending_legacy_secret())
            .finish()
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            version: PROVIDER_CONFIG_VERSION,
            active_profile_id: None,
            profiles: Vec::new(),
            provider_id: String::new(),
            base_url: String::new(),
            api_key: String::new(),
        }
    }
}

impl GatewayConfig {
    fn normalize(&mut self) {
        self.version = PROVIDER_CONFIG_VERSION;
        if self.profiles.is_empty() && !self.base_url.trim().is_empty() {
            let id =
                normalized_profile_id(&self.provider_id).unwrap_or_else(|_| "corporate".into());
            let label = if self.provider_id.trim().is_empty() {
                "Corporate gateway".to_string()
            } else {
                self.provider_id.trim().to_string()
            };
            self.profiles.push(ProviderProfile::openai_compatible(
                id.clone(),
                label,
                self.base_url.trim(),
            ));
            self.active_profile_id = Some(id);
        }

        let mut seen = BTreeSet::new();
        self.profiles.retain_mut(|profile| {
            // Host-managed xAI routing is synthesized after config loading.
            // Persisted profiles may never opt into its credential or dialect.
            if !is_valid_stored_profile(profile) {
                return false;
            }
            let Ok(id) = normalized_profile_id(&profile.id) else {
                return false;
            };
            if !seen.insert(id.clone()) {
                return false;
            }
            profile.id = id;
            profile.label = profile.label.trim().to_string();
            if profile.label.is_empty() {
                profile.label = profile.id.clone();
            }
            profile.base_url = profile.base_url.trim().trim_end_matches('/').to_string();
            let mut model_ids = BTreeSet::new();
            profile.models.retain_mut(|model| {
                if model.id.trim().is_empty() || !model_ids.insert(model.id.clone()) {
                    return false;
                }
                model.display_name = model.display_name.trim().to_string();
                if model.display_name.is_empty() {
                    model.display_name = model.id.clone();
                }
                normalize_effort_options(&mut model.capabilities.effort_options);
                normalize_image_capabilities(&mut model.capabilities);
                if model.capabilities.source == CapabilitySource::Measured
                    && model.capabilities.qualification_schema.as_deref()
                        != Some(CAPABILITY_QUALIFICATION_SCHEMA)
                {
                    let effort_options = model.capabilities.effort_options.clone();
                    model.capabilities = ModelCapabilities::default();
                    model.capabilities.effort_options = effort_options;
                }
                if model.capabilities.computer_capability_source == CapabilitySource::Measured
                    && model.capabilities.computer_qualification_schema.as_deref()
                        != Some(COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA)
                {
                    model.capabilities.computer_use_tier = ComputerUseTier::None;
                    model.capabilities.computer_capability_source = CapabilitySource::Unknown;
                    model.capabilities.computer_qualification_schema = None;
                }
                true
            });
            profile.models.sort_by(|a, b| a.id.cmp(&b.id));
            true
        });

        if self
            .active_profile_id
            .as_ref()
            .is_some_and(|id| !self.profiles.iter().any(|profile| &profile.id == id))
        {
            self.active_profile_id = self.profiles.first().map(|profile| profile.id.clone());
        }
    }

    fn append_environment_profiles(&mut self) {
        self.profiles.retain(|profile| !profile.managed_by_env);
        let mut append = |id: &str, label: &str, base_variables: &[&str], key_variable: &str| {
            let base_url = base_variables.iter().find_map(|variable| {
                std::env::var(variable)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            });
            let Some(base_url) = base_url else {
                return;
            };
            if validate_base_url(&base_url).is_err() {
                return;
            }
            let mut profile = ProviderProfile::openai_compatible(id, label, base_url);
            profile.credential_ref = Some(format!("env:{key_variable}"));
            profile.managed_by_env = true;
            self.profiles.push(profile);
            if self.active_profile_id.is_none() {
                self.active_profile_id = Some(id.to_string());
            }
        };
        append(
            "env-grokptah",
            "GrokPtah environment gateway",
            &["GROKPTAH_API_BASE"],
            "GROKPTAH_API_KEY",
        );
        append(
            "env-openai",
            "OpenAI-compatible environment gateway",
            &["OPENAI_BASE_URL", "OPENAI_API_BASE"],
            "OPENAI_API_KEY",
        );
    }

    pub fn profile(&self, id: &str) -> Option<&ProviderProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn profile_mut(&mut self, id: &str) -> Option<&mut ProviderProfile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    pub fn upsert_profile(&mut self, mut profile: ProviderProfile) -> Result<(), String> {
        validate_stored_profile(&profile)?;
        profile.id = normalized_profile_id(&profile.id)?;
        validate_base_url(&profile.base_url)?;
        profile.base_url = profile.base_url.trim().trim_end_matches('/').to_string();
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
        self.active_profile_id = self
            .active_profile_id
            .clone()
            .or_else(|| self.profiles.first().map(|item| item.id.clone()));
        self.normalize();
        Ok(())
    }

    pub fn remove_profile(&mut self, id: &str) -> Option<ProviderProfile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        let removed = self.profiles.remove(index);
        if self.active_profile_id.as_deref() == Some(id) {
            self.active_profile_id = self.profiles.first().map(|profile| profile.id.clone());
        }
        Some(removed)
    }

    pub fn has_pending_legacy_secret(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn clear_legacy_fields(&mut self) {
        self.provider_id.clear();
        self.base_url.clear();
        self.api_key.clear();
    }
}

fn is_valid_stored_profile(profile: &ProviderProfile) -> bool {
    !profile.managed_by_host
        && profile.kind == ProviderKind::OpenAiCompatible
        && profile.dialect == ProviderDialect::OpenAiChatCompletions
}

fn validate_stored_profile(profile: &ProviderProfile) -> Result<(), String> {
    if !is_valid_stored_profile(profile) {
        return Err(
            "xAI provider kind and dialect are reserved for the synthesized host-managed `xai` profile"
                .into(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

pub fn model_selection_key(provider_id: &str, model_id: &str) -> String {
    format!(
        "{MODEL_SELECTION_PREFIX}{}{}",
        encode_component(provider_id.trim()),
        encode_component(model_id)
    )
}

/// Plain model ids remain the built-in xAI profile for backward compatibility.
pub fn parse_model_selection(value: &str) -> Result<ModelSelection, String> {
    let Some(rest) = value.strip_prefix(MODEL_SELECTION_PREFIX) else {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("model id is empty".into());
        }
        return Ok(ModelSelection {
            provider_id: XAI_PROVIDER_ID.into(),
            model_id: trimmed.into(),
        });
    };
    let (provider_id, rest) =
        decode_component(rest).ok_or_else(|| "invalid provider/model selection key".to_string())?;
    let (model_id, tail) =
        decode_component(rest).ok_or_else(|| "invalid provider/model selection key".to_string())?;
    if !tail.is_empty() || provider_id.is_empty() || model_id.is_empty() {
        return Err("invalid provider/model selection key".into());
    }
    Ok(ModelSelection {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
    })
}

/// Resolve one selection to the same profile-shaped runtime contract whether
/// the profile is host-managed or stored in `gateway.json`.
pub(crate) fn resolve_profile_for_selection(
    selection: &ModelSelection,
    xai_oidc_token_auth: bool,
    credential_fingerprint: Option<&str>,
) -> Result<ProviderProfile, String> {
    if selection.provider_id == XAI_PROVIDER_ID {
        return synthesized_xai_profile(
            &selection.model_id,
            xai_oidc_token_auth,
            credential_fingerprint,
        );
    }
    load()
        .profile(&selection.provider_id)
        .cloned()
        .ok_or_else(|| format!("unknown provider profile `{}`", selection.provider_id))
}

fn synthesized_xai_profile(
    model_id: &str,
    oidc_token_auth: bool,
    credential_fingerprint: Option<&str>,
) -> Result<ProviderProfile, String> {
    let catalog_entry = crate::models_catalog::lookup(model_id);
    let wire_model_id = catalog_entry
        .as_ref()
        .map(|entry| entry.wire_model.clone())
        .unwrap_or_else(|| model_id.to_string());
    let base_url = std::env::var("XAI_API_BASE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if oidc_token_auth {
                catalog_entry
                    .as_ref()
                    .and_then(|entry| entry.base_url.clone())
                    .filter(|url| url.contains("cli-chat-proxy") || url.contains("x.ai"))
                    .unwrap_or_else(|| "https://cli-chat-proxy.grok.com/v1".into())
            } else {
                catalog_entry
                    .as_ref()
                    .and_then(|entry| entry.base_url.clone())
                    .unwrap_or_else(|| "https://api.x.ai/v1".into())
            }
        });
    let effort_options = catalog_entry
        .as_ref()
        .map(|entry| entry.info.effort_options.clone())
        .unwrap_or_default();
    let credential_ref = if oidc_token_auth {
        "managed:xai:oidc"
    } else {
        "managed:xai:api-key"
    };
    let mut capabilities = ModelCapabilities {
        chat: true,
        tools: true,
        stream: true,
        parallel_tool_calls: true,
        effort_options,
        source: CapabilitySource::Declared,
        qualification_schema: None,
        ..ModelCapabilities::default()
    };
    let store = load_managed_qualifications()
        .map_err(|error| format!("read managed xAI qualifications: {error}"))?;
    if let Some(measured) = store.qualifications.iter().find(|record| {
        record.provider_id == XAI_PROVIDER_ID
            && record.model_id == model_id
            && record.base_url == base_url
            && record.wire_model_id == wire_model_id
            && record.credential_ref == credential_ref
            && credential_fingerprint
                .is_some_and(|fingerprint| record.credential_fingerprint == fingerprint)
            && record.capabilities.source == CapabilitySource::Measured
            && record.capabilities.qualification_schema.as_deref()
                == Some(CAPABILITY_QUALIFICATION_SCHEMA)
    }) {
        let effort_options = capabilities.effort_options;
        capabilities = measured.capabilities.clone();
        capabilities.effort_options = effort_options;
        if capabilities.computer_capability_source == CapabilitySource::Measured
            && capabilities.computer_qualification_schema.as_deref()
                != Some(COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA)
        {
            capabilities.computer_use_tier = ComputerUseTier::None;
            capabilities.computer_capability_source = CapabilitySource::Unknown;
            capabilities.computer_qualification_schema = None;
        }
    }
    let display_name = catalog_entry
        .as_ref()
        .map(|entry| entry.info.display_name.clone())
        .unwrap_or_else(|| model_id.to_string());
    Ok(ProviderProfile {
        id: XAI_PROVIDER_ID.into(),
        label: "xAI / Grok Build".into(),
        kind: ProviderKind::Xai,
        dialect: ProviderDialect::XaiChatCompletions,
        deadline_class: ProviderDeadlineClass::Standard,
        base_url,
        credential_ref: Some(credential_ref.into()),
        models: vec![ProviderModel {
            id: model_id.to_string(),
            display_name,
            wire_model_id: Some(wire_model_id),
            capabilities,
        }],
        managed_by_env: false,
        managed_by_host: true,
    })
}

pub(crate) fn save_managed_profile_capabilities(
    profile: &ProviderProfile,
    model: &ProviderModel,
    credential_fingerprint: &str,
) -> io::Result<()> {
    if profile.id != XAI_PROVIDER_ID
        || !profile.managed_by_host
        || profile.kind != ProviderKind::Xai
        || profile.dialect != ProviderDialect::XaiChatCompletions
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only the host-managed xAI profile uses managed qualification storage",
        ));
    }
    let _guard = managed_qualifications_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _file_guard = acquire_managed_qualifications_file_lock()?;
    let mut store = load_managed_qualifications()?;
    store.version = MANAGED_QUALIFICATIONS_VERSION;
    let replacement = ManagedQualification {
        provider_id: profile.id.clone(),
        model_id: model.id.clone(),
        base_url: profile.base_url.clone(),
        wire_model_id: model.wire_model_id().to_string(),
        credential_ref: profile.credential_ref.clone().unwrap_or_default(),
        credential_fingerprint: credential_fingerprint.to_string(),
        capabilities: model.capabilities.clone(),
    };
    store
        .qualifications
        .retain(|record| !record.has_same_route_key(&replacement));
    store.qualifications.push(replacement);
    store
        .qualifications
        .sort_by(|a, b| a.route_key().cmp(&b.route_key()));
    save_managed_qualifications(&store)
}

impl ManagedQualification {
    fn route_key(
        &self,
    ) -> (
        &str,
        &str,
        &str,
        &str,
        &str,
        &str,
        Option<&str>,
        Option<&str>,
    ) {
        (
            &self.provider_id,
            &self.model_id,
            &self.base_url,
            &self.wire_model_id,
            &self.credential_ref,
            &self.credential_fingerprint,
            self.capabilities.qualification_schema.as_deref(),
            self.capabilities.computer_qualification_schema.as_deref(),
        )
    }

    fn has_same_route_key(&self, other: &Self) -> bool {
        self.route_key() == other.route_key()
    }
}

fn managed_qualifications_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn managed_qualifications_path() -> PathBuf {
    grokptah_home().join("provider-qualifications.json")
}

fn acquire_managed_qualifications_file_lock() -> io::Result<fs::File> {
    let path = grokptah_home().join(".provider-qualifications.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    file.lock_exclusive()?;
    set_private_permissions(&path)?;
    Ok(file)
}

fn load_managed_qualifications() -> io::Result<ManagedQualificationStore> {
    let path = managed_qualifications_path();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ManagedQualificationStore::default());
        }
        Err(error) => return Err(error),
    };
    let store: ManagedQualificationStore = serde_json::from_str(&raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if store.version != MANAGED_QUALIFICATIONS_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported managed provider qualification version",
        ));
    }
    Ok(store)
}

/// True when the host has measured this xAI model before, regardless of the
/// route or credential identity that produced the measurement.
///
/// Admission uses this only as a downgrade fence: a new autonomous Execution
/// Run may start from declared capabilities before first qualification, but it
/// may not silently fall back to declared tool claims after a measured route
/// becomes stale. The operator must requalify the new exact route first.
pub(crate) fn has_measured_xai_qualification(
    provider_id: &str,
    model_id: &str,
) -> io::Result<bool> {
    if provider_id != XAI_PROVIDER_ID {
        return Ok(false);
    }
    Ok(load_managed_qualifications()?
        .qualifications
        .into_iter()
        .any(|record| {
            record.provider_id == provider_id
                && record.model_id == model_id
                && record.capabilities.source == CapabilitySource::Measured
                && record.capabilities.qualification_schema.as_deref()
                    == Some(CAPABILITY_QUALIFICATION_SCHEMA)
        }))
}

fn save_managed_qualifications(store: &ManagedQualificationStore) -> io::Result<()> {
    let path = managed_qualifications_path();
    if path.exists() {
        load_managed_qualifications()?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_vec_pretty(store)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("provider-qualifications.json");
    let temporary = loop {
        let candidate = path.with_file_name(format!(
            ".{file_name}.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        match write_private_new_file(&candidate, &raw) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                let _ = fs::remove_file(&candidate);
                return Err(error);
            }
        }
    };
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    set_private_permissions(&path)?;
    sync_parent_directory(&path)?;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn encode_component(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn decode_component(input: &str) -> Option<(&str, &str)> {
    let colon = input.find(':')?;
    let length: usize = input[..colon].parse().ok()?;
    let body = &input[colon + 1..];
    if body.len() < length || !body.is_char_boundary(length) {
        return None;
    }
    Some(body.split_at(length))
}

pub fn normalized_profile_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("provider id is empty".into());
    }
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(
            "provider id must be 1-64 ASCII letters, numbers, dots, dashes, or underscores".into(),
        );
    }
    if value == XAI_PROVIDER_ID {
        return Err("provider id `xai` is reserved for the built-in xAI profile".into());
    }
    Ok(value.to_string())
}

pub fn validate_base_url(value: &str) -> Result<(), String> {
    let value = value.trim();
    let parsed = reqwest::Url::parse(value).map_err(|_| "provider base URL is invalid")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("provider base URL must use http:// or https://".into());
    }
    if parsed.host().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("provider base URL must contain a host and no embedded credentials".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("provider base URL cannot contain a query or fragment".into());
    }
    if parsed.scheme() == "http" {
        let loopback = parsed.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if !loopback {
            return Err("plaintext HTTP provider URLs are allowed only on loopback".into());
        }
    }
    Ok(())
}

fn normalize_effort_options(options: &mut Vec<String>) {
    const ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    let accepted: BTreeSet<String> = options
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| ORDER.contains(&value.as_str()))
        .collect();
    *options = ORDER
        .iter()
        .filter(|value| accepted.contains(**value))
        .map(|value| (*value).to_string())
        .collect();
}

fn normalize_image_capabilities(capabilities: &mut ModelCapabilities) {
    capabilities.image_media_types = capabilities
        .image_media_types
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| ALLOWED_IMAGE_MEDIA_TYPES.contains(&value.as_str()))
        .collect();
    capabilities.image_media_types.sort();
    capabilities.image_media_types.dedup();
    let valid_bound = capabilities
        .max_image_bytes
        .is_some_and(|bytes| bytes > 0 && bytes <= MAX_QUALIFIED_IMAGE_BYTES);
    if !capabilities.image_input || capabilities.image_media_types.is_empty() || !valid_bound {
        capabilities.image_input = false;
        capabilities.image_media_types.clear();
        capabilities.max_image_bytes = None;
    }
}

fn path() -> PathBuf {
    grokptah_home().join("gateway.json")
}

pub fn load() -> GatewayConfig {
    let mut config = load_from_path(&path()).unwrap_or_default();
    config.append_environment_profiles();
    config
}

/// Read configuration for a mutation. An existing malformed file is never
/// treated as empty because doing so would silently overwrite recovery data.
pub fn load_for_update() -> io::Result<GatewayConfig> {
    let config_path = path();
    let mut config = match load_from_path(&config_path) {
        Ok(config) => config,
        Err(error) if error.kind() == io::ErrorKind::NotFound => GatewayConfig::default(),
        Err(error) => return Err(error),
    };
    config.append_environment_profiles();
    Ok(config)
}

fn load_from_path(path: &Path) -> io::Result<GatewayConfig> {
    let raw = fs::read_to_string(path)?;
    let mut config: GatewayConfig = serde_json::from_str(&raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    config.normalize();
    Ok(config)
}

pub fn save(config: &GatewayConfig) -> io::Result<()> {
    let config_path = path();
    if config_path.exists() {
        load_from_path(&config_path)?;
    }
    save_to_path(&config_path, config)
}

fn save_to_path(path: &Path, config: &GatewayConfig) -> io::Result<()> {
    if config.has_pending_legacy_secret() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refusing to persist a raw provider credential",
        ));
    }
    let mut normalized = config.clone();
    normalized
        .profiles
        .retain(|profile| !profile.managed_by_env && !profile.managed_by_host);
    if normalized
        .active_profile_id
        .as_ref()
        .is_some_and(|id| !normalized.profiles.iter().any(|profile| &profile.id == id))
    {
        normalized.active_profile_id = normalized
            .profiles
            .first()
            .map(|profile| profile.id.clone());
    }
    normalized.normalize();
    normalized.clear_legacy_fields();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_vec_pretty(&normalized)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("json.tmp");
    write_private_file(&temporary, &raw)?;
    fs::rename(&temporary, path)?;
    set_private_permissions(path)?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_private_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn set_private_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};

    const TEST_CREDENTIAL_FINGERPRINT: &str = "v1-sha256:test-credential";

    #[test]
    fn legacy_shape_loads_as_profile_but_raw_secret_cannot_be_saved() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("gateway.json"),
            r#"{"provider_id":"corp","base_url":"https://gw.example/v1","api_key":"secret"}"#,
        )
        .unwrap();
        set_grokptah_home_override(Some(home));

        let config = load();
        assert_eq!(
            config.profile("corp").unwrap().base_url,
            "https://gw.example/v1"
        );
        assert!(config.has_pending_legacy_secret());
        let error = save(&config).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        set_grokptah_home_override(None);
    }

    #[test]
    fn profile_config_roundtrip_contains_references_but_no_secret_field() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home.clone()));

        let mut config = GatewayConfig::default();
        let mut profile =
            ProviderProfile::openai_compatible("corp", "Corporate", "https://gw.example/v1");
        profile.credential_ref = Some("keychain:provider/corp/api-key".into());
        profile.upsert_model(ProviderModel::unqualified("cheap/code-model"));
        config.upsert_profile(profile).unwrap();
        save(&config).unwrap();

        let raw = fs::read_to_string(home.join("gateway.json")).unwrap();
        assert!(raw.contains("keychain:provider/corp/api-key"));
        assert!(!raw.contains("api_key"));
        assert_eq!(
            load().profile("corp").unwrap().models[0].id,
            "cheap/code-model"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(home.join("gateway.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        set_grokptah_home_override(None);
    }

    #[test]
    fn provider_model_selection_is_collision_safe_and_plain_ids_stay_xai() {
        let encoded = model_selection_key("corp", "team:model/alpha");
        assert_eq!(
            parse_model_selection(&encoded).unwrap(),
            ModelSelection {
                provider_id: "corp".into(),
                model_id: "team:model/alpha".into()
            }
        );
        assert_eq!(
            parse_model_selection("grok-4.5").unwrap(),
            ModelSelection {
                provider_id: XAI_PROVIDER_ID.into(),
                model_id: "grok-4.5".into()
            }
        );
        assert_ne!(
            model_selection_key("ab", "c"),
            model_selection_key("a", "bc")
        );

        let opaque = model_selection_key("corp", " Team/Code:Cheap ");
        assert_eq!(
            parse_model_selection(&opaque).unwrap().model_id,
            " Team/Code:Cheap "
        );
    }

    #[test]
    fn opaque_model_ids_preserve_bytes_through_profile_persistence() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));

        let opaque_id = " Team/Code:Cheap/日本語 ";
        let mut profile =
            ProviderProfile::openai_compatible("corp", "Corporate", "https://gw.example/v1");
        profile.upsert_model(ProviderModel::unqualified(opaque_id));
        let mut config = GatewayConfig::default();
        config.upsert_profile(profile).unwrap();
        save(&config).unwrap();

        let loaded = load_for_update().unwrap();
        assert_eq!(loaded.profile("corp").unwrap().models[0].id, opaque_id);
        assert_eq!(
            parse_model_selection(&model_selection_key("corp", opaque_id))
                .unwrap()
                .model_id,
            opaque_id
        );

        set_grokptah_home_override(None);
    }

    #[test]
    fn rejects_reserved_or_unsafe_profile_ids_and_credential_urls() {
        assert!(normalized_profile_id("xai").is_err());
        assert!(normalized_profile_id("../../corp").is_err());
        assert!(validate_base_url("file:///tmp/key").is_err());
        assert!(validate_base_url("https://user:secret@example.com/v1").is_err());
        assert!(validate_base_url("https://gateway.example/v1").is_ok());
        assert!(validate_base_url("http://gateway.example/v1").is_err());
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
        assert!(validate_base_url("https://gateway.example/v1?token=value").is_err());
    }

    #[test]
    fn persisted_and_upserted_xai_impersonators_are_rejected() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home.clone()));

        fs::write(
            home.join("gateway.json"),
            r#"{
                "version": 2,
                "active_profile_id": "attacker",
                "profiles": [{
                    "id": "attacker",
                    "label": "Attacker",
                    "kind": "xai",
                    "dialect": "xai_chat_completions",
                    "base_url": "https://attacker.example/v1",
                    "models": [{"id": "grok-4.5"}]
                }]
            }"#,
        )
        .unwrap();
        let loaded = load_for_update().unwrap();
        assert!(loaded.profile("attacker").is_none());
        assert!(loaded.active_profile_id.is_none());

        let mut forged_kind =
            ProviderProfile::openai_compatible("corp", "Corp", "https://corp.example/v1");
        forged_kind.kind = ProviderKind::Xai;
        assert!(GatewayConfig::default()
            .upsert_profile(forged_kind)
            .is_err());

        let mut forged_dialect =
            ProviderProfile::openai_compatible("corp", "Corp", "https://corp.example/v1");
        forged_dialect.dialect = ProviderDialect::XaiChatCompletions;
        assert!(GatewayConfig::default()
            .upsert_profile(forged_dialect)
            .is_err());

        let mut forged_host =
            ProviderProfile::openai_compatible("corp", "Corp", "https://corp.example/v1");
        forged_host.managed_by_host = true;
        assert!(GatewayConfig::default()
            .upsert_profile(forged_host)
            .is_err());

        set_grokptah_home_override(None);
    }

    fn measured_managed_profile(
        model_id: &str,
        base_url: &str,
        credential_ref: &str,
        chat: bool,
        tools: bool,
    ) -> (ProviderProfile, ProviderModel) {
        let mut capabilities = ModelCapabilities {
            chat,
            tools,
            stream: tools,
            source: CapabilitySource::Measured,
            qualification_schema: Some(CAPABILITY_QUALIFICATION_SCHEMA.into()),
            ..ModelCapabilities::default()
        };
        capabilities.computer_capability_source = CapabilitySource::Measured;
        capabilities.computer_qualification_schema =
            Some(COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA.into());
        let model = ProviderModel {
            id: model_id.into(),
            display_name: model_id.into(),
            wire_model_id: Some(model_id.into()),
            capabilities,
        };
        let profile = ProviderProfile {
            id: XAI_PROVIDER_ID.into(),
            label: "xAI".into(),
            kind: ProviderKind::Xai,
            dialect: ProviderDialect::XaiChatCompletions,
            deadline_class: ProviderDeadlineClass::Standard,
            base_url: base_url.into(),
            credential_ref: Some(credential_ref.into()),
            models: vec![model.clone()],
            managed_by_env: false,
            managed_by_host: true,
        };
        (profile, model)
    }

    #[test]
    fn managed_qualification_replaces_only_the_exact_route() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        set_grokptah_home_override(Some(home));
        let previous_base = std::env::var_os("XAI_API_BASE");

        let (api_profile, failed_api) = measured_managed_profile(
            "grok-route",
            "https://api.x.ai/v1",
            "managed:xai:api-key",
            false,
            false,
        );
        save_managed_profile_capabilities(&api_profile, &failed_api, TEST_CREDENTIAL_FINGERPRINT)
            .unwrap();
        let (oidc_profile, oidc) = measured_managed_profile(
            "grok-route",
            "https://cli-chat-proxy.grok.com/v1",
            "managed:xai:oidc",
            true,
            true,
        );
        save_managed_profile_capabilities(&oidc_profile, &oidc, TEST_CREDENTIAL_FINGERPRINT)
            .unwrap();
        let (alternate_profile, alternate) = measured_managed_profile(
            "grok-route",
            "https://alternate.x.ai/v1",
            "managed:xai:api-key",
            true,
            true,
        );
        save_managed_profile_capabilities(
            &alternate_profile,
            &alternate,
            TEST_CREDENTIAL_FINGERPRINT,
        )
        .unwrap();

        let mut updated_oidc = oidc.clone();
        updated_oidc.capabilities.tools = false;
        save_managed_profile_capabilities(
            &oidc_profile,
            &updated_oidc,
            TEST_CREDENTIAL_FINGERPRINT,
        )
        .unwrap();

        let store = load_managed_qualifications().unwrap();
        assert_eq!(store.qualifications.len(), 3);
        assert!(has_measured_xai_qualification(XAI_PROVIDER_ID, "grok-route").unwrap());
        assert!(!has_measured_xai_qualification(XAI_PROVIDER_ID, "never-measured").unwrap());
        assert!(!has_measured_xai_qualification("compatible-provider", "grok-route").unwrap());
        let failed = store
            .qualifications
            .iter()
            .find(|record| {
                record.credential_ref == "managed:xai:api-key"
                    && record.base_url == "https://api.x.ai/v1"
            })
            .unwrap();
        assert!(!failed.capabilities.chat);
        assert!(!failed.capabilities.tools);
        let oidc_record = store
            .qualifications
            .iter()
            .find(|record| record.credential_ref == "managed:xai:oidc")
            .unwrap();
        assert!(!oidc_record.capabilities.tools);
        assert!(store
            .qualifications
            .iter()
            .any(|record| record.base_url == "https://alternate.x.ai/v1"
                && record.capabilities.tools));

        unsafe { std::env::set_var("XAI_API_BASE", "https://api.x.ai/v1") };
        let failed_projection = resolve_profile_for_selection(
            &ModelSelection {
                provider_id: XAI_PROVIDER_ID.into(),
                model_id: "grok-route".into(),
            },
            false,
            Some(TEST_CREDENTIAL_FINGERPRINT),
        )
        .unwrap();
        assert_eq!(
            failed_projection.models[0].capabilities.source,
            CapabilitySource::Measured
        );
        assert!(!failed_projection.models[0].capabilities.chat);
        assert!(!failed_projection.models[0].capabilities.tools);
        let rotated_credential_projection = resolve_profile_for_selection(
            &ModelSelection {
                provider_id: XAI_PROVIDER_ID.into(),
                model_id: "grok-route".into(),
            },
            false,
            Some("v1-sha256:rotated-credential"),
        )
        .unwrap();
        assert_eq!(
            rotated_credential_projection.models[0].capabilities.source,
            CapabilitySource::Declared,
            "measured authority must not cross credential identities"
        );

        unsafe {
            if let Some(value) = previous_base {
                std::env::set_var("XAI_API_BASE", value);
            } else {
                std::env::remove_var("XAI_API_BASE");
            }
        }

        set_grokptah_home_override(None);
    }

    #[test]
    fn concurrent_managed_qualification_writes_preserve_every_route() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        set_grokptah_home_override(Some(home.clone()));

        let threads: Vec<_> = (0..16)
            .map(|index| {
                std::thread::spawn(move || {
                    let (profile, model) = measured_managed_profile(
                        &format!("grok-{index}"),
                        &format!("https://route-{index}.x.ai/v1"),
                        if index % 2 == 0 {
                            "managed:xai:api-key"
                        } else {
                            "managed:xai:oidc"
                        },
                        true,
                        index % 3 != 0,
                    );
                    save_managed_profile_capabilities(
                        &profile,
                        &model,
                        TEST_CREDENTIAL_FINGERPRINT,
                    )
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }

        let store = load_managed_qualifications().unwrap();
        assert_eq!(store.qualifications.len(), 16);
        for index in 0..16 {
            assert!(store
                .qualifications
                .iter()
                .any(|record| record.model_id == format!("grok-{index}")));
        }
        let temporary_files = fs::read_dir(&home)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporary_files, 0);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(home.join("provider-qualifications.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        set_grokptah_home_override(None);
    }

    #[test]
    fn managed_qualification_lock_is_enforced_by_the_filesystem() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        set_grokptah_home_override(Some(home.clone()));

        let first = acquire_managed_qualifications_file_lock().unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(home.join(".provider-qualifications.lock"))
            .unwrap();
        assert!(FileExt::try_lock_exclusive(&second).is_err());
        drop(first);
        FileExt::try_lock_exclusive(&second).unwrap();
        FileExt::unlock(&second).unwrap();

        set_grokptah_home_override(None);
    }

    #[test]
    fn corrupt_managed_qualification_store_is_never_overwritten() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        let path = home.join("provider-qualifications.json");
        fs::write(&path, "{recovery-data").unwrap();
        set_grokptah_home_override(Some(home));

        let (profile, model) = measured_managed_profile(
            "grok-4.5",
            "https://api.x.ai/v1",
            "managed:xai:api-key",
            true,
            true,
        );
        assert_eq!(
            save_managed_profile_capabilities(&profile, &model, TEST_CREDENTIAL_FINGERPRINT,)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let resolution = resolve_profile_for_selection(
            &ModelSelection {
                provider_id: XAI_PROVIDER_ID.into(),
                model_id: "grok-4.5".into(),
            },
            false,
            Some(TEST_CREDENTIAL_FINGERPRINT),
        );
        assert!(resolution
            .unwrap_err()
            .contains("read managed xAI qualifications"));
        assert_eq!(fs::read_to_string(path).unwrap(), "{recovery-data");

        set_grokptah_home_override(None);
    }

    #[test]
    fn malformed_existing_config_is_never_overwritten() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        fs::create_dir_all(&home).unwrap();
        let config_path = home.join("gateway.json");
        fs::write(&config_path, "{malformed recovery data").unwrap();
        set_grokptah_home_override(Some(home));

        assert_eq!(
            load_for_update().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            save(&GatewayConfig::default()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fs::read_to_string(config_path).unwrap(),
            "{malformed recovery data"
        );

        set_grokptah_home_override(None);
    }

    #[test]
    fn measured_capabilities_invalidate_on_endpoint_or_schema_change() {
        let mut profile =
            ProviderProfile::openai_compatible("corp", "Corp", "https://one.example/v1");
        let mut model = ProviderModel::unqualified("code");
        model.capabilities.tools = true;
        model.capabilities.stream = true;
        model.capabilities.source = CapabilitySource::Measured;
        model.capabilities.qualification_schema = Some(CAPABILITY_QUALIFICATION_SCHEMA.into());
        model.capabilities.computer_use_tier = ComputerUseTier::SemanticAct;
        model.capabilities.computer_capability_source = CapabilitySource::Measured;
        model.capabilities.computer_qualification_schema =
            Some(COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA.into());
        model.capabilities.effort_options = vec!["low".into()];
        profile.upsert_model(model);

        profile.set_base_url("https://two.example/v1");
        assert!(!profile.models[0].capabilities.tools);
        assert_eq!(
            profile.models[0].capabilities.source,
            CapabilitySource::Unknown
        );
        assert_eq!(profile.models[0].capabilities.effort_options, vec!["low"]);
        assert_eq!(
            profile.models[0].capabilities.computer_use_tier,
            ComputerUseTier::None
        );

        profile.models[0].capabilities.tools = true;
        profile.models[0].capabilities.source = CapabilitySource::Measured;
        profile.models[0].capabilities.qualification_schema = Some("old-schema".into());
        let mut config = GatewayConfig::default();
        config.profiles.push(profile);
        config.normalize();
        assert!(!config.profiles[0].models[0].capabilities.tools);
        assert_eq!(
            config.profiles[0].models[0].capabilities.source,
            CapabilitySource::Unknown
        );
    }

    #[test]
    fn unknown_models_have_no_computer_authority() {
        let model = ProviderModel::unqualified("unknown-model");
        assert_eq!(model.capabilities.computer_use_tier, ComputerUseTier::None);
        assert!(!model.capabilities.computer_use_tier.allows_observation());
        assert!(!model.capabilities.image_input);
    }

    #[test]
    fn stale_computer_qualification_never_retains_action_authority() {
        let mut model = ProviderModel::unqualified("qualified-on-old-schema");
        model.capabilities.source = CapabilitySource::Declared;
        model.capabilities.tools = true;
        model.capabilities.computer_use_tier = ComputerUseTier::SemanticAct;
        model.capabilities.computer_capability_source = CapabilitySource::Measured;
        model.capabilities.computer_qualification_schema = Some("old-computer-schema".into());
        let mut profile =
            ProviderProfile::openai_compatible("corp", "Corp", "https://gw.example/v1");
        profile.upsert_model(model);
        let mut config = GatewayConfig::default();
        config.profiles.push(profile);

        config.normalize();

        let capabilities = &config.profiles[0].models[0].capabilities;
        assert!(capabilities.tools, "coding declaration remains independent");
        assert_eq!(capabilities.computer_use_tier, ComputerUseTier::None);
        assert_eq!(
            capabilities.computer_capability_source,
            CapabilitySource::Unknown
        );
    }

    #[test]
    fn effective_tier_fails_closed_for_incoherent_profile_claims() {
        let mut capabilities = ModelCapabilities {
            computer_use_tier: ComputerUseTier::VisualFallbackAct,
            ..ModelCapabilities::default()
        };
        assert_eq!(
            capabilities.effective_computer_use_tier(),
            ComputerUseTier::None
        );
        capabilities.tools = true;
        capabilities.computer_capability_source = CapabilitySource::Declared;
        assert_eq!(
            capabilities.effective_computer_use_tier(),
            ComputerUseTier::SemanticAct,
            "visual fallback requires explicit image input support"
        );
        capabilities.image_input = true;
        capabilities.image_media_types = vec!["image/png".into()];
        capabilities.max_image_bytes = Some(1024);
        assert_eq!(
            capabilities.effective_computer_use_tier(),
            ComputerUseTier::SemanticAct,
            "visual fallback requires isolated-runtime qualification evidence"
        );
        capabilities.computer_capability_source = CapabilitySource::Measured;
        assert_eq!(
            capabilities.effective_computer_use_tier(),
            ComputerUseTier::SemanticAct,
            "measured provenance alone does not grant visual fallback"
        );
        capabilities.computer_qualification_schema =
            Some(ISOLATED_VISUAL_COMPUTER_QUALIFICATION_SCHEMA.into());
        assert_eq!(
            capabilities.effective_computer_use_tier(),
            ComputerUseTier::VisualFallbackAct
        );
    }

    #[test]
    fn image_capabilities_are_bounded_and_allowlisted() {
        let mut capabilities = ModelCapabilities {
            image_input: true,
            image_media_types: vec![
                " IMAGE/PNG ".into(),
                "image/svg+xml".into(),
                "image/png".into(),
            ],
            max_image_bytes: Some(MAX_QUALIFIED_IMAGE_BYTES),
            ..ModelCapabilities::default()
        };
        normalize_image_capabilities(&mut capabilities);
        assert!(capabilities.image_input);
        assert_eq!(capabilities.image_media_types, vec!["image/png"]);

        capabilities.max_image_bytes = Some(MAX_QUALIFIED_IMAGE_BYTES + 1);
        normalize_image_capabilities(&mut capabilities);
        assert!(!capabilities.image_input);
        assert!(capabilities.image_media_types.is_empty());
        assert!(capabilities.max_image_bytes.is_none());
    }

    #[test]
    fn effort_options_are_filtered_and_ordered() {
        let mut options = vec!["HIGH".into(), "bogus".into(), "low".into()];
        normalize_effort_options(&mut options);
        assert_eq!(options, vec!["low", "high"]);
    }

    #[test]
    fn environment_gateway_pairs_base_with_matching_key_reference() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(temp.path().join(".grokptah")));
        unsafe {
            std::env::set_var("GROKPTAH_API_BASE", "https://corp.example/v1");
            std::env::set_var("OPENAI_BASE_URL", "https://openai.example/v1");
        }

        let config = load();
        let grokptah = config.profile("env-grokptah").unwrap();
        assert_eq!(grokptah.base_url, "https://corp.example/v1");
        assert_eq!(
            grokptah.credential_ref.as_deref(),
            Some("env:GROKPTAH_API_KEY")
        );
        let openai = config.profile("env-openai").unwrap();
        assert_eq!(openai.base_url, "https://openai.example/v1");
        assert_eq!(openai.credential_ref.as_deref(), Some("env:OPENAI_API_KEY"));

        unsafe {
            std::env::remove_var("GROKPTAH_API_BASE");
            std::env::remove_var("OPENAI_BASE_URL");
        }
        set_grokptah_home_override(None);
    }

    #[test]
    fn synthesized_xai_profile_preserves_catalog_routing_for_every_entry() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(temp.path().join(".grokptah")));
        let previous_override = std::env::var_os("XAI_API_BASE");
        unsafe { std::env::remove_var("XAI_API_BASE") };

        for entry in crate::models_catalog::load_catalog() {
            for oidc_token_auth in [false, true] {
                let selection = ModelSelection {
                    provider_id: XAI_PROVIDER_ID.into(),
                    model_id: entry.info.id.clone(),
                };
                let profile =
                    resolve_profile_for_selection(&selection, oidc_token_auth, None).unwrap();
                let model = &profile.models[0];
                let expected_base = if oidc_token_auth {
                    entry
                        .base_url
                        .clone()
                        .filter(|url| url.contains("cli-chat-proxy") || url.contains("x.ai"))
                        .unwrap_or_else(|| "https://cli-chat-proxy.grok.com/v1".into())
                } else {
                    entry
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.x.ai/v1".into())
                };
                assert_eq!(profile.id, XAI_PROVIDER_ID);
                assert!(profile.managed_by_host);
                assert_eq!(profile.kind, ProviderKind::Xai);
                assert_eq!(profile.dialect, ProviderDialect::XaiChatCompletions);
                assert_eq!(profile.deadline_class, ProviderDeadlineClass::Standard);
                assert_eq!(profile.base_url.as_bytes(), expected_base.as_bytes());
                assert_eq!(model.id.as_bytes(), entry.info.id.as_bytes());
                assert_eq!(
                    model.wire_model_id().as_bytes(),
                    entry.wire_model.as_bytes()
                );
                assert_eq!(
                    model.capabilities.effort_options.as_slice(),
                    entry.info.effort_options.as_slice()
                );
                assert!(model.capabilities.chat);
                assert!(model.capabilities.tools);
                assert!(model.capabilities.stream);
                assert!(model.capabilities.parallel_tool_calls);
                assert_eq!(model.capabilities.source, CapabilitySource::Declared);
                assert!(profile.accepts_effort(&entry.info.id, "max"));
            }
        }

        unsafe {
            if let Some(value) = previous_override {
                std::env::set_var("XAI_API_BASE", value);
            } else {
                std::env::remove_var("XAI_API_BASE");
            }
        }
        set_grokptah_home_override(None);
    }

    #[test]
    fn xai_override_and_unknown_model_fallback_preserve_legacy_bytes() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(temp.path().join(".grokptah")));
        let previous_override = std::env::var_os("XAI_API_BASE");
        unsafe { std::env::set_var("XAI_API_BASE", "  http://127.0.0.1:9876/custom/  ") };
        let selection = ModelSelection {
            provider_id: XAI_PROVIDER_ID.into(),
            model_id: "unknown/model:opaque".into(),
        };

        for oidc_token_auth in [false, true] {
            let profile = resolve_profile_for_selection(&selection, oidc_token_auth, None).unwrap();
            let model = &profile.models[0];
            assert_eq!(profile.base_url, "http://127.0.0.1:9876/custom/");
            assert_eq!(model.id, "unknown/model:opaque");
            assert_eq!(model.wire_model_id(), "unknown/model:opaque");
            assert!(model.capabilities.effort_options.is_empty());
            assert_eq!(model.capabilities.source, CapabilitySource::Declared);
        }

        unsafe {
            if let Some(value) = previous_override {
                std::env::set_var("XAI_API_BASE", value);
            } else {
                std::env::remove_var("XAI_API_BASE");
            }
        }
        set_grokptah_home_override(None);
    }

    #[test]
    fn xai_id_cannot_be_created_renamed_mutated_or_persisted() {
        let _lock = home_override_serial();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".grokptah");
        set_grokptah_home_override(Some(home.clone()));

        let create = ProviderProfile::openai_compatible(
            XAI_PROVIDER_ID,
            "user xAI",
            "https://example.com/v1",
        );
        let mut config = GatewayConfig::default();
        assert!(config
            .upsert_profile(create)
            .unwrap_err()
            .contains("reserved"));

        let mut renamed =
            ProviderProfile::openai_compatible("corp", "Corp", "https://example.com/v1");
        renamed.id = XAI_PROVIDER_ID.into();
        assert!(config
            .upsert_profile(renamed)
            .unwrap_err()
            .contains("reserved"));

        let selection = ModelSelection {
            provider_id: XAI_PROVIDER_ID.into(),
            model_id: "grok-4.5".into(),
        };
        let mut managed = resolve_profile_for_selection(&selection, false, None).unwrap();
        managed.label = "mutated".into();
        assert!(config
            .upsert_profile(managed.clone())
            .unwrap_err()
            .contains("reserved"));

        config.profiles.push(managed);
        save(&config).unwrap();
        let persisted = fs::read_to_string(home.join("gateway.json")).unwrap();
        assert!(!persisted.contains("mutated"));
        assert!(load().profile(XAI_PROVIDER_ID).is_none());

        set_grokptah_home_override(None);
    }
}
