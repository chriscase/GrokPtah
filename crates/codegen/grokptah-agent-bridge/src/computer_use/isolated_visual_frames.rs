use ring::hmac;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::isolated_visual::{
    IsolatedVisualLaunchContract, IsolatedVisualResourceLimits,
    ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
};
use super::isolated_visual_channel::IsolatedVisualChannelBinding;
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

pub const ISOLATED_VISUAL_FRAME_MAGIC: u32 = 0x4750_5446;
pub const ISOLATED_VISUAL_FRAME_VERSION: u16 = 1;
pub const ISOLATED_VISUAL_FRAME_HEADER_BYTES: usize = 100;
pub const ISOLATED_VISUAL_FRAME_TAG_BYTES: usize = 32;
pub const ISOLATED_VISUAL_FRAME_CHUNK_BYTES: usize = 64 * 1024;
const ISOLATED_VISUAL_FRAME_CONTEXT: &[u8] = b"grokptah-isolated-visual-frame-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameCarrierRole {
    GuestSender,
    HostReceiver,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedVisualFrameChunk {
    pub frame_sequence: u64,
    pub request_nonce: String,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub total_bytes: u64,
    pub offset: u64,
    pub width: u32,
    pub height: u32,
    pub content_sha256: [u8; 32],
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedVisualFrame {
    pub frame_sequence: u64,
    pub request_nonce: String,
    pub width: u32,
    pub height: u32,
    pub content_sha256: [u8; 32],
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ReceivingFrame {
    frame_sequence: u64,
    request_nonce: String,
    chunk_count: u32,
    next_chunk_index: u32,
    total_bytes: u64,
    next_offset: u64,
    width: u32,
    height: u32,
    content_sha256: [u8; 32],
    bytes: Vec<u8>,
}

/// Authenticated, bounded guest-frame carrier. It is intentionally separate
/// from the model-facing JSON envelope: frame bytes never enter projections,
/// audit text, or provider traffic. The caller still needs the corresponding
/// signed `Frame` metadata envelope before admitting these chunks.
pub struct IsolatedVisualFrameCarrier {
    role: FrameCarrierRole,
    key: hmac::Key,
    run_id: String,
    surface_id: String,
    incarnation: String,
    limits: IsolatedVisualResourceLimits,
    inbound: Option<ReceivingFrame>,
    last_frame_sequence: u64,
}

impl std::fmt::Debug for IsolatedVisualFrameCarrier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedVisualFrameCarrier")
            .field("role", &self.role)
            .field("last_frame_sequence", &self.last_frame_sequence)
            .field("channel_secret", &"[REDACTED]")
            .finish()
    }
}

impl IsolatedVisualFrameCarrier {
    pub fn new_guest(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(FrameCarrierRole::GuestSender, contract, channel_secret)
    }

    pub fn new_host(
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        Self::new(FrameCarrierRole::HostReceiver, contract, channel_secret)
    }

    /// Constructs the source-only guest carrier from the authenticated
    /// bootstrap challenge and exact session identity. Runtime dispatch still
    /// requires the signed helper/guest proof described in the roadmap.
    pub fn new_guest_with_challenge(
        contract: &IsolatedVisualLaunchContract,
        challenge: &[u8; 32],
    ) -> ComputerResult<Self> {
        let binding = IsolatedVisualChannelBinding::from_contract(contract)?;
        let channel_secret = binding.derive_channel_secret(challenge)?;
        Self::new(FrameCarrierRole::GuestSender, contract, &channel_secret)
    }

    /// Constructs the source-only host carrier from the same challenge-bound
    /// session key the guest derives after binding.
    pub fn new_host_with_challenge(
        contract: &IsolatedVisualLaunchContract,
        challenge: &[u8; 32],
    ) -> ComputerResult<Self> {
        let binding = IsolatedVisualChannelBinding::from_contract(contract)?;
        let channel_secret = binding.derive_channel_secret(challenge)?;
        Self::new(FrameCarrierRole::HostReceiver, contract, &channel_secret)
    }

