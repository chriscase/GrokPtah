use ring::hmac;
use uuid::Uuid;

use super::isolated_visual::{
    IsolatedVisualLaunchContract, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::isolated_visual_input::{
    IsolatedVisualInputGate, IsolatedVisualInputKeyState, IsolatedVisualInputMessage,
};
use super::types::{
    ComputerError, ComputerErrorCode, ComputerKey, ComputerResult, PointerButton,
    PointerButtonState,
};

pub const ISOLATED_VISUAL_INPUT_MAGIC: u32 = 0x4750_5441;
pub const ISOLATED_VISUAL_INPUT_VERSION: u16 = 1;
pub const ISOLATED_VISUAL_INPUT_HEADER_BYTES: usize = 64;
pub const ISOLATED_VISUAL_INPUT_TAG_BYTES: usize = 32;
pub const ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES: usize =
    ISOLATED_VISUAL_INPUT_HEADER_BYTES + 4 * 1024 + ISOLATED_VISUAL_INPUT_TAG_BYTES;
const ISOLATED_VISUAL_INPUT_CONTEXT: &[u8] = b"grokptah-isolated-visual-input-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputWireRole {
    HostSender,
    GuestReceiver,
}

/// Authenticated binary host-to-guest input transport. It is intentionally
/// separate from the model-facing JSON protocol and remains non-dispatchable
/// until a signed guest/runtime proof is available.
pub struct IsolatedVisualInputWire {
    role: InputWireRole,
    key: hmac::Key,
    run_id: String,
    surface_id: String,
    incarnation: String,
}

impl std::fmt::Debug for IsolatedVisualInputWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedVisualInputWire")
            .field("role", &self.role)
            .field("channel_secret", &"[REDACTED]")
            .finish()
    }
}

impl IsolatedVisualInputWire {
    pub fn new_host(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(InputWireRole::HostSender, contract, channel_secret)
    }

    pub fn new_guest(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(InputWireRole::GuestReceiver, contract, channel_secret)
    }

    /// Constructs the source-only host sender from the exact bootstrap
    /// challenge and identity binding; it cannot widen the current locked
    /// Computer Use capability by itself.
    pub fn new_host_with_challenge(
        contract: &IsolatedVisualLaunchContract,
        challenge: &[u8; 32],
    ) -> ComputerResult<Self> {
        let binding = IsolatedVisualChannelBinding::from_contract(contract)?;
        let channel_secret = binding.derive_channel_secret(challenge)?;
        Self::new(InputWireRole::HostSender, contract, &channel_secret)
    }

    /// Constructs the source-only guest receiver from the same challenge-bound
    /// key used by the host sender.
    pub fn new_guest_with_challenge(
        contract: &IsolatedVisualLaunchContract,
        challenge: &[u8; 32],
    ) -> ComputerResult<Self> {
        let binding = IsolatedVisualChannelBinding::from_contract(contract)?;
        let channel_secret = binding.derive_channel_secret(challenge)?;
        Self::new(InputWireRole::GuestReceiver, contract, &channel_secret)
    }

