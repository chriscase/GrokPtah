//! Declared versus measured Computer Use capability evidence (#272/#458).
//!
//! Model names and provider declarations never grant action authority. A
//! measured capability is still bound to the exact host-issued capability
//! generation and route. Synthetic qualification is retained as synthetic
//! evidence and is capped at Economy.

use serde::{Deserialize, Serialize};

use super::authority_seam::HostIssuedBinding;
use super::profile::AdaptiveProfile;
use crate::gateway_config::{CapabilitySource, ComputerUseTier, ModelCapabilities};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAttribution {
    Unknown,
    Declared,
    Measured,
}

impl CapabilityAttribution {
    pub const fn from_source(source: CapabilitySource) -> Self {
        match source {
            CapabilitySource::Unknown => Self::Unknown,
            CapabilitySource::Declared => Self::Declared,
            CapabilitySource::Measured => Self::Measured,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Declared => "declared",
            Self::Measured => "measured",
        }
    }
}

/// Capability facts for one exact provider route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityEvidence {
    pub tools: bool,
    pub image_input: bool,
    pub max_image_bytes: Option<u64>,
    pub tier: ComputerUseTier,
    pub attribution: CapabilityAttribution,
    pub durable_authority: bool,
    pub session_measured: bool,
    pub synthetic_only: bool,
}

impl ModelCapabilityEvidence {
    pub(crate) fn from_model_capabilities(
        capabilities: &ModelCapabilities,
        authority: Option<&HostIssuedBinding>,
        session_measured: bool,
        synthetic_only: bool,
    ) -> Self {
        Self {
            tools: capabilities.tools,
            image_input: capabilities.image_input,
            max_image_bytes: capabilities.max_image_bytes,
            tier: capabilities.effective_computer_use_tier(),
            attribution: CapabilityAttribution::from_source(
                capabilities.computer_capability_source,
            ),
            durable_authority: authority.is_some()
                && capabilities.computer_capability_source == CapabilitySource::Measured,
            session_measured,
            synthetic_only,
        }
    }

    pub const fn is_text_oriented(&self) -> bool {
        !self.image_input
    }

    pub const fn has_qualified_visual_path(&self) -> bool {
        self.image_input
            && self.max_image_bytes.is_some()
            && matches!(self.tier, ComputerUseTier::VisualFallbackAct)
            && self.durable_authority
            && !self.synthetic_only
    }

    /// Only measured, structured-tool capability may propose. Declared
    /// capability remains useful metadata, never an authority substitute.
    pub const fn may_propose(&self) -> bool {
        self.tools
            && matches!(self.attribution, CapabilityAttribution::Measured)
            && self.durable_authority
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilityEvidence {
    pub semantic_observation: bool,
    pub screenshot_capture: bool,
    pub independent_verifier: bool,
    pub isolated_guest: bool,
}

impl HostCapabilityEvidence {
    pub const SEMANTIC_ONLY: Self = Self {
        semantic_observation: true,
        screenshot_capture: false,
        independent_verifier: false,
        isolated_guest: false,
    };
}

/// Complete evidence used for one deterministic profile decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvidence {
    pub model: ModelCapabilityEvidence,
    pub host: HostCapabilityEvidence,
    /// Publicly safe opaque capability snapshot reference. The actual
    /// generation object is retained only in-process and is never serialized.
    #[serde(skip)]
    pub(crate) authority: Option<HostIssuedBinding>,
}

impl CapabilityEvidence {
    pub fn new(model: ModelCapabilityEvidence, host: HostCapabilityEvidence) -> Self {
        Self {
            model,
            host,
            authority: None,
        }
    }

    pub(crate) fn with_authority(
        model: ModelCapabilityEvidence,
        host: HostCapabilityEvidence,
        authority: HostIssuedBinding,
    ) -> Self {
        Self {
            model,
            host,
            authority: Some(authority),
        }
    }

    pub(crate) fn bind_authority(&mut self, authority: HostIssuedBinding) {
        self.authority = Some(authority);
    }

    /// Synthetic evidence is explicit and cannot be mistaken for live
    /// eligibility. It is intentionally limited to the semantic Economy path.
    pub fn synthetic(model: ModelCapabilityEvidence, host: HostCapabilityEvidence) -> Self {
        Self {
            model: ModelCapabilityEvidence {
                durable_authority: false,
                session_measured: true,
                synthetic_only: true,
                ..model
            },
            host,
            authority: None,
        }
    }

    pub fn capability_snapshot_reference(&self) -> Option<String> {
        self.authority
            .as_ref()
            .map(|authority| authority.capability_reference().to_string())
    }

    pub fn may_propose(&self) -> bool {
        self.authority.is_some() && self.model.may_propose()
    }

    pub(crate) fn principal_generation_reference(&self) -> Option<String> {
        self.authority
            .as_ref()
            .map(|authority| authority.principal_reference().to_string())
    }

    /// The highest profile the evidence can honestly support. This is a
    /// ceiling, not a selection.
    pub fn ceiling(&self) -> AdaptiveProfile {
        if !self.may_propose() {
            return AdaptiveProfile::Economy;
        }
        if self.model.synthetic_only || self.model.is_text_oriented() {
            return AdaptiveProfile::Economy;
        }
        if self.model.has_qualified_visual_path()
            && self.host.screenshot_capture
            && self.host.independent_verifier
        {
            return AdaptiveProfile::HighAssurance;
        }
        if self.host.semantic_observation {
            AdaptiveProfile::Balanced
        } else {
            AdaptiveProfile::Economy
        }
    }
}
