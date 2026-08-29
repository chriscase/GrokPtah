use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{
    validate_digest, validate_id, validate_relative_path, GUEST_PROTOCOL_VERSION,
    ISOLATED_VISUAL_BACKEND_ID, SCHEMA_VERSION,
};

pub const MAX_VCPUS: u8 = 2;
pub const MAX_MEMORY_MIB: u32 = 4_096;
pub const MAX_OVERLAY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_DISPLAY_WIDTH: u32 = 1_280;
pub const MAX_DISPLAY_HEIGHT: u32 = 800;
pub const MAX_FRAMES_PER_SECOND: u8 = 10;
pub const MAX_ENCODED_FRAME_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_DURATION_SECONDS: u64 = 30 * 60;
pub const MAX_INPUT_EVENTS: u32 = 256;
pub const MAX_TEXT_EVENT_BYTES: u32 = 4 * 1024;
pub const MAX_RESIDENT_FRAME_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SOURCE_BLOB_BYTES: u64 = 1024 * 1024;
pub const MAX_SOURCE_OBJECTS: usize = 64;
pub const MAX_CONCURRENT_GUESTS: usize = 4;
pub const MAX_SURFACE_LEASES: usize = 512;

/// Closed default profile. Any host bridge is a new reviewed capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualSecurityProfile {
    pub network_devices: u8,
    pub host_clipboard: bool,
    pub shared_directories: bool,
    pub credential_forwarding: bool,
    pub host_input_forwarding: bool,
    pub usb_passthrough: bool,
    pub camera: bool,
    pub microphone: bool,
}

impl IsolatedVisualSecurityProfile {
    pub fn locked_down() -> Self {
        Self {
            network_devices: 0,
            host_clipboard: false,
            shared_directories: false,
            credential_forwarding: false,
            host_input_forwarding: false,
            usb_passthrough: false,
            camera: false,
            microphone: false,
        }
    }

    pub fn validate(&self) -> IsolatedResult<()> {
        if *self != Self::locked_down() {
            return Err(IsolatedError::forbidden(
                "isolated visual profile requests an unreviewed host bridge",
            ));
        }
        Ok(())
    }
}

/// Surface budgets. Throughput and residency are independent.
///
/// Exhausting `max_frames`, `max_input_events`, or `duration_seconds` is
/// terminal. Exhausting `max_captured_bytes` degrades capture and is not a
/// default terminal `LimitReached`. Resident bytes decrement on rotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualResourceLimits {
    pub virtual_cpus: u8,
    pub memory_mib: u32,
    pub overlay_bytes: u64,
    pub display_width: u32,
    pub display_height: u32,
    pub frames_per_second: u8,
    pub encoded_frame_bytes: u64,
    pub duration_seconds: u64,
    pub max_frames: u32,
    pub max_input_events: u32,
    pub text_event_bytes: u32,
    pub max_resident_frame_bytes: u64,
    pub max_captured_bytes: u64,
    pub max_surface_bytes: u64,
}

impl IsolatedVisualResourceLimits {
    pub fn proof_defaults() -> Self {
        Self {
            virtual_cpus: MAX_VCPUS,
            memory_mib: 512,
            overlay_bytes: 64 * 1024 * 1024,
            display_width: MAX_DISPLAY_WIDTH,
            display_height: MAX_DISPLAY_HEIGHT,
            frames_per_second: MAX_FRAMES_PER_SECOND,
            encoded_frame_bytes: MAX_ENCODED_FRAME_BYTES,
            duration_seconds: 10 * 60,
            max_frames: 120,
            max_input_events: MAX_INPUT_EVENTS,
            text_event_bytes: MAX_TEXT_EVENT_BYTES,
            max_resident_frame_bytes: MAX_RESIDENT_FRAME_BYTES,
            max_captured_bytes: 64 * 1024 * 1024,
            max_surface_bytes: MAX_RESIDENT_FRAME_BYTES,
        }
    }