    fn new(
        role: InputWireRole,
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        contract.validate()?;
        if channel_secret.len() != 32 || channel_secret.iter().all(|byte| *byte == 0) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated input channel secret is missing or invalid",
            ));
        }
        Ok(Self {
            role,
            key: hmac::Key::new(hmac::HMAC_SHA256, channel_secret),
            run_id: contract.run_id.clone(),
            surface_id: contract.surface.surface_id().to_string(),
            incarnation: contract.surface.incarnation().to_string(),
        })
    }

    pub fn seal(
        &self,
        gate: &mut IsolatedVisualInputGate,
        frame_sequence: u64,
        input_sequence: u64,
        request_nonce: &str,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<Vec<u8>> {
        if self.role != InputWireRole::HostSender {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "only the isolated host may seal input packets",
            ));
        }
        validate_nonce(request_nonce)?;
        let packet = self.encode(frame_sequence, input_sequence, request_nonce, &message)?;
        gate.admit(frame_sequence, input_sequence, message)?;
        Ok(packet)
    }

    pub fn open(
        &self,
        gate: &mut IsolatedVisualInputGate,
        encoded: &[u8],
    ) -> ComputerResult<IsolatedVisualInputMessage> {
        if self.role != InputWireRole::GuestReceiver {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "only the isolated guest may open input packets",
            ));
        }
        let decoded = self.decode(encoded)?;
        gate.admit(
            decoded.frame_sequence,
            decoded.input_sequence,
            decoded.message.clone(),
        )?;
        Ok(decoded.message)
    }

    fn encode(
        &self,
        frame_sequence: u64,
        input_sequence: u64,
        request_nonce: &str,
        message: &IsolatedVisualInputMessage,
    ) -> ComputerResult<Vec<u8>> {
        let nonce = parse_nonce(request_nonce)?;
        let fields = WireFields::from_message(message)?;
        let mut packet = Vec::with_capacity(ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES);
        put_u32(&mut packet, ISOLATED_VISUAL_INPUT_MAGIC);
        put_u16(&mut packet, ISOLATED_VISUAL_INPUT_VERSION);
        put_u16(&mut packet, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16);
        put_u64(&mut packet, frame_sequence);
        put_u64(&mut packet, input_sequence);
        packet.extend_from_slice(nonce.as_bytes());
        packet.push(fields.kind);
        packet.push(fields.state);
        put_u16(&mut packet, fields.code);
        put_u32(&mut packet, fields.x);
        put_u32(&mut packet, fields.y);
        put_i32(&mut packet, fields.delta_x);
        put_i32(&mut packet, fields.delta_y);
        put_u32(&mut packet, fields.text.len() as u32);
        packet.extend_from_slice(&fields.text);
        if packet.len() != ISOLATED_VISUAL_INPUT_HEADER_BYTES + fields.text.len() {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "isolated input packet header size drifted",
            ));
        }
        let tag = hmac::sign(&self.key, &self.authentication_bytes(&packet));
        packet.extend_from_slice(tag.as_ref());
        Ok(packet)
    }

    fn decode(&self, encoded: &[u8]) -> ComputerResult<DecodedInput> {
        if encoded.len() < ISOLATED_VISUAL_INPUT_HEADER_BYTES + ISOLATED_VISUAL_INPUT_TAG_BYTES
            || encoded.len() > ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated input packet exceeds its bound",
            ));
        }
        let mut cursor = 0;
        let magic = take_u32(encoded, &mut cursor)?;
        let version = take_u16(encoded, &mut cursor)?;
        let protocol_version = take_u16(encoded, &mut cursor)?;
        if magic != ISOLATED_VISUAL_INPUT_MAGIC
            || version != ISOLATED_VISUAL_INPUT_VERSION
            || protocol_version != ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated input packet version or magic is unsupported",
            ));
        }
        let frame_sequence = take_u64(encoded, &mut cursor)?;
        let input_sequence = take_u64(encoded, &mut cursor)?;
        let request_nonce = Uuid::from_bytes(take_array::<16>(encoded, &mut cursor)?).to_string();
        validate_nonce(&request_nonce)?;
        let kind = take_u8(encoded, &mut cursor)?;
        let state = take_u8(encoded, &mut cursor)?;
        let code = take_u16(encoded, &mut cursor)?;
        let x = take_u32(encoded, &mut cursor)?;
        let y = take_u32(encoded, &mut cursor)?;
        let delta_x = take_i32(encoded, &mut cursor)?;
        let delta_y = take_i32(encoded, &mut cursor)?;
        let text_len = take_u32(encoded, &mut cursor)? as usize;
        if cursor != ISOLATED_VISUAL_INPUT_HEADER_BYTES
            || text_len > 4 * 1024
            || encoded.len()
                != ISOLATED_VISUAL_INPUT_HEADER_BYTES + text_len + ISOLATED_VISUAL_INPUT_TAG_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated input packet length is inconsistent",
            ));
        }
        let payload_end = cursor + text_len;
        hmac::verify(
            &self.key,
            &self.authentication_bytes(&encoded[..payload_end]),
            &encoded[payload_end..],
        )
        .map_err(|_| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated input packet authentication failed",
            )
        })?;
        let text = encoded[cursor..payload_end].to_vec();
        let message = WireFields {
            kind,
            state,
            code,
            x,
            y,
            delta_x,
            delta_y,
            text,
        }
        .into_message()?;
        Ok(DecodedInput {
            frame_sequence,
            input_sequence,
            message,
        })
    }

    fn authentication_bytes(&self, packet: &[u8]) -> Vec<u8> {
        let mut authenticated = Vec::with_capacity(
            ISOLATED_VISUAL_INPUT_CONTEXT.len()
                + self.run_id.len()
                + self.surface_id.len()
                + self.incarnation.len()
                + 12
                + packet.len(),
        );
        authenticated.extend_from_slice(ISOLATED_VISUAL_INPUT_CONTEXT);
        append_binding(&mut authenticated, &self.run_id);
        append_binding(&mut authenticated, &self.surface_id);
        append_binding(&mut authenticated, &self.incarnation);
        authenticated.extend_from_slice(packet);
        authenticated
    }
}