    fn new(
        role: FrameCarrierRole,
        contract: &IsolatedVisualLaunchContract,
        channel_secret: &[u8],
    ) -> ComputerResult<Self> {
        contract.validate()?;
        if channel_secret.len() != 32 || channel_secret.iter().all(|byte| *byte == 0) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated frame channel secret is missing or invalid",
            ));
        }
        Ok(Self {
            role,
            key: hmac::Key::new(hmac::HMAC_SHA256, channel_secret),
            run_id: contract.run_id.clone(),
            surface_id: contract.surface.surface_id().to_string(),
            incarnation: contract.surface.incarnation().to_string(),
            limits: contract.manifest.limits.clone(),
            inbound: None,
            last_frame_sequence: 0,
        })
    }

    pub fn seal_frame(
        &mut self,
        frame_sequence: u64,
        request_nonce: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> ComputerResult<Vec<Vec<u8>>> {
        if self.role != FrameCarrierRole::GuestSender {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "only the isolated guest may seal frame chunks",
            ));
        }
        validate_request_nonce(request_nonce)?;
        self.validate_dimensions(width, height)?;
        self.validate_frame_size(bytes.len())?;
        if frame_sequence == 0 || frame_sequence <= self.last_frame_sequence {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated frame sequence is stale",
            ));
        }
        let content_sha256: [u8; 32] = Sha256::digest(bytes).into();
        let chunk_count = u32::try_from(bytes.len().div_ceil(ISOLATED_VISUAL_FRAME_CHUNK_BYTES))
            .map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    "isolated frame chunk count exceeds its bound",
                )
            })?;
        let mut chunks = Vec::with_capacity(chunk_count as usize);
        for chunk_index in 0..chunk_count {
            let start = chunk_index as usize * ISOLATED_VISUAL_FRAME_CHUNK_BYTES;
            let end = (start + ISOLATED_VISUAL_FRAME_CHUNK_BYTES).min(bytes.len());
            let chunk = IsolatedVisualFrameChunk {
                frame_sequence,
                request_nonce: request_nonce.into(),
                chunk_index,
                chunk_count,
                total_bytes: bytes.len() as u64,
                offset: start as u64,
                width,
                height,
                content_sha256,
                bytes: bytes[start..end].to_vec(),
            };
            chunks.push(self.encode_chunk(&chunk)?);
        }
        self.last_frame_sequence = frame_sequence;
        Ok(chunks)
    }

    pub fn open_chunk(&mut self, encoded: &[u8]) -> ComputerResult<Option<IsolatedVisualFrame>> {
        if self.role != FrameCarrierRole::HostReceiver {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "only the isolated host may open frame chunks",
            ));
        }
        let chunk = self.decode_chunk(encoded)?;
        self.validate_dimensions(chunk.width, chunk.height)?;
        self.validate_frame_size(chunk.total_bytes as usize)?;
        if chunk.chunk_count == 0
            || chunk.chunk_count as usize
                > self.limits.encoded_frame_bytes as usize / ISOLATED_VISUAL_FRAME_CHUNK_BYTES + 1
            || chunk.chunk_index >= chunk.chunk_count
            || chunk.bytes.is_empty()
            || chunk.bytes.len() > ISOLATED_VISUAL_FRAME_CHUNK_BYTES
            || chunk.offset.checked_add(chunk.bytes.len() as u64) > Some(chunk.total_bytes)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame chunk exceeds its bound",
            ));
        }
        if let Some(receiving) = self.inbound.as_mut() {
            if receiving.frame_sequence != chunk.frame_sequence
                || receiving.request_nonce != chunk.request_nonce
                || receiving.chunk_count != chunk.chunk_count
                || receiving.total_bytes != chunk.total_bytes
                || receiving.width != chunk.width
                || receiving.height != chunk.height
                || receiving.content_sha256 != chunk.content_sha256
                || receiving.next_chunk_index != chunk.chunk_index
                || receiving.next_offset != chunk.offset
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "isolated frame chunk is reordered or mismatched",
                ));
            }
            receiving.bytes.extend_from_slice(&chunk.bytes);
            receiving.next_chunk_index =
                receiving.next_chunk_index.checked_add(1).ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "isolated frame chunk sequence exhausted",
                    )
                })?;
            receiving.next_offset = receiving
                .next_offset
                .checked_add(chunk.bytes.len() as u64)
                .ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "isolated frame offset overflow",
                    )
                })?;
            if receiving.next_chunk_index != receiving.chunk_count {
                return Ok(None);
            }
        } else {
            if chunk.frame_sequence <= self.last_frame_sequence
                || chunk.chunk_index != 0
                || chunk.offset != 0
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "isolated frame sequence is stale or starts with a gap",
                ));
            }
            let mut bytes = Vec::with_capacity(chunk.total_bytes as usize);
            bytes.extend_from_slice(&chunk.bytes);
            self.inbound = Some(ReceivingFrame {
                frame_sequence: chunk.frame_sequence,
                request_nonce: chunk.request_nonce,
                chunk_count: chunk.chunk_count,
                next_chunk_index: 1,
                total_bytes: chunk.total_bytes,
                next_offset: chunk.bytes.len() as u64,
                width: chunk.width,
                height: chunk.height,
                content_sha256: chunk.content_sha256,
                bytes,
            });
            if chunk.chunk_count != 1 {
                return Ok(None);
            }
        }
        let Some(receiving) = self.inbound.take() else {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "isolated frame assembly state disappeared",
            ));
        };
        self.last_frame_sequence = receiving.frame_sequence;
        let digest: [u8; 32] = Sha256::digest(&receiving.bytes).into();
        if receiving.bytes.len() as u64 != receiving.total_bytes
            || digest != receiving.content_sha256
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated frame digest does not match its authenticated bytes",
            ));
        }
        Ok(Some(IsolatedVisualFrame {
            frame_sequence: receiving.frame_sequence,
            request_nonce: receiving.request_nonce,
            width: receiving.width,
            height: receiving.height,
            content_sha256: receiving.content_sha256,
            bytes: receiving.bytes,
        }))
    }

    fn validate_dimensions(&self, width: u32, height: u32) -> ComputerResult<()> {
        if width == 0
            || width > self.limits.display_width
            || height == 0
            || height > self.limits.display_height
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame dimensions exceed the measured manifest",
            ));
        }
        Ok(())
    }

    fn validate_frame_size(&self, bytes: usize) -> ComputerResult<()> {
        if bytes == 0 || bytes as u64 > self.limits.encoded_frame_bytes {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame bytes exceed the measured manifest",
            ));
        }
        Ok(())
    }

    fn encode_chunk(&self, chunk: &IsolatedVisualFrameChunk) -> ComputerResult<Vec<u8>> {
        let nonce = parse_uuid("request_nonce", &chunk.request_nonce)?;
        if chunk.bytes.is_empty()
            || chunk.bytes.len() > ISOLATED_VISUAL_FRAME_CHUNK_BYTES
            || chunk.chunk_count == 0
            || chunk.chunk_index >= chunk.chunk_count
            || chunk.offset.checked_add(chunk.bytes.len() as u64) > Some(chunk.total_bytes)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame chunk is outside its bounds",
            ));
        }
        let mut bytes = Vec::with_capacity(
            ISOLATED_VISUAL_FRAME_HEADER_BYTES
                + chunk.bytes.len()
                + ISOLATED_VISUAL_FRAME_TAG_BYTES,
        );
        put_u32(&mut bytes, ISOLATED_VISUAL_FRAME_MAGIC);
        put_u16(&mut bytes, ISOLATED_VISUAL_FRAME_VERSION);
        put_u16(&mut bytes, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16);
        put_u64(&mut bytes, chunk.frame_sequence);
        bytes.extend_from_slice(&nonce);
        put_u32(&mut bytes, chunk.chunk_index);
        put_u32(&mut bytes, chunk.chunk_count);
        put_u64(&mut bytes, chunk.total_bytes);
        put_u64(&mut bytes, chunk.offset);
        put_u32(&mut bytes, chunk.width);
        put_u32(&mut bytes, chunk.height);
        bytes.extend_from_slice(&chunk.content_sha256);
        put_u32(&mut bytes, chunk.bytes.len() as u32);
        bytes.extend_from_slice(&chunk.bytes);
        let tag = hmac::sign(&self.key, &self.authentication_bytes(&bytes));
        bytes.extend_from_slice(tag.as_ref());
        Ok(bytes)
    }

    fn decode_chunk(&self, encoded: &[u8]) -> ComputerResult<IsolatedVisualFrameChunk> {
        if encoded.len() < ISOLATED_VISUAL_FRAME_HEADER_BYTES + ISOLATED_VISUAL_FRAME_TAG_BYTES
            || encoded.len()
                > ISOLATED_VISUAL_FRAME_HEADER_BYTES
                    + ISOLATED_VISUAL_FRAME_CHUNK_BYTES
                    + ISOLATED_VISUAL_FRAME_TAG_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame packet exceeds its bound",
            ));
        }
        let mut cursor = 0;
        let magic = take_u32(encoded, &mut cursor)?;
        let version = take_u16(encoded, &mut cursor)?;
        let protocol_version = take_u16(encoded, &mut cursor)?;
        if magic != ISOLATED_VISUAL_FRAME_MAGIC
            || version != ISOLATED_VISUAL_FRAME_VERSION
            || protocol_version != ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated frame packet version or magic is unsupported",
            ));
        }
        let frame_sequence = take_u64(encoded, &mut cursor)?;
        let request_nonce = Uuid::from_bytes(take_array::<16>(encoded, &mut cursor)?).to_string();
        validate_request_nonce(&request_nonce)?;
        let chunk_index = take_u32(encoded, &mut cursor)?;
        let chunk_count = take_u32(encoded, &mut cursor)?;
        let total_bytes = take_u64(encoded, &mut cursor)?;
        let offset = take_u64(encoded, &mut cursor)?;
        let width = take_u32(encoded, &mut cursor)?;
        let height = take_u32(encoded, &mut cursor)?;
        let content_sha256 = take_array::<32>(encoded, &mut cursor)?;
        let chunk_bytes = take_u32(encoded, &mut cursor)? as usize;
        if cursor != ISOLATED_VISUAL_FRAME_HEADER_BYTES
            || chunk_bytes == 0
            || chunk_bytes > ISOLATED_VISUAL_FRAME_CHUNK_BYTES
            || encoded.len()
                != ISOLATED_VISUAL_FRAME_HEADER_BYTES
                    + chunk_bytes
                    + ISOLATED_VISUAL_FRAME_TAG_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated frame packet length is inconsistent",
            ));
        }
        let payload_end = cursor + chunk_bytes;
        let payload = encoded[cursor..payload_end].to_vec();
        let tag = &encoded[payload_end..];
        hmac::verify(
            &self.key,
            &self.authentication_bytes(&encoded[..payload_end]),
            tag,
        )
        .map_err(|_| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated frame packet authentication failed",
            )
        })?;
        Ok(IsolatedVisualFrameChunk {
            frame_sequence,
            request_nonce,
            chunk_index,
            chunk_count,
            total_bytes,
            offset,
            width,
            height,
            content_sha256,
            bytes: payload,
        })
    }

    fn authentication_bytes(&self, packet: &[u8]) -> Vec<u8> {
        let mut authenticated = Vec::with_capacity(
            ISOLATED_VISUAL_FRAME_CONTEXT.len()
                + self.run_id.len()
                + self.surface_id.len()
                + self.incarnation.len()
                + 12
                + packet.len(),
        );
        authenticated.extend_from_slice(ISOLATED_VISUAL_FRAME_CONTEXT);
        append_binding(&mut authenticated, &self.run_id);
        append_binding(&mut authenticated, &self.surface_id);
        append_binding(&mut authenticated, &self.incarnation);
        authenticated.extend_from_slice(packet);
        authenticated
    }
}

