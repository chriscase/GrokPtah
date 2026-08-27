//! Explicit startup configuration.
//!
//! Configuration is a file plus a small set of environment overrides. There is
//! no implicit discovery, no ambient credential, and no defaulted authority:
//! an operation the configuration did not name is refused. Unknown keys are
//! rejected so a typo cannot silently widen or narrow the host.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use grokptah_agent_sdk::run::{MAX_EVENT_UPDATE_BYTES, MAX_ROUNDS};
use grokptah_agent_sdk::{CapabilityAvailability, CapabilitySet};
use serde::{Deserialize, Serialize};

use crate::error::{HostError, HostResult, io_error};

/// Environment override for the host home directory.
pub const ENV_HOME: &str = "GROKPTAH_HEADLESS_HOME";
/// Environment override for the approved workspace.
pub const ENV_WORKSPACE: &str = "GROKPTAH_HEADLESS_WORKSPACE";
/// Environment override for the host session identity.
pub const ENV_SESSION_ID: &str = "GROKPTAH_HEADLESS_SESSION_ID";

/// Directory name owned by the desktop authority.
const DESKTOP_HOME_DIR: &str = ".grokptah";
/// Maximum bytes accepted in a configuration file.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// Which run engine the host is allowed to drive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineSelection {
    /// No engine is wired. Observation works; every submit fails closed.
    #[default]
    Disabled,
    /// Deterministic offline fixture engine driven by a scripted file.
    Fixture {
        /// Path to the JSON script of scripted outcomes.
        script: PathBuf,
    },
}

/// Bounded host ceilings. The authority can only narrow a caller's request
/// against these; it never widens one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostLimits {
    /// Runs allowed to execute concurrently.
    pub max_active_runs: u32,
    /// Runs allowed to wait for bounded admission.
    pub max_queued_runs: u32,
    /// Maximum accepted prompt bytes.
    pub max_prompt_bytes: u32,
    /// Maximum model rounds for one run.
    pub max_rounds: u16,
    /// Maximum wall-clock duration for one run.
    pub max_duration_ms: u64,
    /// Events retained per run for cursor replay.
    pub event_retention: u32,
    /// Maximum serialized bytes in one journaled event update.
    pub max_event_bytes: u32,
    /// Default control lease lifetime.
    pub lease_ttl_ms: u64,
    /// How long an unresolved escalation waits before it is denied.
    pub attention_ttl_ms: u64,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            max_active_runs: 1,
            max_queued_runs: 4,
            max_prompt_bytes: 32 * 1024,
            max_rounds: 8,
            max_duration_ms: 15 * 60 * 1_000,
            event_retention: 256,
            max_event_bytes: 64 * 1024,
            lease_ttl_ms: 120_000,
            attention_ttl_ms: 15 * 60 * 1_000,
        }
    }
}

impl HostLimits {
    fn validate(&self) -> HostResult<()> {
        let positive = [
            ("maxActiveRuns", u64::from(self.max_active_runs)),
            ("maxQueuedRuns", u64::from(self.max_queued_runs)),
            ("maxPromptBytes", u64::from(self.max_prompt_bytes)),
            ("maxRounds", u64::from(self.max_rounds)),
            ("maxDurationMs", self.max_duration_ms),
            ("eventRetention", u64::from(self.event_retention)),
            ("maxEventBytes", u64::from(self.max_event_bytes)),
            ("leaseTtlMs", self.lease_ttl_ms),
            ("attentionTtlMs", self.attention_ttl_ms),
        ];
        for (name, value) in positive {
            if value == 0 {
                return Err(HostError::invalid(
                    "limit_not_positive",
                    format!("{name} must be greater than zero"),
                ));
            }
        }
        if self.max_rounds > MAX_ROUNDS {
            return Err(HostError::invalid(
                "limit_exceeds_contract",
                format!("maxRounds must not exceed the public ceiling of {MAX_ROUNDS}"),
            ));
        }
        if self.max_event_bytes as usize > MAX_EVENT_UPDATE_BYTES {
            return Err(HostError::invalid(
                "limit_exceeds_contract",
                "maxEventBytes must not exceed the public event bound",
            ));
        }
        Ok(())
    }
}