struct DecodedInput {
    frame_sequence: u64,
    input_sequence: u64,
    message: IsolatedVisualInputMessage,
}

struct WireFields {
    kind: u8,
    state: u8,
    code: u16,
    x: u32,
    y: u32,
    delta_x: i32,
    delta_y: i32,
    text: Vec<u8>,
}

impl WireFields {
    fn from_message(message: &IsolatedVisualInputMessage) -> ComputerResult<Self> {
        let fields = match message {
            IsolatedVisualInputMessage::PointerMove { x, y } => Self {
                kind: 1,
                state: 0,
                code: 0,
                x: *x,
                y: *y,
                delta_x: 0,
                delta_y: 0,
                text: Vec::new(),
            },
            IsolatedVisualInputMessage::PointerButton {
                x,
                y,
                button,
                state,
            } => Self {
                kind: 2,
                state: match state {
                    PointerButtonState::Down => 1,
                    PointerButtonState::Up => 2,
                },
                code: match button {
                    PointerButton::Primary => 1,
                    PointerButton::Secondary => 2,
                },
                x: *x,
                y: *y,
                delta_x: 0,
                delta_y: 0,
                text: Vec::new(),
            },
            IsolatedVisualInputMessage::Scroll { delta_x, delta_y } => Self {
                kind: 3,
                state: 0,
                code: 0,
                x: 0,
                y: 0,
                delta_x: *delta_x,
                delta_y: *delta_y,
                text: Vec::new(),
            },
            IsolatedVisualInputMessage::Key { key, state } => Self {
                kind: 4,
                state: match state {
                    IsolatedVisualInputKeyState::Down => 1,
                    IsolatedVisualInputKeyState::Up => 2,
                },
                code: key_code(*key),
                x: 0,
                y: 0,
                delta_x: 0,
                delta_y: 0,
                text: Vec::new(),
            },
            IsolatedVisualInputMessage::Text { text } => Self {
                kind: 5,
                state: 0,
                code: 0,
                x: 0,
                y: 0,
                delta_x: 0,
                delta_y: 0,
                text: text.as_bytes().to_vec(),
            },
        };
        if fields.text.len() > 4 * 1024 || fields.text.contains(&0) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated input text payload is invalid",
            ));
        }
        Ok(fields)
    }

    fn into_message(self) -> ComputerResult<IsolatedVisualInputMessage> {
        let Self {
            kind,
            state,
            code,
            x,
            y,
            delta_x,
            delta_y,
            text,
        } = self;
        let message = match kind {
            1 if state == 0 && code == 0 && delta_x == 0 && delta_y == 0 && text.is_empty() => {
                IsolatedVisualInputMessage::PointerMove { x, y }
            }
            2 if (state == 1 || state == 2)
                && (code == 1 || code == 2)
                && delta_x == 0
                && delta_y == 0
                && text.is_empty() =>
            {
                IsolatedVisualInputMessage::PointerButton {
                    x,
                    y,
                    button: if code == 1 {
                        PointerButton::Primary
                    } else {
                        PointerButton::Secondary
                    },
                    state: if state == 1 {
                        PointerButtonState::Down
                    } else {
                        PointerButtonState::Up
                    },
                }
            }
            3 if state == 0 && code == 0 && x == 0 && y == 0 && text.is_empty() => {
                IsolatedVisualInputMessage::Scroll { delta_x, delta_y }
            }
            4 if (state == 1 || state == 2)
                && x == 0
                && y == 0
                && delta_x == 0
                && delta_y == 0
                && text.is_empty() =>
            {
                let key = key_from_code(code).ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::InvalidRequest,
                        "isolated input key code is unknown",
                    )
                })?;
                IsolatedVisualInputMessage::Key {
                    key,
                    state: if state == 1 {
                        IsolatedVisualInputKeyState::Down
                    } else {
                        IsolatedVisualInputKeyState::Up
                    },
                }
            }
            5 if state == 0 && code == 0 && x == 0 && y == 0 && delta_x == 0 && delta_y == 0 => {
                IsolatedVisualInputMessage::Text {
                    text: String::from_utf8(text).map_err(|_| {
                        ComputerError::new(
                            ComputerErrorCode::InvalidRequest,
                            "isolated input text is not UTF-8",
                        )
                    })?,
                }
            }
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "isolated input packet contains invalid fields",
                ))
            }
        };
        Ok(message)
    }
}