    pub fn validate(&self) -> IsolatedResult<()> {
        let ok = self.virtual_cpus > 0
            && self.virtual_cpus <= MAX_VCPUS
            && self.memory_mib > 0
            && self.memory_mib <= MAX_MEMORY_MIB
            && self.overlay_bytes > 0
            && self.overlay_bytes <= MAX_OVERLAY_BYTES
            && self.display_width > 0
            && self.display_width <= MAX_DISPLAY_WIDTH
            && self.display_height > 0
            && self.display_height <= MAX_DISPLAY_HEIGHT
            && self.frames_per_second > 0
            && self.frames_per_second <= MAX_FRAMES_PER_SECOND
            && self.encoded_frame_bytes > 0
            && self.encoded_frame_bytes <= MAX_ENCODED_FRAME_BYTES
            && self.duration_seconds > 0
            && self.duration_seconds <= MAX_DURATION_SECONDS
            && self.max_frames > 0
            && self.max_input_events > 0
            && self.max_input_events <= MAX_INPUT_EVENTS
            && self.text_event_bytes > 0
            && self.text_event_bytes <= MAX_TEXT_EVENT_BYTES
            && self.max_resident_frame_bytes > 0
            && self.max_resident_frame_bytes <= MAX_RESIDENT_FRAME_BYTES
            && self.max_captured_bytes > 0
            && self.max_surface_bytes > 0
            && self.max_surface_bytes <= self.max_resident_frame_bytes;
        if !ok {
            return Err(IsolatedError::limit(
                "isolated visual resource request exceeds the proof ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceObjectKind {
    Blob,
    Manifest,
    Helper,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceObject {
    pub digest_sha256: String,
    pub kind: SourceObjectKind,
    pub media_type: String,
    pub byte_len: u64,
}

impl SourceObject {
    pub fn validate(&self) -> IsolatedResult<()> {
        validate_digest("object digest", &self.digest_sha256)?;
        let max = match self.kind {
            SourceObjectKind::Image => MAX_OVERLAY_BYTES,
            SourceObjectKind::Helper => 64 * 1024 * 1024,
            SourceObjectKind::Blob | SourceObjectKind::Manifest => MAX_SOURCE_BLOB_BYTES,
        };
        if self.byte_len == 0 || self.byte_len > max {
            return Err(IsolatedError::limit("source object exceeds size limits"));
        }
        match (self.kind, self.media_type.as_str()) {
            (SourceObjectKind::Blob, "text/plain")
            | (SourceObjectKind::Blob, "text/x-c")
            | (SourceObjectKind::Blob, "text/x-c-header")
            | (SourceObjectKind::Manifest, "application/json")
            | (SourceObjectKind::Helper, "application/octet-stream")
            | (SourceObjectKind::Image, "application/vnd.grokptah.guest-image.v1") => Ok(()),
            _ => Err(IsolatedError::forbidden(
                "source object media type is not allowlisted",
            )),
        }
    }
}

/// Content-addressed guest/helper/image closure. Paths are allowlisted;
/// identity is the digest, never a Git ref, worktree, or ambient index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedSourceManifest {
    pub schema_version: u32,
    pub backend_id: String,
    pub guest_protocol_version: u32,
    pub objects: Vec<IsolatedSourceEntry>,
    pub helper_content_sha256: String,
    pub helper_signing_requirement_sha256: String,
    pub guest_image_sha256: Option<String>,
    pub configuration_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedSourceEntry {
    pub relative_path: String,
    pub object: SourceObject,
}

impl IsolatedSourceManifest {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.backend_id != ISOLATED_VISUAL_BACKEND_ID
            || self.guest_protocol_version != GUEST_PROTOCOL_VERSION
        {
            return Err(IsolatedError::invalid(
                "isolated source manifest version or backend identity is unsupported",
            ));
        }
        if self.objects.is_empty() || self.objects.len() > MAX_SOURCE_OBJECTS {
            return Err(IsolatedError::invalid(
                "isolated source object closure is empty or oversized",
            ));
        }
        validate_digest("helper content digest", &self.helper_content_sha256)?;
        validate_digest(
            "helper signing requirement digest",
            &self.helper_signing_requirement_sha256,
        )?;
        validate_digest("configuration digest", &self.configuration_sha256)?;
        if let Some(image) = &self.guest_image_sha256 {
            validate_digest("guest image digest", image)?;
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut casefold = std::collections::BTreeSet::new();
        for entry in &self.objects {
            validate_relative_path(&entry.relative_path)?;
            entry.object.validate()?;
            if !seen.insert(entry.relative_path.clone()) {
                return Err(IsolatedError::conflict(
                    "duplicate source path in object closure",
                ));
            }
            if !casefold.insert(crate::ids::casefold_key(&entry.relative_path)) {
                return Err(IsolatedError::conflict(
                    "source path collides under case-insensitive comparison",
                ));
            }
        }
        Ok(())
    }

    pub fn allowlist(&self) -> Vec<&str> {
        self.objects
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualManifest {
    pub schema_version: u32,
    pub backend_id: String,
    pub guest_protocol_version: u32,
    pub helper_content_sha256: String,
    pub helper_signing_requirement_sha256: String,
    pub guest_image_sha256: String,
    pub configuration_sha256: String,
    pub security_profile: IsolatedVisualSecurityProfile,
    pub limits: IsolatedVisualResourceLimits,
}

impl IsolatedVisualManifest {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.backend_id != ISOLATED_VISUAL_BACKEND_ID
            || self.guest_protocol_version != GUEST_PROTOCOL_VERSION
        {
            return Err(IsolatedError::invalid(
                "isolated visual manifest version or backend identity is unsupported",
            ));
        }
        validate_digest("helper content digest", &self.helper_content_sha256)?;
        validate_digest(
            "helper signing requirement digest",
            &self.helper_signing_requirement_sha256,
        )?;
        validate_digest("guest image digest", &self.guest_image_sha256)?;
        validate_digest("configuration digest", &self.configuration_sha256)?;
        self.security_profile.validate()?;
        self.limits.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerSurfaceBinding {
    pub surface_id: String,
    pub incarnation: String,
}

impl ComputerSurfaceBinding {
    pub fn issue() -> Self {
        Self {
            surface_id: uuid::Uuid::new_v4().to_string(),
            incarnation: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn next_incarnation(&self) -> Self {
        Self {
            surface_id: self.surface_id.clone(),
            incarnation: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn validate(&self) -> IsolatedResult<()> {
        validate_id("surface_id", &self.surface_id)?;
        validate_id("incarnation", &self.incarnation)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperIdentity {
    pub helper_id: String,
    pub content_sha256: String,
    pub signing_requirement_sha256: String,
}

impl HelperIdentity {
    pub fn validate(&self) -> IsolatedResult<()> {
        validate_id("helper_id", &self.helper_id)?;
        validate_digest("helper content digest", &self.content_sha256)?;
        validate_digest(
            "helper signing requirement digest",
            &self.signing_requirement_sha256,
        )?;
        Ok(())
    }
}