/// Validated startup configuration for one headless host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostConfig {
    /// Directory the host owns exclusively. Never the desktop home.
    pub home: PathBuf,
    /// The single approved workspace for this host.
    pub workspace: PathBuf,
    /// Session identity every run is fenced to.
    pub session_id: String,
    /// Capability set this host is permitted to honor.
    pub capabilities: CapabilitySet,
    /// Explicit operator grants for gated capabilities. Absent means denied.
    #[serde(default)]
    pub grants: Vec<String>,
    /// Bounded ceilings applied to every admitted run.
    #[serde(default)]
    pub limits: HostLimits,
    /// Which engine the host may drive.
    #[serde(default)]
    pub engine: EngineSelection,
}

impl HostConfig {
    /// Read and validate a configuration file.
    pub fn load(path: &Path) -> HostResult<Self> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| io_error("config_unreadable", &error).with_request_id("config"))?;
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(HostError::invalid(
                "config_too_large",
                "configuration exceeds its byte bound",
            ));
        }
        let raw =
            std::fs::read_to_string(path).map_err(|error| io_error("config_unreadable", &error))?;
        Self::parse(&raw)
    }

    /// Parse and validate configuration from its serialized form.
    pub fn parse(raw: &str) -> HostResult<Self> {
        let config: Self = serde_json::from_str(raw).map_err(|error| {
            HostError::invalid(
                "config_malformed",
                format!("configuration is not valid ({})", error.classify_label()),
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Apply environment overrides through an injected reader.
    ///
    /// The reader is injected so a test can exercise precedence without
    /// mutating process environment shared with other tests.
    pub fn apply_overrides<F>(&mut self, read: F) -> HostResult<()>
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(home) = read(ENV_HOME) {
            self.home = PathBuf::from(home);
        }
        if let Some(workspace) = read(ENV_WORKSPACE) {
            self.workspace = PathBuf::from(workspace);
        }
        if let Some(session_id) = read(ENV_SESSION_ID) {
            self.session_id = session_id;
        }
        self.validate()
    }

    /// Reject any configuration that is ambiguous, unbounded, or would make the
    /// host a second writer to the desktop home.
    pub fn validate(&self) -> HostResult<()> {
        let home = utf8_path(&self.home, "home")?;
        let workspace = utf8_path(&self.workspace, "workspace")?;

        if !self.home.is_absolute() {
            return Err(HostError::invalid(
                "home_not_absolute",
                "home must be an absolute path",
            ));
        }
        if !self.workspace.is_absolute() {
            return Err(HostError::invalid(
                "workspace_not_absolute",
                "workspace must be an absolute path",
            ));
        }
        if self
            .home
            .file_name()
            .is_some_and(|name| name == DESKTOP_HOME_DIR)
        {
            return Err(HostError::invalid(
                "desktop_home_refused",
                "home must not be the desktop authority home",
            ));
        }
        if home == workspace
            || self.home.starts_with(&self.workspace)
            || self.workspace.starts_with(&self.home)
        {
            return Err(HostError::invalid(
                "home_workspace_overlap",
                "home and workspace must be distinct, non-nested roots",
            ));
        }

        validate_session_id(&self.session_id)?;

        if !self.capabilities.is_current() {
            return Err(HostError::invalid(
                "capability_contract_rejected",
                "capability set is not the current well-formed contract",
            ));
        }

        let mut seen = BTreeSet::new();
        for grant in &self.grants {
            let descriptor = self.capabilities.get(grant).ok_or_else(|| {
                HostError::invalid(
                    "grant_unknown_capability",
                    "a grant names a capability the host does not advertise",
                )
            })?;
            if descriptor.availability != CapabilityAvailability::Gated {
                return Err(HostError::invalid(
                    "grant_not_gated",
                    "a grant names a capability that is not gated",
                ));
            }
            if !seen.insert(grant.as_str()) {
                return Err(HostError::invalid(
                    "grant_duplicated",
                    "grants must be unique",
                ));
            }
        }

        self.limits.validate()
    }

    /// The host home as UTF-8. Validated configuration always has one.
    pub fn home_str(&self) -> &str {
        self.home.to_str().unwrap_or_default()
    }

    /// The approved workspace as UTF-8. Validated configuration always has one.
    pub fn workspace_str(&self) -> &str {
        self.workspace.to_str().unwrap_or_default()
    }

    /// Share-safe alias used in every durable record and projection.
    ///
    /// Only the workspace's own directory name is published; the absolute path
    /// stays in configuration and in the redaction policy, so no host path ever
    /// reaches a durable record, an event, or an operator projection.
    ///
    /// The alias is also what an orchestrator binds itself to, so a directory
    /// name that is not a valid opaque reference — a dotfile, a name with
    /// spaces — falls back to a derived identifier rather than producing an
    /// alias that could never be bound.
    pub fn workspace_alias(&self) -> String {
        self.workspace
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::identity::ExternalRef::new)
            .map_or_else(
                || crate::identity::opaque_id("ws", &[self.workspace_str()]),
                |name| name.as_str().to_owned(),
            )
    }

    /// Whether an explicit operator grant covers a gated capability.
    pub fn has_grant(&self, capability_id: &str) -> bool {
        self.grants.iter().any(|grant| grant == capability_id)
    }

    /// Share-safe echo of the effective configuration.
    ///
    /// Host paths are replaced by their labels so `config-check` output can be
    /// pasted into a bug report.
    pub fn redacted_view(&self) -> serde_json::Value {
        serde_json::json!({
            "home": crate::redaction::HOME_LABEL,
            "workspace": crate::redaction::WORKSPACE_LABEL,
            "sessionId": self.session_id,
            "capabilities": self
                .capabilities
                .capabilities
                .iter()
                .map(|capability| {
                    serde_json::json!({
                        "id": capability.id,
                        "availability": capability.availability,
                        "humanGate": capability.human_gate,
                        "granted": self.has_grant(&capability.id),
                    })
                })
                .collect::<Vec<_>>(),
            "limits": self.limits,
            "engine": match &self.engine {
                EngineSelection::Disabled => "disabled",
                EngineSelection::Fixture { .. } => "fixture",
            },
        })
    }
}

fn utf8_path<'a>(path: &'a Path, field: &str) -> HostResult<&'a str> {
    path.to_str()
        .ok_or_else(|| HostError::invalid("path_not_utf8", format!("{field} must be valid UTF-8")))
}