fn append_binding(bytes: &mut Vec<u8>, value: &str) {
    put_u32(bytes, value.len() as u32);
    bytes.extend_from_slice(value.as_bytes());
}

fn parse_uuid(name: &str, value: &str) -> ComputerResult<[u8; 16]> {
    let uuid = Uuid::parse_str(value).map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("isolated frame {name} is not a UUID"),
        )
    })?;
    if uuid.to_string() != value {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("isolated frame {name} is not canonical"),
        ));
    }
    Ok(*uuid.as_bytes())
}

fn validate_request_nonce(value: &str) -> ComputerResult<()> {
    let uuid = parse_uuid("request nonce", value)?;
    if Uuid::from_bytes(uuid).get_version_num() != 4 {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated frame request nonce is not UUIDv4",
        ));
    }
    Ok(())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn take_array<const N: usize>(bytes: &[u8], cursor: &mut usize) -> ComputerResult<[u8; N]> {
    let end = cursor.checked_add(N).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated frame packet cursor overflow",
        )
    })?;
    let value = bytes.get(*cursor..end).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated frame packet is truncated",
        )
    })?;
    let mut result = [0_u8; N];
    result.copy_from_slice(value);
    *cursor = end;
    Ok(result)
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u16> {
    Ok(u16::from_be_bytes(take_array(bytes, cursor)?))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u32> {
    Ok(u32::from_be_bytes(take_array(bytes, cursor)?))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> ComputerResult<u64> {
    Ok(u64::from_be_bytes(take_array(bytes, cursor)?))
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual::{
        IsolatedVisualManifest, IsolatedVisualSecurityProfile,
        ISOLATED_VISUAL_MANIFEST_SCHEMA_VERSION, MACOS_ISOLATED_VISUAL_CANDIDATE_BACKEND_ID,
    };
    use super::super::types::ComputerSurfaceBinding;
    use super::*;

    fn contract() -> IsolatedVisualLaunchContract {
        IsolatedVisualLaunchContract {
            run_id: "run-opaque-frame-test".into(),
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
                limits: super::super::isolated_visual::IsolatedVisualResourceLimits::proof_defaults(
                ),
            },
        }
    }

    #[test]
    fn frame_chunks_round_trip_and_digest() {
        let contract = contract();
        let secret = [7_u8; 32];
        let mut guest = IsolatedVisualFrameCarrier::new_guest(&contract, &secret).unwrap();
        let mut host = IsolatedVisualFrameCarrier::new_host(&contract, &secret).unwrap();
        let nonce = Uuid::new_v4().to_string();
        let payload = vec![0x5a; ISOLATED_VISUAL_FRAME_CHUNK_BYTES + 17];
        let chunks = guest.seal_frame(1, &nonce, 800, 600, &payload).unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(host.open_chunk(&chunks[0]).unwrap().is_none());
        let frame = host.open_chunk(&chunks[1]).unwrap().unwrap();
        assert_eq!(frame.bytes, payload);
        assert_eq!(frame.request_nonce, nonce);
    }

    #[test]
    fn challenge_binding_derives_interoperable_frame_keys() {
        let contract = contract();
        let challenge = [0x42_u8; 32];
        let mut guest =
            IsolatedVisualFrameCarrier::new_guest_with_challenge(&contract, &challenge).unwrap();
        let mut host =
            IsolatedVisualFrameCarrier::new_host_with_challenge(&contract, &challenge).unwrap();
        let chunks = guest
            .seal_frame(1, &Uuid::new_v4().to_string(), 800, 600, b"bound-frame")
            .unwrap();
        assert_eq!(
            host.open_chunk(&chunks[0]).unwrap().unwrap().bytes,
            b"bound-frame"
        );
    }

    #[test]
    fn tamper_replay_and_reorder_fail_closed() {
        let contract = contract();
        let secret = [8_u8; 32];
        let mut guest = IsolatedVisualFrameCarrier::new_guest(&contract, &secret).unwrap();
        let mut host = IsolatedVisualFrameCarrier::new_host(&contract, &secret).unwrap();
        let nonce = Uuid::new_v4().to_string();
        let payload = vec![0x21; ISOLATED_VISUAL_FRAME_CHUNK_BYTES + 1];
        let chunks = guest.seal_frame(1, &nonce, 800, 600, &payload).unwrap();
        assert_eq!(
            host.open_chunk(&chunks[1]).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
        let mut tampered = chunks[0].clone();
        tampered[ISOLATED_VISUAL_FRAME_HEADER_BYTES] ^= 1;
        assert_eq!(
            host.open_chunk(&tampered).unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
        host.open_chunk(&chunks[0]).unwrap();
        host.open_chunk(&chunks[1]).unwrap();
        assert_eq!(
            host.open_chunk(&chunks[0]).unwrap_err().code,
            ComputerErrorCode::StaleObservation
        );
    }

    #[test]
    fn host_and_guest_roles_are_not_interchangeable() {
        let contract = contract();
        let secret = [9_u8; 32];
        let mut host = IsolatedVisualFrameCarrier::new_host(&contract, &secret).unwrap();
        assert_eq!(
            host.seal_frame(1, &Uuid::new_v4().to_string(), 800, 600, &[1])
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );
    }
}
