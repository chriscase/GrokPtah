use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{hex_encode, sha256_hex, validate_digest, validate_id};
use crate::manifest::IsolatedVisualResourceLimits;

type HmacSha256 = Hmac<Sha256>;

pub const CHANNEL_SECRET_BYTES: usize = 32;
pub const MAX_SIGNED_ENVELOPE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedPointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IsolatedInputKind {
    PointerMove {
        x: u32,
        y: u32,
    },
    PointerButton {
        x: u32,
        y: u32,
        button: IsolatedPointerButton,
        pressed: bool,
    },
    Key {
        code: String,
        pressed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedInputEvent {
    pub dispatch_id: String,
    pub guest_id: String,
    pub lease_id: String,
    pub lease_revision: u64,
    pub surface_id: String,
    pub incarnation: String,
    pub frame_epoch: u64,
    pub kind: IsolatedInputKind,
}

impl IsolatedInputEvent {
    pub fn validate(&self, limits: &IsolatedVisualResourceLimits) -> IsolatedResult<()> {
        validate_id("dispatch_id", &self.dispatch_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_id("lease_id", &self.lease_id)?;
        validate_id("surface_id", &self.surface_id)?;
        validate_id("incarnation", &self.incarnation)?;
        if self.lease_revision == 0 || self.frame_epoch == 0 {
            return Err(IsolatedError::invalid("input event is missing host epochs"));
        }
        match &self.kind {
            IsolatedInputKind::PointerMove { x, y }
            | IsolatedInputKind::PointerButton { x, y, .. } => {
                if *x >= limits.display_width || *y >= limits.display_height {
                    return Err(IsolatedError::invalid(
                        "pointer event is outside the isolated surface",
                    ));
                }
            }
            IsolatedInputKind::Key { code, .. } => {
                if code.is_empty()
                    || code.len() as u32 > limits.text_event_bytes
                    || code.contains('\0')
                {
                    return Err(IsolatedError::invalid("key event is invalid"));
                }
            }
        }
        Ok(())
    }

    pub fn payload_sha256(&self) -> IsolatedResult<String> {
        let encoded = serde_json::to_vec(self)
            .map_err(|_| IsolatedError::internal("input event is not serializable"))?;
        Ok(sha256_hex(&encoded))
    }
}

/// Public/durable frame metadata. Frame bytes never enter this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedFrameMeta {
    pub frame_id: String,
    pub guest_id: String,
    pub surface_id: String,
    pub incarnation: String,
    pub lease_id: String,
    pub lease_revision: u64,
    pub frame_epoch: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub content_sha256: String,
    pub encoded_bytes: u64,
    pub mac_sha256: String,
    pub captured_at: DateTime<Utc>,
}

impl IsolatedFrameMeta {
    pub fn validate(&self, limits: &IsolatedVisualResourceLimits) -> IsolatedResult<()> {
        validate_id("frame_id", &self.frame_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_id("surface_id", &self.surface_id)?;
        validate_id("incarnation", &self.incarnation)?;
        validate_id("lease_id", &self.lease_id)?;
        validate_digest("content_sha256", &self.content_sha256)?;
        validate_digest("mac_sha256", &self.mac_sha256)?;
        if self.width == 0
            || self.height == 0
            || self.width > limits.display_width
            || self.height > limits.display_height
            || self.encoded_bytes == 0
            || self.encoded_bytes > limits.encoded_frame_bytes
            || self.sequence == 0
            || self.frame_epoch == 0
        {
            return Err(IsolatedError::invalid("isolated frame metadata is invalid"));
        }
        Ok(())
    }
}

pub fn mac_frame(
    secret: &[u8; CHANNEL_SECRET_BYTES],
    meta: &IsolatedFrameMeta,
) -> IsolatedResult<String> {
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| IsolatedError::internal("channel secret rejected"))?;
    mac.update(meta.frame_id.as_bytes());
    mac.update(b"\0");
    mac.update(meta.incarnation.as_bytes());
    mac.update(b"\0");
    mac.update(&meta.sequence.to_be_bytes());
    mac.update(meta.content_sha256.as_bytes());
    Ok(hex_encode(&mac.finalize().into_bytes()))
}

pub fn verify_frame_mac(
    secret: &[u8; CHANNEL_SECRET_BYTES],
    meta: &IsolatedFrameMeta,
) -> IsolatedResult<()> {
    let expected = mac_frame(secret, meta)?;
    if expected != meta.mac_sha256 {
        return Err(IsolatedError::unauthorized("isolated frame MAC is invalid"));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ResidentFrame {
    pub meta: IsolatedFrameMeta,
    bytes: Vec<u8>,
}

impl ResidentFrame {
    pub fn new(meta: IsolatedFrameMeta, bytes: Vec<u8>) -> IsolatedResult<Self> {
        if bytes.len() as u64 != meta.encoded_bytes || sha256_hex(&bytes) != meta.content_sha256 {
            return Err(IsolatedError::conflict(
                "resident frame bytes do not match metadata",
            ));
        }
        Ok(Self { meta, bytes })
    }

    pub fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Bytes are available only to the host-owned surface. They are never
    /// serialized into projections, stores, or MCP payloads.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::IsolatedVisualResourceLimits;

    #[test]
    fn pointer_outside_surface_is_rejected() {
        let limits = IsolatedVisualResourceLimits::proof_defaults();
        let event = IsolatedInputEvent {
            dispatch_id: "dispatch-1".into(),
            guest_id: "guest-1".into(),
            lease_id: "lease-1".into(),
            lease_revision: 1,
            surface_id: "surface-1".into(),
            incarnation: "inc-1".into(),
            frame_epoch: 1,
            kind: IsolatedInputKind::PointerMove { x: 9_999, y: 0 },
        };
        assert!(event.validate(&limits).is_err());
    }
}
