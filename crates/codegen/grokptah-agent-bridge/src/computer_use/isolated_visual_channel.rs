use ring::hmac;
use sha2::{Digest, Sha256};

use super::isolated_visual::{
    IsolatedVisualLaunchContract, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION,
};
use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

pub const ISOLATED_VISUAL_BINDING_MAGIC: u32 = 0x4750_5442;
pub const ISOLATED_VISUAL_BINDING_VERSION: u16 = 1;
pub const ISOLATED_VISUAL_BINDING_HEADER_BYTES: usize = 80;
pub const ISOLATED_VISUAL_BINDING_DIGEST_BYTES: usize = 32;
pub const ISOLATED_VISUAL_BINDING_TAG_BYTES: usize = 32;
pub const ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES: usize = 256;
pub const ISOLATED_VISUAL_BINDING_CONTEXT: &[u8] = b"grokptah-isolated-visual-binding-v1";
const ISOLATED_VISUAL_CHANNEL_CONTEXT: &[u8] = b"grokptah-isolated-visual-channel-v1";
const ISOLATED_VISUAL_CONFIRM_CONTEXT: &[u8] = b"grokptah-isolated-visual-channel-confirm-v1";

/// The exact identity that is hashed before any frame or input packet is
/// admitted. The four strings are length-prefixed in a fixed order so a
/// concatenation collision cannot move input between runs, surfaces, or
/// incarnations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedVisualChannelBinding {
    pub run_id: String,
    pub surface_id: String,
    pub incarnation: String,
    pub input_domain_id: String,
}