fn key_code(key: ComputerKey) -> u16 {
    match key {
        ComputerKey::Enter => 1,
        ComputerKey::Escape => 2,
        ComputerKey::Tab => 3,
        ComputerKey::ArrowUp => 4,
        ComputerKey::ArrowDown => 5,
        ComputerKey::ArrowLeft => 6,
        ComputerKey::ArrowRight => 7,
        ComputerKey::Space => 8,
        ComputerKey::Backspace => 9,
        ComputerKey::Delete => 10,
        ComputerKey::Home => 11,
        ComputerKey::End => 12,
        ComputerKey::PageUp => 13,
        ComputerKey::PageDown => 14,
        ComputerKey::Shift => 15,
        ComputerKey::Control => 16,
        ComputerKey::Alt => 17,
        ComputerKey::Meta => 18,
    }
}

fn key_from_code(code: u16) -> Option<ComputerKey> {
    Some(match code {
        1 => ComputerKey::Enter,
        2 => ComputerKey::Escape,
        3 => ComputerKey::Tab,
        4 => ComputerKey::ArrowUp,
        5 => ComputerKey::ArrowDown,
        6 => ComputerKey::ArrowLeft,
        7 => ComputerKey::ArrowRight,
        8 => ComputerKey::Space,
        9 => ComputerKey::Backspace,
        10 => ComputerKey::Delete,
        11 => ComputerKey::Home,
        12 => ComputerKey::End,
        13 => ComputerKey::PageUp,
        14 => ComputerKey::PageDown,
        15 => ComputerKey::Shift,
        16 => ComputerKey::Control,
        17 => ComputerKey::Alt,
        18 => ComputerKey::Meta,
        _ => return None,
    })
}

fn validate_nonce(value: &str) -> ComputerResult<()> {
    let uuid = parse_nonce(value)?;
    if uuid.get_version_num() != 4 {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated input request nonce is not UUIDv4",
        ));
    }
    Ok(())
}

fn parse_nonce(value: &str) -> ComputerResult<Uuid> {
    let uuid = Uuid::parse_str(value).map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated input request nonce is not a UUID",
        )
    })?;
    if uuid.to_string() != value {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated input request nonce is not canonical",
        ));
    }
    Ok(uuid)
}

fn append_binding(bytes: &mut Vec<u8>, value: &str) {
    put_u32(bytes, value.len() as u32);
    bytes.extend_from_slice(value.as_bytes());
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn put_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn take_array<const N: usize>(bytes: &[u8], cursor: &mut usize) -> ComputerResult<[u8; N]> {
    let end = cursor.checked_add(N).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated input packet cursor overflow",
        )
    })?;
    let value = bytes.get(*cursor..end).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated input packet is truncated",
        )
    })?;
    let mut result = [0_u8; N];
    result.copy_from_slice(value);
    *cursor = end;
    Ok(result)
}

