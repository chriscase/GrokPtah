//! Provider-scoped gateway profiles (#278).
//!
//! Config contains endpoint/model metadata and credential references only. The
//! legacy top-level `api_key` remains readable for one safe migration, but new
//! writes reject it so a bearer cannot be persisted back to disk.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;

pub const PROVIDER_CONFIG_VERSION: u32 = 2;
pub const XAI_PROVIDER_ID: &str = "xai";
pub const MODEL_SELECTION_PREFIX: &str = "ptah.model.v1:";
pub const CAPABILITY_QUALIFICATION_SCHEMA: &str = "grokptah.provider-qualification.v1";
pub const COMPUTER_CAPABILITY_QUALIFICATION_SCHEMA: &str = "grokptah.computer-qualification.v1";
pub const MAX_QUALIFIED_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
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
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

impl ProviderModel {
    pub fn unqualified(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            id,
            capabilities: ModelCapabilities::default(),
        }
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
        .retain(|profile| !profile.managed_by_env);
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
}
