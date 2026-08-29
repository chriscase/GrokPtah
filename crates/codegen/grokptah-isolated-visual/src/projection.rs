use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{IsolatedError, IsolatedResult};
use crate::lease::{ComputerDispatchState, ComputerSurfaceLease, ComputerSurfaceLeaseState};
use crate::lifecycle::{
    IsolatedEvidenceClass, IsolatedGuestPhase, IsolatedGuestRecord, IsolatedGuestTerminal,
};

const FORBIDDEN_CAPTURE_KEYS: &[&str] = &[
    "apiKey",
    "api_key",
    "authorization",
    "baseUrl",
    "base_url",
    "bearer",
    "channelSecret",
    "channel_secret",
    "clipboard",
    "clipboardContents",
    "clipboard_contents",
    "credential",
    "credentials",
    "helperPath",
    "helper_path",
    "hostClipboard",
    "hostHome",
    "host_home",
    "ipAddress",
    "ip_address",
    "ipv4",
    "ipv6",
    "macAddress",
    "mac_address",
    "networkDevices",
    "networkInterface",
    "network_interface",
    "overlayPath",
    "overlay_path",
    "password",
    "sharedDirectory",
    "shared_directory",
    "ssid",
    "token",
];

/// Redacted public projection. No frame bytes, paths, clipboard, credentials,
/// network identities, or helper secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualProjection {
    pub guest_id: String,
    pub run_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub surface_id: String,
    pub incarnation: String,
    pub phase: IsolatedGuestPhase,
    pub terminal: Option<IsolatedGuestTerminal>,
    pub cleaned: bool,
    pub evidence_class: IsolatedEvidenceClass,
    pub conflict_domain_id: String,
    pub lease: Option<IsolatedLeaseProjection>,
    pub frame_epoch: u64,
    pub frames_seen: u32,
    pub input_events_seen: u32,
    pub resident_frame_bytes: u64,
    pub captured_bytes: u64,
    pub virtualization_framework_launched: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedLeaseProjection {
    pub lease_id: String,
    pub state: ComputerSurfaceLeaseState,
    pub revision: u64,
    pub queue_sequence: u64,
    pub dispatch_state: Option<ComputerDispatchState>,
    pub expires_at: DateTime<Utc>,
}

pub fn project_guest(
    guest: &IsolatedGuestRecord,
    lease: Option<&ComputerSurfaceLease>,
    virtualization_framework_launched: bool,
) -> IsolatedVisualProjection {
    IsolatedVisualProjection {
        guest_id: guest.guest_id.clone(),
        run_id: guest.run_id.clone(),
        work_id: guest.work_id.clone(),
        work_attempt_id: guest.work_attempt_id.clone(),
        agent_id: guest.agent_id.clone(),
        surface_id: guest.surface.surface_id.clone(),
        incarnation: guest.surface.incarnation.clone(),
        phase: guest.phase,
        terminal: guest.terminal,
        cleaned: guest.cleaned,
        evidence_class: guest.evidence_class,
        conflict_domain_id: guest.conflict_domain_id.clone(),
        lease: lease.map(|lease| IsolatedLeaseProjection {
            lease_id: lease.lease_id.clone(),
            state: lease.state,
            revision: lease.revision,
            queue_sequence: lease.queue_sequence,
            dispatch_state: lease.dispatch.as_ref().map(|dispatch| dispatch.state),
            expires_at: lease.expires_at,
        }),
        frame_epoch: guest.frame_epoch,
        frames_seen: guest.frames_seen,
        input_events_seen: guest.input_events_seen,
        resident_frame_bytes: guest.resident_frame_bytes,
        captured_bytes: guest.captured_bytes,
        virtualization_framework_launched,
        updated_at: guest.updated_at,
    }
}

pub fn redact_public_value(value: &Value) -> IsolatedResult<Value> {
    let mut redacted = value.clone();
    strip_forbidden_keys(&mut redacted);
    if contains_forbidden_key(&redacted) || contains_sensitive_needle(&redacted) {
        return Err(IsolatedError::forbidden(
            "isolated projection still contains a forbidden path, clipboard, credential, or network field",
        ));
    }
    Ok(redacted)
}

fn strip_forbidden_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !is_forbidden_key(key));
            for child in map.values_mut() {
                strip_forbidden_keys(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_forbidden_keys(child);
            }
        }
        _ => {}
    }
}

fn is_forbidden_key(key: &str) -> bool {
    FORBIDDEN_CAPTURE_KEYS
        .iter()
        .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
}

fn contains_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| is_forbidden_key(key)) || map.values().any(contains_forbidden_key)
        }
        Value::Array(values) => values.iter().any(contains_forbidden_key),
        _ => false,
    }
}

pub fn contains_sensitive_needle(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().any(contains_sensitive_needle),
        Value::Array(values) => values.iter().any(contains_sensitive_needle),
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("/users/")
                || lower.contains("/private/")
                || lower.contains("/home/")
                || lower.contains("clipboard:")
                || lower.contains("password=")
                || lower.contains("token=")
                || lower.contains("api_key=")
                || lower.contains("ssid=")
                || lower.starts_with("http://")
                || lower.starts_with("https://")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_json_has_no_frame_bytes_or_needles() {
        let dirty = json!({
            "frameSequence": 1,
            "clipboard": "secret-paste",
            "helperPath": "/Users/chris/helper",
            "width": 2
        });
        let redacted = redact_public_value(&dirty).unwrap();
        let text = redacted.to_string();
        assert!(!text.contains("clipboard"));
        assert!(!text.contains("helperPath"));
        assert_eq!(redacted["width"], 2);
        assert!(redact_public_value(&json!({ "note": "clipboard: copied token=abc" })).is_err());
    }
}