fn take_u8(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u8> {
    Ok(take_array::<1>(bytes, cursor)?[0])
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u16> {
    Ok(u16::from_be_bytes(take_array(bytes, cursor)?))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u32> {
    Ok(u32::from_be_bytes(take_array(bytes, cursor)?))
}

fn take_i32(bytes: &[u8], cursor: &mut usize) -> ComputerResult<i32> {
    Ok(i32::from_be_bytes(take_array(bytes, cursor)?))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u64> {
    Ok(u64::from_be_bytes(take_array(bytes, cursor)?))
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual::{
        IsolatedVisualManifest, IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION, MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
    };
    use super::super::types::ComputerSurfaceBinding;
    use super::*;

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: "opaque-input-wire-test".into(),
            surface: ComputerSurfaceBinding::issue(),
            input_domain_id: "guest-input-domain".into(),
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

    #[test]
    fn authenticated_input_round_trip_uses_the_gate() {
        let contract = contract();
        let secret = [3_u8; 32];
        let mut host_gate = IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        let mut guest_gate =
            IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        host_gate.bind_frame(4, 800, 600).unwrap();
        guest_gate.bind_frame(4, 800, 600).unwrap();
        let host = IsolatedVisualInputWire::new_host(&contract, &secret).unwrap();
        let guest = IsolatedVisualInputWire::new_guest(&contract, &secret).unwrap();
        let nonce = Uuid::new_v4().to_string();
        let message = IsolatedVisualInputMessage::Text {
            text: "世界".into(),
        };
        let encoded = host
            .seal(&mut host_gate, 4, 1, &nonce, message.clone())
            .unwrap();
        assert_eq!(guest.open(&mut guest_gate, &encoded).unwrap(), message);
        assert_eq!(host_gate.accepted_events(), 1);
        assert_eq!(guest_gate.next_input_sequence(), 1);
    }

    #[test]
    fn challenge_binding_derives_interoperable_input_keys() {
        let contract = contract();
        let challenge = [0x52_u8; 32];
        let mut host_gate = IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        let mut guest_gate =
            IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        host_gate.bind_frame(4, 800, 600).unwrap();
        guest_gate.bind_frame(4, 800, 600).unwrap();
        let host = IsolatedVisualInputWire::new_host_with_challenge(&contract, &challenge).unwrap();
        let guest =
            IsolatedVisualInputWire::new_guest_with_challenge(&contract, &challenge).unwrap();
        let nonce = Uuid::new_v4().to_string();
        let message = IsolatedVisualInputMessage::PointerMove { x: 42, y: 17 };
        let encoded = host
            .seal(&mut host_gate, 4, 1, &nonce, message.clone())
            .unwrap();
        assert_eq!(guest.open(&mut guest_gate, &encoded).unwrap(), message);
    }

    #[test]
    fn tamper_replay_and_reverse_direction_fail_closed() {
        let contract = contract();
        let secret = [4_u8; 32];
        let mut host_gate = IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        let mut guest_gate =
            IsolatedVisualInputGate::new(contract.manifest.limits.clone()).unwrap();
        host_gate.bind_frame(1, 800, 600).unwrap();
        guest_gate.bind_frame(1, 800, 600).unwrap();
        let host = IsolatedVisualInputWire::new_host(&contract, &secret).unwrap();
        let guest = IsolatedVisualInputWire::new_guest(&contract, &secret).unwrap();
        let encoded = host
            .seal(
                &mut host_gate,
                1,
                1,
                &Uuid::new_v4().to_string(),
                IsolatedVisualInputMessage::PointerMove { x: 1, y: 2 },
            )
            .unwrap();
        let mut tampered = encoded.clone();
        tampered[ISOLATED_VISUAL_INPUT_HEADER_BYTES] ^= 1;
        assert_eq!(
            guest.open(&mut guest_gate, &tampered).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        guest.open(&mut guest_gate, &encoded).unwrap();
        assert_eq!(
            guest.open(&mut guest_gate, &encoded).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
        assert_eq!(
            guest
                .seal(
                    &mut guest_gate,
                    1,
                    2,
                    &Uuid::new_v4().to_string(),
                    IsolatedVisualInputMessage::PointerMove { x: 2, y: 3 },
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
    }
}