fn validate_session_id(session_id: &str) -> HostResult<()> {
    if session_id.trim().is_empty() || session_id.len() > 128 {
        return Err(HostError::invalid(
            "session_id_invalid",
            "sessionId must be non-empty and bounded",
        ));
    }
    if session_id
        .chars()
        .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err(HostError::invalid(
            "session_id_invalid",
            "sessionId must not contain control or path characters",
        ));
    }
    Ok(())
}

/// Classify a deserialization failure without echoing caller content.
trait ClassifyLabel {
    fn classify_label(&self) -> &'static str;
}

impl ClassifyLabel for serde_json::Error {
    fn classify_label(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "shape",
            serde_json::error::Category::Eof => "truncated",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn config_with_workspace(name: &str) -> HostConfig {
        let root = testing::fixture_root();
        testing::config_for(&root.join("host-home"), &root.join("workspace").join(name))
    }

    #[test]
    fn a_bindable_directory_name_is_published_as_the_alias() {
        assert_eq!(
            config_with_workspace("project").workspace_alias(),
            "project"
        );
        assert_eq!(
            config_with_workspace("my-repo.v2").workspace_alias(),
            "my-repo.v2"
        );
    }

    #[test]
    fn a_name_that_could_never_be_bound_falls_back_to_a_derived_alias() {
        // A dotfile, a name with spaces, and a traversal-shaped name all fail
        // the opaque-reference rules, so publishing them would produce an alias
        // an orchestrator could not bind to.
        for awkward in [".hidden", "two words", "trailing-"] {
            let alias = config_with_workspace(awkward).workspace_alias();
            assert!(
                alias.starts_with("ws-"),
                "{awkward:?} should fall back, got {alias}"
            );
            assert!(
                crate::identity::ExternalRef::new(&alias).is_some(),
                "the fallback alias must itself be bindable"
            );
        }
    }

    #[test]
    fn distinct_unbindable_workspaces_do_not_collide() {
        assert_ne!(
            config_with_workspace(".one").workspace_alias(),
            config_with_workspace(".two").workspace_alias()
        );
    }
}