impl IsolatedVisualChannelBinding {
    pub fn from_contract(contract: &IsolatedVisualLaunchContract) -> ComputerResult<Self> {
        contract.validate()?;
        let binding = Self {
            run_id: contract.run_id.clone(),
            surface_id: contract.surface.surface_id().to_string(),
            incarnation: contract.surface.incarnation().to_string(),
            input_domain_id: contract.input_domain_id.clone(),
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> ComputerResult<()> {
        for (name, value) in [
            ("run_id", &self.run_id),
            ("surface_id", &self.surface_id),
            ("incarnation", &self.incarnation),
            ("input_domain_id", &self.input_domain_id),
        ] {
            if value.is_empty()
                || value.len() > ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES
                || value.as_bytes().contains(&0)
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    format!("invalid isolated visual binding {name}"),
                ));
            }
        }
        if self.input_domain_id == self.surface_id || self.input_domain_id == self.incarnation {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual input domain is not independent from its surface identity",
            ));
        }
        Ok(())
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let fields = [
            self.run_id.as_bytes(),
            self.surface_id.as_bytes(),
            self.incarnation.as_bytes(),
            self.input_domain_id.as_bytes(),
        ];
        let mut bytes = Vec::with_capacity(
            ISOLATED_VISUAL_BINDING_CONTEXT.len()
                + fields.iter().map(|field| 4 + field.len()).sum::<usize>(),
        );
        bytes.extend_from_slice(ISOLATED_VISUAL_BINDING_CONTEXT);
        for field in fields {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        bytes
    }

    pub fn digest(&self) -> ComputerResult<[u8; ISOLATED_VISUAL_BINDING_DIGEST_BYTES]> {
        self.validate()?;
        Ok(Sha256::digest(self.canonical_bytes()).into())
    }

    pub fn derive_channel_secret(
        &self,
        challenge: &[u8; 32],
    ) -> ComputerResult<[u8; ISOLATED_VISUAL_BINDING_DIGEST_BYTES]> {
        let digest = self.digest()?;
        let mut message = Vec::with_capacity(ISOLATED_VISUAL_CHANNEL_CONTEXT.len() + digest.len());
        message.extend_from_slice(ISOLATED_VISUAL_CHANNEL_CONTEXT);
        message.extend_from_slice(&digest);
        Ok(
            hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, challenge), &message)
                .as_ref()
                .try_into()
                .expect("HMAC-SHA256 is always 32 bytes"),
        )
    }

    pub fn confirmation_tag(
        &self,
        challenge: &[u8; 32],
    ) -> ComputerResult<[u8; ISOLATED_VISUAL_BINDING_TAG_BYTES]> {
        let digest = self.digest()?;
        let secret = self.derive_channel_secret(challenge)?;
        let mut message = Vec::with_capacity(ISOLATED_VISUAL_CONFIRM_CONTEXT.len() + digest.len());
        message.extend_from_slice(ISOLATED_VISUAL_CONFIRM_CONTEXT);
        message.extend_from_slice(&digest);
        Ok(
            hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &secret), &message)
                .as_ref()
                .try_into()
                .expect("HMAC-SHA256 is always 32 bytes"),
        )
    }

    /// The binding packet header is fixed-width; the four UTF-8 fields follow
    /// it in the same order used by [`digest`]. This encoder is source-only
    /// until the signed helper/guest runtime consumes the packet.
    pub fn encode_header_and_payload(&self, challenge: &[u8; 32]) -> ComputerResult<Vec<u8>> {
        self.validate()?;
        let digest = self.digest()?;
        let confirmation = self.confirmation_tag(challenge)?;
        let fields = [
            self.run_id.as_bytes(),
            self.surface_id.as_bytes(),
            self.incarnation.as_bytes(),
            self.input_domain_id.as_bytes(),
        ];
        let payload_bytes = fields.iter().map(|field| field.len()).sum::<usize>();
        let mut packet = Vec::with_capacity(ISOLATED_VISUAL_BINDING_HEADER_BYTES + payload_bytes);
        put_u32(&mut packet, ISOLATED_VISUAL_BINDING_MAGIC);
        put_u16(&mut packet, ISOLATED_VISUAL_BINDING_VERSION);
        put_u16(&mut packet, ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16);
        for field in fields {
            put_u16(&mut packet, field.len() as u16);
        }
        packet.extend_from_slice(&digest);
        packet.extend_from_slice(&confirmation);
        for field in fields {
            packet.extend_from_slice(field);
        }
        debug_assert_eq!(
            packet.len(),
            ISOLATED_VISUAL_BINDING_HEADER_BYTES + payload_bytes
        );
        Ok(packet)
    }

    /// Decodes and authenticates the fixed binding packet on the guest side.
    /// The challenge is never serialized into the packet; a wrong challenge,
    /// changed identity, or changed confirmation fails closed.
    pub fn decode_header_and_payload(packet: &[u8], challenge: &[u8; 32]) -> ComputerResult<Self> {
        if packet.len() < ISOLATED_VISUAL_BINDING_HEADER_BYTES
            || packet.len()
                > ISOLATED_VISUAL_BINDING_HEADER_BYTES + 4 * ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated visual binding packet exceeds its bound",
            ));
        }
        if read_u32(packet, 0)? != ISOLATED_VISUAL_BINDING_MAGIC
            || read_u16(packet, 4)? != ISOLATED_VISUAL_BINDING_VERSION
            || read_u16(packet, 6)? != ISOLATED_VISUAL_GUEST_PROTOCOL_VERSION as u16
        {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated visual binding version or magic is unsupported",
            ));
        }
        let lengths = [
            read_u16(packet, 8)? as usize,
            read_u16(packet, 10)? as usize,
            read_u16(packet, 12)? as usize,
            read_u16(packet, 14)? as usize,
        ];
        if lengths
            .iter()
            .any(|length| *length == 0 || *length > ISOLATED_VISUAL_BINDING_MAX_FIELD_BYTES)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual binding field exceeds its bound",
            ));
        }
        let payload_bytes = lengths.iter().sum::<usize>();
        if packet.len() != ISOLATED_VISUAL_BINDING_HEADER_BYTES + payload_bytes {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual binding packet length is inconsistent",
            ));
        }
        let payload = &packet[ISOLATED_VISUAL_BINDING_HEADER_BYTES..];
        let mut offset = 0;
        let mut fields = Vec::with_capacity(4);
        for (index, length) in lengths.into_iter().enumerate() {
            let end = offset + length;
            let field = std::str::from_utf8(&payload[offset..end]).map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    format!("isolated visual binding field {index} is not UTF-8"),
                )
            })?;
            fields.push(field.to_owned());
            offset = end;
        }
        let binding = Self {
            run_id: fields.remove(0),
            surface_id: fields.remove(0),
            incarnation: fields.remove(0),
            input_domain_id: fields.remove(0),
        };
        let digest = binding.digest()?;
        if !constant_time_eq(&digest, &packet[16..48]) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated visual binding digest does not match its identities",
            ));
        }
        let confirmation = binding.confirmation_tag(challenge)?;
        if !constant_time_eq(&confirmation, &packet[48..80]) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated visual binding confirmation failed",
            ));
        }
        Ok(binding)
    }
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn read_u16(packet: &[u8], offset: usize) -> ComputerResult<u16> {
    let end = offset.checked_add(2).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated visual binding header offset overflow",
        )
    })?;
    let bytes = packet.get(offset..end).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated visual binding header is truncated",
        )
    })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(packet: &[u8], offset: usize) -> ComputerResult<u32> {
    let end = offset.checked_add(4).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated visual binding header offset overflow",
        )
    })?;
    let bytes = packet.get(offset..end).ok_or_else(|| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "isolated visual binding header is truncated",
        )
    })?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_binding_vector_matches_freestanding_guest() {
        let binding = IsolatedVisualChannelBinding {
            run_id: "run-1".into(),
            surface_id: "surface-1".into(),
            incarnation: "incarnation-1".into(),
            input_domain_id: "domain-1".into(),
        };
        assert_eq!(
            binding.digest().unwrap(),
            [
                0xbb, 0xfa, 0x00, 0x3d, 0x74, 0xa3, 0x72, 0xbc, 0xf2, 0x33, 0xbe, 0xe0, 0x74, 0x5f,
                0x75, 0xd5, 0xa7, 0xf2, 0x55, 0x19, 0xac, 0xd9, 0xfb, 0x8e, 0xdb, 0x14, 0x18, 0xfe,
                0xec, 0xfa, 0x3d, 0x5c,
            ]
        );
        let packet = binding.encode_header_and_payload(&[0; 32]).unwrap();
        assert_eq!(&packet[..4], &ISOLATED_VISUAL_BINDING_MAGIC.to_be_bytes());
        // run-1 (5) + surface-1 (9) + incarnation-1 (13) + domain-1 (8).
        assert_eq!(
            packet.len(),
            ISOLATED_VISUAL_BINDING_HEADER_BYTES + 5 + 9 + 13 + 8
        );
        assert_eq!(
            IsolatedVisualChannelBinding::decode_header_and_payload(&packet, &[0; 32]).unwrap(),
            binding
        );
        let mut tampered = packet.clone();
        tampered[16] ^= 1;
        assert_eq!(
            IsolatedVisualChannelBinding::decode_header_and_payload(&tampered, &[0; 32])
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            IsolatedVisualChannelBinding::decode_header_and_payload(&packet, &[1; 32])
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }
}
