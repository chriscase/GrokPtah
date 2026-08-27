use std::fmt;

use ring::hmac;
use serde::{Deserialize, Serialize};

use super::isolated_visual::{
    IsolatedVisualLaunchContract, IsolatedVisualResourceLimits,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
};
use super::types::{
    validate_id, ComputerError, ComputerErrorCode, ComputerResult, ComputerSurfaceBinding,
};

pub const ISOLATED_VISUAL_CHANNEL_SECRET_BYTES: usize = 32;
pub const ISOLATED_VISUAL_MAX_SIGNED_ENVELOPE_BYTES: usize = 128 * 1024;

fn validate_digest(name: &str, value: &str) -> ComputerResult<()> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("invalid {name}"),
        ));
    }
    Ok(())
}

fn validate_request_nonce(value: &str) -> ComputerResult<()> {
    let nonce = uuid::Uuid::parse_str(value).map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated protocol request nonce is not a canonical UUIDv4",
        )
    })?;
    if nonce.get_version_num() != 4 || nonce.to_string() != value {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated protocol request nonce is not a canonical UUIDv4",
        ));
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolRole {
    Host,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualGuestHealth {
    Booting,
    ReadOnlyReady,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedVisualGuestFailure {
    MalformedRequest,
    StaleFrame,
    AuthenticationFailed,
    LimitReached,
    BackendFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IsolatedVisualHostMessage {
    Observe {
        maximum_frame_bytes: u64,
        maximum_width: u32,
        maximum_height: u32,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IsolatedVisualGuestMessage {
    Frame {
        content_sha256: String,
        encoded_bytes: u64,
        width: u32,
        height: u32,
    },
    Health {
        state: IsolatedVisualGuestHealth,
    },
    ShutdownAck,
    Failure {
        code: IsolatedVisualGuestFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "direction",
    content = "message",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IsolatedVisualProtocolPayload {
    HostToGuest(IsolatedVisualHostMessage),
    GuestToHost(IsolatedVisualGuestMessage),
}

impl IsolatedVisualProtocolPayload {
    fn role(&self) -> ProtocolRole {
        match self {
            Self::HostToGuest(_) => ProtocolRole::Host,
            Self::GuestToHost(_) => ProtocolRole::Guest,
        }
    }

    fn is_frame(&self) -> bool {
        matches!(
            self,
            Self::GuestToHost(IsolatedVisualGuestMessage::Frame { .. })
        )
    }

    fn validate(&self, limits: &IsolatedVisualResourceLimits) -> ComputerResult<()> {
        match self {
            Self::HostToGuest(IsolatedVisualHostMessage::Observe {
                maximum_frame_bytes,
                maximum_width,
                maximum_height,
            }) => {
                if *maximum_frame_bytes == 0
                    || *maximum_frame_bytes > limits.encoded_frame_bytes
                    || *maximum_width == 0
                    || *maximum_width > limits.display_width
                    || *maximum_height == 0
                    || *maximum_height > limits.display_height
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "isolated observe request exceeds the measured manifest",
                    ));
                }
            }
            Self::GuestToHost(IsolatedVisualGuestMessage::Frame {
                content_sha256,
                encoded_bytes,
                width,
                height,
            }) => {
                validate_digest("isolated frame digest", content_sha256)?;
                if *encoded_bytes == 0
                    || *encoded_bytes > limits.encoded_frame_bytes
                    || *width == 0
                    || *width > limits.display_width
                    || *height == 0
                    || *height > limits.display_height
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "isolated frame exceeds the measured manifest",
                    ));
                }
            }
            Self::HostToGuest(IsolatedVisualHostMessage::Stop)
            | Self::GuestToHost(IsolatedVisualGuestMessage::Health { .. })
            | Self::GuestToHost(IsolatedVisualGuestMessage::ShutdownAck)
            | Self::GuestToHost(IsolatedVisualGuestMessage::Failure { .. }) => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualProtocolSurfaceBinding {
    surface_id: String,
    incarnation: String,
}

impl IsolatedVisualProtocolSurfaceBinding {
    fn from_surface(surface: &ComputerSurfaceBinding) -> ComputerResult<Self> {
        surface.validate()?;
        Ok(Self {
            surface_id: surface.surface_id().to_string(),
            incarnation: surface.incarnation().to_string(),
        })
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    fn validate(&self) -> ComputerResult<()> {
        validate_id("surface_id", &self.surface_id)?;
        validate_id("incarnation", &self.incarnation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolatedVisualProtocolEnvelope {
    pub protocol_version: u32,
    pub run_id: String,
    pub surface: IsolatedVisualProtocolSurfaceBinding,
    pub sequence: u64,
    pub frame_sequence: u64,
    pub input_sequence: u64,
    pub request_nonce: String,
    pub payload_len: u64,
    pub payload: IsolatedVisualProtocolPayload,
    pub authenticator_sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnsignedEnvelope<'a> {
    protocol_version: u32,
    run_id: &'a str,
    surface: &'a IsolatedVisualProtocolSurfaceBinding,
    sequence: u64,
    frame_sequence: u64,
    input_sequence: u64,
    request_nonce: &'a str,
    payload_len: u64,
    payload: &'a IsolatedVisualProtocolPayload,
}

impl IsolatedVisualProtocolEnvelope {
    fn unsigned(&self) -> UnsignedEnvelope<'_> {
        UnsignedEnvelope {
            protocol_version: self.protocol_version,
            run_id: &self.run_id,
            surface: &self.surface,
            sequence: self.sequence,
            frame_sequence: self.frame_sequence,
            input_sequence: self.input_sequence,
            request_nonce: &self.request_nonce,
            payload_len: self.payload_len,
            payload: &self.payload,
        }
    }
}

/// Host or guest endpoint for the authenticated read-only proof channel. The
/// HMAC key is intentionally non-serializable and redacted from Debug output.
pub struct IsolatedVisualProtocolSession {
    role: ProtocolRole,
    key: hmac::Key,
    run_id: String,
    surface: IsolatedVisualProtocolSurfaceBinding,
    limits: IsolatedVisualResourceLimits,
    outbound_sequence: u64,
    inbound_sequence: u64,
    outbound_frame_sequence: u64,
    inbound_frame_sequence: u64,
    outstanding_request_nonce: Option<String>,
    accepted_request_nonce: Option<String>,
}

impl fmt::Debug for IsolatedVisualProtocolSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedVisualProtocolSession")
            .field("role", &self.role)
            .field("run_id", &self.run_id)
            .field("surface", &self.surface)
            .field("outbound_sequence", &self.outbound_sequence)
            .field("inbound_sequence", &self.inbound_sequence)
            .field("channel_secret", &"[REDACTED]")
            .finish()
    }
}

impl IsolatedVisualProtocolSession {
    pub fn new_host(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(ProtocolRole::Host, contract, channel_secret)
    }

    pub fn new_guest(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(ProtocolRole::Guest, contract, channel_secret)
    }

    fn new(
        role: ProtocolRole,
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        contract.validate()?;
        if channel_secret.len() != ISOLATED_VISUAL_CHANNEL_SECRET_BYTES
            || channel_secret.iter().all(|byte| *byte == 0)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual channel secret is missing or invalid",
            ));
        }
        Ok(Self {
            role,
            key: hmac::Key::new(hmac::HMAC_SHA256, channel_secret),
            run_id: contract.run_id.clone(),
            surface: IsolatedVisualProtocolSurfaceBinding::from_surface(&contract.surface)?,
            limits: contract.manifest.limits.clone(),
            outbound_sequence: 0,
            inbound_sequence: 0,
            outbound_frame_sequence: 0,
            inbound_frame_sequence: 0,
            outstanding_request_nonce: None,
            accepted_request_nonce: None,
        })
    }

    pub fn seal(
        &mut self,
        request_nonce: String,
        frame_sequence: u64,
        input_sequence: u64,
        payload: IsolatedVisualProtocolPayload,
    ) -> ComputerResult<IsolatedVisualProtocolEnvelope> {
        if payload.role() != self.role {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "isolated protocol endpoint cannot send the opposite direction",
            ));
        }
        payload.validate(&self.limits)?;
        validate_request_nonce(&request_nonce)?;
        if input_sequence != 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "read-only isolated protocol does not accept input events",
            ));
        }
        if frame_sequence < self.outbound_frame_sequence
            || (payload.is_frame() && frame_sequence == self.outbound_frame_sequence)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated protocol frame sequence is stale",
            ));
        }
        match self.role {
            ProtocolRole::Host if self.outstanding_request_nonce.is_some() => {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated protocol permits only one outstanding request",
                ));
            }
            ProtocolRole::Guest
                if self.accepted_request_nonce.as_deref() != Some(request_nonce.as_str()) =>
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenTarget,
                    "isolated guest response is not bound to the accepted host request",
                ));
            }
            _ => {}
        }
        let sequence = self.outbound_sequence.checked_add(1).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated protocol sequence exhausted",
            )
        })?;
        let payload_len = serde_json::to_vec(&payload)
            .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?
            .len() as u64;
        let mut envelope = IsolatedVisualProtocolEnvelope {
            protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
            run_id: self.run_id.clone(),
            surface: self.surface.clone(),
            sequence,
            frame_sequence,
            input_sequence,
            request_nonce,
            payload_len,
            payload,
            authenticator_sha256: String::new(),
        };
        let unsigned = serde_json::to_vec(&envelope.unsigned())
            .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?;
        if unsigned.len() > ISOLATED_VISUAL_MAX_SIGNED_ENVELOPE_BYTES {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated protocol envelope exceeds its signed bound",
            ));
        }
        envelope.authenticator_sha256 = encode_hex(hmac::sign(&self.key, &unsigned).as_ref());
        self.outbound_sequence = sequence;
        self.outbound_frame_sequence = frame_sequence;
        match self.role {
            ProtocolRole::Host => {
                self.outstanding_request_nonce = Some(envelope.request_nonce.clone())
            }
            ProtocolRole::Guest => self.accepted_request_nonce = None,
        }
        Ok(envelope)
    }

    pub fn open(
        &mut self,
        envelope: IsolatedVisualProtocolEnvelope,
    ) -> ComputerResult<IsolatedVisualProtocolPayload> {
        if validate_digest(
            "isolated protocol authenticator",
            &envelope.authenticator_sha256,
        )
        .is_err()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated protocol authentication failed",
            ));
        }
        let authenticator = decode_hex(&envelope.authenticator_sha256).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated protocol authentication failed",
            )
        })?;
        let unsigned = serde_json::to_vec(&envelope.unsigned())
            .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?;
        if unsigned.len() > ISOLATED_VISUAL_MAX_SIGNED_ENVELOPE_BYTES
            || hmac::verify(&self.key, &unsigned, &authenticator).is_err()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated protocol authentication failed",
            ));
        }

        if envelope.protocol_version != ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION
            || envelope.run_id != self.run_id
            || envelope.surface != self.surface
            || envelope.payload.role() == self.role
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated protocol envelope is outside this channel binding",
            ));
        }
        envelope.surface.validate()?;
        envelope.payload.validate(&self.limits)?;
        validate_request_nonce(&envelope.request_nonce)?;
        let payload_len = serde_json::to_vec(&envelope.payload)
            .map_err(|error| ComputerError::new(ComputerErrorCode::Internal, error.to_string()))?
            .len() as u64;
        if envelope.payload_len != payload_len {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated protocol payload length is inconsistent",
            ));
        }
        if envelope.input_sequence != 0 {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "read-only isolated protocol does not accept input events",
            ));
        }
        let expected_sequence = self.inbound_sequence.checked_add(1).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated protocol sequence exhausted",
            )
        })?;
        if envelope.sequence != expected_sequence
            || envelope.frame_sequence < self.inbound_frame_sequence
            || (envelope.payload.is_frame()
                && envelope.frame_sequence == self.inbound_frame_sequence)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated protocol sequence is duplicate, skipped, or stale",
            ));
        }
        match self.role {
            ProtocolRole::Host
                if self.outstanding_request_nonce.as_deref()
                    != Some(envelope.request_nonce.as_str()) =>
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenTarget,
                    "isolated guest response does not match the outstanding host request",
                ));
            }
            ProtocolRole::Guest if self.accepted_request_nonce.is_some() => {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated protocol permits only one accepted request",
                ));
            }
            _ => {}
        }
        self.inbound_sequence = envelope.sequence;
        self.inbound_frame_sequence = envelope.frame_sequence;
        match self.role {
            ProtocolRole::Host => self.outstanding_request_nonce = None,
            ProtocolRole::Guest => self.accepted_request_nonce = Some(envelope.request_nonce),
        }
        Ok(envelope.payload)
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::computer_use::{
        IsolatedVisualManifest, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION, MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
    };

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: Uuid::new_v4().to_string(),
            surface: ComputerSurfaceBinding::issue(),
            input_domain_id: Uuid::new_v4().to_string(),
            manifest: IsolatedVisualManifest {
                schema_version: ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION,
                backend_id: MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID.into(),
                guest_protocol_version: ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
                helper_content_sha256: "a".repeat(64),
                helper_signing_requirement_sha256: "b".repeat(64),
                guest_image_sha256: "c".repeat(64),
                configuration_sha256: "d".repeat(64),
                security_profile: IsolatedVisualSecurityProfile::locked_down(),
                limits: IsolatedVisualResourceLimits::proof_defaults(),
            },
        }
    }

    fn observe() -> IsolatedVisualProtocolPayload {
        IsolatedVisualProtocolPayload::HostToGuest(IsolatedVisualHostMessage::Observe {
            maximum_frame_bytes: 1024,
            maximum_width: 800,
            maximum_height: 600,
        })
    }

    fn frame() -> IsolatedVisualProtocolPayload {
        IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Frame {
            content_sha256: "e".repeat(64),
            encoded_bytes: 1024,
            width: 800,
            height: 600,
        })
    }

    #[test]
    fn authenticated_read_only_round_trip_is_bound_and_monotonic() {
        let contract = contract();
        let secret = [7_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
        let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
        let mut guest = IsolatedVisualProtocolSession::new_guest(&contract, &secret).unwrap();

        let request_nonce = Uuid::new_v4().to_string();
        let request = host.seal(request_nonce.clone(), 0, 0, observe()).unwrap();
        assert_eq!(request.sequence, 1);
        assert_eq!(request.authenticator_sha256.len(), 64);
        assert_eq!(
            host.seal(Uuid::new_v4().to_string(), 0, 0, observe())
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        assert_eq!(guest.open(request).unwrap(), observe());

        assert_eq!(
            guest
                .seal(Uuid::new_v4().to_string(), 1, 0, frame())
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenTarget
        );
        let response = guest.seal(request_nonce.clone(), 1, 0, frame()).unwrap();
        assert_eq!(host.open(response).unwrap(), frame());

        let next_request = host
            .seal(Uuid::new_v4().to_string(), 1, 0, observe())
            .unwrap();
        assert_eq!(guest.open(next_request).unwrap(), observe());
        let debug = format!("{host:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&encode_hex(&secret)));
    }

    #[test]
    fn tampering_and_replay_fail_without_consuming_the_valid_message() {
        let contract = contract();
        let secret = [8_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
        let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
        let mut guest = IsolatedVisualProtocolSession::new_guest(&contract, &secret).unwrap();
        let valid = host
            .seal(Uuid::new_v4().to_string(), 0, 0, observe())
            .unwrap();
        let mut tampered = valid.clone();
        tampered.frame_sequence = 1;
        assert_eq!(
            guest.open(tampered).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        guest.open(valid.clone()).unwrap();
        assert_eq!(
            guest.open(valid).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn wrong_secret_binding_and_input_claims_fail_closed() {
        let contract = contract();
        let secret = [9_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
        let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
        let mut wrong_guest =
            IsolatedVisualProtocolSession::new_guest(&contract, &[10_u8; 32]).unwrap();
        let request = host
            .seal(Uuid::new_v4().to_string(), 0, 0, observe())
            .unwrap();
        assert_eq!(
            wrong_guest.open(request).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            host.seal(Uuid::new_v4().to_string(), 0, 1, observe())
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
        assert_eq!(
            IsolatedVisualProtocolSession::new_host(&contract, &[0_u8; 32])
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn malformed_or_oversized_frames_and_unknown_fields_are_rejected() {
        let contract = contract();
        let secret = [11_u8; ISOLATED_VISUAL_CHANNEL_SECRET_BYTES];
        let mut guest = IsolatedVisualProtocolSession::new_guest(&contract, &secret).unwrap();
        let mut invalid_frame = frame();
        if let IsolatedVisualProtocolPayload::GuestToHost(IsolatedVisualGuestMessage::Frame {
            encoded_bytes,
            ..
        }) = &mut invalid_frame
        {
            *encoded_bytes = contract.manifest.limits.encoded_frame_bytes + 1;
        }
        assert_eq!(
            guest
                .seal(Uuid::new_v4().to_string(), 1, 0, invalid_frame)
                .unwrap_err()
                .code,
            ComputerErrorCode::LimitReached
        );

        let mut host = IsolatedVisualProtocolSession::new_host(&contract, &secret).unwrap();
        let envelope = host
            .seal(Uuid::new_v4().to_string(), 0, 0, observe())
            .unwrap();
        let mut value = serde_json::to_value(envelope).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<IsolatedVisualProtocolEnvelope>(value).is_err());

        let mut nested = serde_json::to_value(
            IsolatedVisualProtocolSession::new_host(&contract, &secret)
                .unwrap()
                .seal(Uuid::new_v4().to_string(), 0, 0, observe())
                .unwrap(),
        )
        .unwrap();
        nested["surface"]
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<IsolatedVisualProtocolEnvelope>(nested).is_err());

        let mut nested = serde_json::to_value(
            IsolatedVisualProtocolSession::new_host(&contract, &secret)
                .unwrap()
                .seal(Uuid::new_v4().to_string(), 0, 0, observe())
                .unwrap(),
        )
        .unwrap();
        nested["payload"]["message"]
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<IsolatedVisualProtocolEnvelope>(nested).is_err());

        assert_eq!(
            IsolatedVisualProtocolSession::new_host(&contract, &secret)
                .unwrap()
                .seal("not-a-uuid".into(), 0, 0, observe())
                .unwrap_err()
                .code,
            ComputerErrorCode::InvalidRequest
        );
    }
}
