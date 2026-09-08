//! Content-addressed captured-frame evidence for Contained Browser.
//!
//! Digests are computed from the exact bounded capture bytes supplied by the
//! frame source. They are never derived from labels, booleans, paths, or
//! caller-provided digest claims. Raw payload bytes never enter public/sealed
//! evidence, logs, projections, snapshots, or error strings.
//!
//! The default simulator may emit deterministic synthetic payload bytes for
//! CI. Those bytes are labeled [`CapturedFrameSource::SyntheticSimulator`] and
//! do **not** constitute a real browser capture or isolation PASS.
//!
//! The optional `browser-engine` path stays fail-closed in this slice: no
//! engine receipt, captured bytes, native/physical marker, or PASS is
//! fabricated.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{HarnessError, HarnessResult};
use crate::simulator::GuestFrame;

/// Maximum admitted captured-frame payload size.
pub const MAX_CAPTURED_FRAME_BYTES: usize = 1_048_576;

/// Logical width of the deterministic simulator payload.
pub const SYNTHETIC_FRAME_WIDTH: u32 = 32;

/// Logical height of the deterministic simulator payload.
pub const SYNTHETIC_FRAME_HEIGHT: u32 = 16;

/// Distinctive bytes embedded only in the private synthetic payload.
/// Public/sealed JSON, snapshots, and errors must never contain this needle.
pub const SYNTHETIC_FRAME_PAYLOAD_NEEDLE: &[u8] = b"raw-synthetic-frame-pixels-must-not-serialize";

const SYNTHETIC_FRAME_MAGIC: &[u8] = b"GROKPTAH-SYNTHETIC-BROWSER-FRAME-v1\0";
const CANONICAL_DIGEST_PREFIX: &str = "sha256:";
const CANONICAL_HEX_LEN: usize = 64;

/// Who produced the captured bytes. Must never be upgraded at seal time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedFrameSource {
    /// In-process simulator produced explicit synthetic payload bytes.
    /// Not a real browser capture and not isolation PASS.
    SyntheticSimulator,
    /// Reserved for a real isolated browser-engine capture. Unwired here.
    BrowserEngine,
}

/// Media/source classification for admitted payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedFrameMediaKind {
    /// Explicit synthetic CI payload. Not a PNG/RGBA engine raster claim.
    SyntheticPayload,
    /// Reserved for engine-captured RGBA8. Unwired in this slice.
    EngineRgba8,
}

/// Public/sealed captured-frame metadata. Never contains raw bytes or paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedFrameEvidence {
    pub epoch: u64,
    /// Canonical `sha256:` + 64 lowercase hex, computed from capture bytes only.
    pub digest: String,
    pub byte_length: usize,
    pub source: CapturedFrameSource,
    pub media_kind: CapturedFrameMediaKind,
    pub width: u32,
    pub height: u32,
}

/// Before/after public frame evidence sealed with a completed postcondition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedFramePair {
    pub before: CapturedFrameEvidence,
    pub after: CapturedFrameEvidence,
}

/// Private bounded capture. Bytes are never serialized.
#[derive(Clone)]
pub struct BoundedCapturedFrame {
    epoch: u64,
    bytes: Vec<u8>,
    source: CapturedFrameSource,
    media_kind: CapturedFrameMediaKind,
    width: u32,
    height: u32,
}

impl fmt::Debug for BoundedCapturedFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedCapturedFrame")
            .field("epoch", &self.epoch)
            .field("byte_length", &self.bytes.len())
            .field("source", &self.source)
            .field("media_kind", &self.media_kind)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("digest", &self.digest())
            .finish()
    }
}

impl BoundedCapturedFrame {
    /// Admit bytes from a capture/frame source. Digest is always recomputed
    /// from those bytes; callers cannot supply a digest claim.
    pub fn admit_from_source(
        epoch: u64,
        bytes: Vec<u8>,
        source: CapturedFrameSource,
        media_kind: CapturedFrameMediaKind,
        width: u32,
        height: u32,
    ) -> HarnessResult<Self> {
        if source == CapturedFrameSource::BrowserEngine
            || media_kind == CapturedFrameMediaKind::EngineRgba8
        {
            return Err(engine_capture_unwired_error());
        }
        if epoch == 0 {
            return Err(HarnessError::invalid_state(
                "captured-frame epoch must be bound to a booted frame",
            ));
        }
        validate_admission_bounds(&bytes, source, media_kind, width, height)?;
        Ok(Self {
            epoch,
            bytes,
            source,
            media_kind,
            width,
            height,
        })
    }

    /// Deterministic simulator capture for the in-process Contained Browser.
    pub fn admit_simulator_capture(epoch: u64, guest_link_clicked: bool) -> HarnessResult<Self> {
        Self::admit_from_source(
            epoch,
            simulator_synthetic_frame_bytes(guest_link_clicked),
            CapturedFrameSource::SyntheticSimulator,
            CapturedFrameMediaKind::SyntheticPayload,
            SYNTHETIC_FRAME_WIDTH,
            SYNTHETIC_FRAME_HEIGHT,
        )
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn byte_length(&self) -> usize {
        self.bytes.len()
    }

    pub fn source(&self) -> CapturedFrameSource {
        self.source
    }

    /// Canonical content digest of the exact admitted bytes.
    pub fn digest(&self) -> String {
        canonical_sha256_digest(&self.bytes)
    }

    pub fn public_evidence(&self) -> CapturedFrameEvidence {
        CapturedFrameEvidence {
            epoch: self.epoch,
            digest: self.digest(),
            byte_length: self.bytes.len(),
            source: self.source,
            media_kind: self.media_kind,
            width: self.width,
            height: self.height,
        }
    }

    pub fn to_guest_frame(&self, guest_button_pressed: bool) -> GuestFrame {
        let evidence = self.public_evidence();
        GuestFrame {
            epoch: evidence.epoch,
            digest: evidence.digest.clone(),
            guest_button_pressed,
            captured_frame: Some(evidence),
        }
    }

    /// Test/audit helper: recompute digest from the private payload.
    pub fn captured_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Explicit synthetic payload bytes for the default simulator.
///
/// Deterministic and labeled synthetic. Not a real browser screenshot, not an
/// engine raster, and not isolation PASS.
pub fn simulator_synthetic_frame_bytes(guest_link_clicked: bool) -> Vec<u8> {
    let pixel_len = (SYNTHETIC_FRAME_WIDTH as usize) * (SYNTHETIC_FRAME_HEIGHT as usize);
    let mut bytes = Vec::with_capacity(
        SYNTHETIC_FRAME_MAGIC.len() + SYNTHETIC_FRAME_PAYLOAD_NEEDLE.len() + 1 + pixel_len,
    );
    bytes.extend_from_slice(SYNTHETIC_FRAME_MAGIC);
    bytes.extend_from_slice(SYNTHETIC_FRAME_PAYLOAD_NEEDLE);
    // Visual content changes when the guest-local link is clicked. Epoch is
    // not mixed into the hash; it stays truthful metadata on the evidence.
    let fill = if guest_link_clicked { 0xC3 } else { 0x21 };
    bytes.resize(bytes.len() + pixel_len, fill);
    bytes
}

/// Canonical `sha256:` + 64 lowercase hex over `bytes` only.
pub fn canonical_sha256_digest(bytes: &[u8]) -> String {
    format!("{CANONICAL_DIGEST_PREFIX}{:x}", Sha256::digest(bytes))
}

pub fn is_canonical_sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(CANONICAL_DIGEST_PREFIX) else {
        return false;
    };
    hex.len() == CANONICAL_HEX_LEN
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Fail-closed engine capture entry. This slice does not wire a real capture.
pub fn admit_browser_engine_capture(
    _bytes: Vec<u8>,
    _width: u32,
    _height: u32,
) -> HarnessResult<BoundedCapturedFrame> {
    Err(engine_capture_unwired_error())
}

/// There is no admit-with-digest API. Caller digest claims fail closed.
pub fn admit_captured_frame_with_claimed_digest(
    _bytes: Vec<u8>,
    _claimed_digest: &str,
    _epoch: u64,
    _source: CapturedFrameSource,
    _media_kind: CapturedFrameMediaKind,
    _width: u32,
    _height: u32,
) -> HarnessResult<BoundedCapturedFrame> {
    Err(HarnessError::invalid_state(
        "caller-provided captured-frame digest claims are rejected; digest is computed from bounded capture bytes only",
    ))
}

impl CapturedFrameEvidence {
    pub fn verify_against_bytes(&self, bytes: &[u8]) -> HarnessResult<()> {
        validate_public_evidence(self)?;
        if bytes.is_empty() {
            return Err(HarnessError::invalid_state(
                "captured-frame bytes are empty",
            ));
        }
        if bytes.len() > MAX_CAPTURED_FRAME_BYTES {
            return Err(HarnessError::invalid_state(
                "captured-frame bytes exceed maximum admitted length",
            ));
        }
        if bytes.len() != self.byte_length {
            return Err(HarnessError::invalid_state(
                "captured-frame byte length does not match admitted payload",
            ));
        }
        let recomputed = canonical_sha256_digest(bytes);
        if recomputed != self.digest {
            return Err(HarnessError::invalid_state(
                "captured-frame digest does not match recomputation from bytes",
            ));
        }
        Ok(())
    }
}

pub fn validate_public_evidence(evidence: &CapturedFrameEvidence) -> HarnessResult<()> {
    if evidence.epoch == 0 {
        return Err(HarnessError::invalid_state(
            "captured-frame epoch must be bound to a booted frame",
        ));
    }
    if !is_canonical_sha256_digest(&evidence.digest) {
        return Err(HarnessError::invalid_state(
            "captured-frame digest is not canonical sha256 lowercase hex",
        ));
    }
    if evidence.byte_length == 0 {
        return Err(HarnessError::invalid_state(
            "captured-frame byte length is empty",
        ));
    }
    if evidence.byte_length > MAX_CAPTURED_FRAME_BYTES {
        return Err(HarnessError::invalid_state(
            "captured-frame byte length exceeds maximum admitted length",
        ));
    }
    if evidence.width == 0 || evidence.height == 0 {
        return Err(HarnessError::invalid_state(
            "captured-frame dimensions must be non-zero",
        ));
    }
    if evidence.source == CapturedFrameSource::BrowserEngine
        || evidence.media_kind == CapturedFrameMediaKind::EngineRgba8
    {
        return Err(engine_capture_unwired_error());
    }
    if evidence.source != CapturedFrameSource::SyntheticSimulator
        || evidence.media_kind != CapturedFrameMediaKind::SyntheticPayload
    {
        return Err(HarnessError::invalid_state(
            "captured-frame source and media classification are inconsistent",
        ));
    }
    if evidence.width != SYNTHETIC_FRAME_WIDTH || evidence.height != SYNTHETIC_FRAME_HEIGHT {
        return Err(HarnessError::invalid_state(
            "synthetic simulator frame dimensions are fixed",
        ));
    }
    Ok(())
}

/// GuestFrame may support postcondition evidence only after this check.
pub fn require_frame_for_postcondition(
    frame: &GuestFrame,
) -> HarnessResult<&CapturedFrameEvidence> {
    let Some(evidence) = frame.captured_frame.as_ref() else {
        return Err(HarnessError::invalid_state(
            "contained-browser postcondition requires captured-frame metadata",
        ));
    };
    validate_public_evidence(evidence)?;
    if frame.epoch != evidence.epoch {
        return Err(HarnessError::invalid_state(
            "captured-frame epoch is stale or misbound",
        ));
    }
    if frame.digest != evidence.digest {
        return Err(HarnessError::invalid_state(
            "guest frame digest does not match captured-frame evidence digest",
        ));
    }
    if !is_canonical_sha256_digest(&frame.digest) {
        return Err(HarnessError::invalid_state(
            "contained-browser GuestFrame digest is not canonical sha256 lowercase hex",
        ));
    }
    Ok(evidence)
}

pub fn assert_source_not_upgraded(
    declared: CapturedFrameSource,
    observed: CapturedFrameSource,
) -> HarnessResult<()> {
    if declared != observed {
        return Err(HarnessError::invalid_state(
            "captured-frame source must not be upgraded",
        ));
    }
    if observed == CapturedFrameSource::BrowserEngine {
        return Err(engine_capture_unwired_error());
    }
    Ok(())
}

/// Truthful epoch + content-addressed digest change for guest-local inject.
pub fn assert_postcondition_change(before: &GuestFrame, after: &GuestFrame) -> HarnessResult<()> {
    let before_ev = require_frame_for_postcondition(before)?;
    let after_ev = require_frame_for_postcondition(after)?;
    assert_source_not_upgraded(before_ev.source, after_ev.source)?;
    assert_source_not_upgraded(CapturedFrameSource::SyntheticSimulator, after_ev.source)?;
    if before.epoch >= after.epoch {
        return Err(HarnessError::invalid_state(
            "postcondition requires a later captured-frame epoch",
        ));
    }
    if before.digest == after.digest {
        return Err(HarnessError::invalid_state(
            "postcondition requires captured-frame digest change from bytes",
        ));
    }
    Ok(())
}

fn validate_admission_bounds(
    bytes: &[u8],
    source: CapturedFrameSource,
    media_kind: CapturedFrameMediaKind,
    width: u32,
    height: u32,
) -> HarnessResult<()> {
    if bytes.is_empty() {
        return Err(HarnessError::invalid_state(
            "captured-frame bytes are empty",
        ));
    }
    if bytes.len() > MAX_CAPTURED_FRAME_BYTES {
        return Err(HarnessError::invalid_state(
            "captured-frame bytes exceed maximum admitted length",
        ));
    }
    if width == 0 || height == 0 {
        return Err(HarnessError::invalid_state(
            "captured-frame dimensions must be non-zero",
        ));
    }
    if source != CapturedFrameSource::SyntheticSimulator
        || media_kind != CapturedFrameMediaKind::SyntheticPayload
    {
        return Err(HarnessError::invalid_state(
            "captured-frame source and media classification are inconsistent",
        ));
    }
    if width != SYNTHETIC_FRAME_WIDTH || height != SYNTHETIC_FRAME_HEIGHT {
        return Err(HarnessError::invalid_state(
            "synthetic simulator frame dimensions are fixed",
        ));
    }
    Ok(())
}

fn engine_capture_unwired_error() -> HarnessError {
    HarnessError::backend_unavailable(
        "browser-engine captured frame is not wired; refusing to fabricate engine receipt, captured bytes, native marker, or isolation PASS",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_recomputed_from_exact_bytes() {
        let capture = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("admit");
        let expected = canonical_sha256_digest(capture.captured_bytes());
        assert_eq!(capture.digest(), expected);
        assert!(is_canonical_sha256_digest(&capture.digest()));
        assert_eq!(capture.digest().len(), CANONICAL_DIGEST_PREFIX.len() + 64);
        capture
            .public_evidence()
            .verify_against_bytes(capture.captured_bytes())
            .expect("recompute");
    }

    #[test]
    fn state_change_changes_digest_not_from_labels() {
        let before = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("before");
        let after = BoundedCapturedFrame::admit_simulator_capture(2, true).expect("after");
        assert_ne!(before.digest(), after.digest());
        assert_eq!(
            before.digest(),
            canonical_sha256_digest(&simulator_synthetic_frame_bytes(false))
        );
        assert_eq!(
            after.digest(),
            canonical_sha256_digest(&simulator_synthetic_frame_bytes(true))
        );
        assert!(!before.digest().contains("link="));
        assert!(!after.digest().contains("contained-browser-frame"));
    }

    #[test]
    fn empty_and_oversize_rejected() {
        let empty = BoundedCapturedFrame::admit_from_source(
            1,
            Vec::new(),
            CapturedFrameSource::SyntheticSimulator,
            CapturedFrameMediaKind::SyntheticPayload,
            SYNTHETIC_FRAME_WIDTH,
            SYNTHETIC_FRAME_HEIGHT,
        )
        .expect_err("empty");
        assert_eq!(empty.code, crate::error::HarnessErrorCode::InvalidState);

        let oversize = vec![0x11; MAX_CAPTURED_FRAME_BYTES + 1];
        let err = BoundedCapturedFrame::admit_from_source(
            1,
            oversize,
            CapturedFrameSource::SyntheticSimulator,
            CapturedFrameMediaKind::SyntheticPayload,
            SYNTHETIC_FRAME_WIDTH,
            SYNTHETIC_FRAME_HEIGHT,
        )
        .expect_err("oversize");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);
        assert!(!err.message.contains("raw-synthetic-frame-pixels"));
    }

    #[test]
    fn one_byte_tamper_rejected() {
        let capture = BoundedCapturedFrame::admit_simulator_capture(1, true).expect("admit");
        let evidence = capture.public_evidence();
        let mut tampered = capture.captured_bytes().to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        let err = evidence
            .verify_against_bytes(&tampered)
            .expect_err("tamper");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);
    }

    #[test]
    fn caller_digest_and_malformed_rejected() {
        let bytes = simulator_synthetic_frame_bytes(false);
        let err = admit_captured_frame_with_claimed_digest(
            bytes.clone(),
            "sha256:contained-browser-frame:1:link=false",
            1,
            CapturedFrameSource::SyntheticSimulator,
            CapturedFrameMediaKind::SyntheticPayload,
            SYNTHETIC_FRAME_WIDTH,
            SYNTHETIC_FRAME_HEIGHT,
        )
        .expect_err("caller digest");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);

        let capture = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("admit");
        let mut evidence = capture.public_evidence();
        evidence.digest = "sha256:contained-browser-frame:1:link=false".into();
        assert!(validate_public_evidence(&evidence).is_err());
        evidence.digest = format!("SHA256:{}", "ab".repeat(32));
        assert!(validate_public_evidence(&evidence).is_err());
        evidence.digest = canonical_sha256_digest(&bytes);
        evidence.digest = evidence.digest.to_uppercase();
        assert!(validate_public_evidence(&evidence).is_err());
    }

    #[test]
    fn stale_misbound_and_source_upgrade_rejected() {
        let capture = BoundedCapturedFrame::admit_simulator_capture(1, false).expect("admit");
        let mut frame = capture.to_guest_frame(false);
        frame.epoch = 9;
        let err = require_frame_for_postcondition(&frame).expect_err("stale");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);

        let mut evidence = capture.public_evidence();
        evidence.source = CapturedFrameSource::BrowserEngine;
        let upgraded = validate_public_evidence(&evidence).expect_err("upgrade");
        assert_eq!(
            upgraded.code,
            crate::error::HarnessErrorCode::BackendUnavailable
        );

        let engine_bytes = simulator_synthetic_frame_bytes(false);
        let engine_err = BoundedCapturedFrame::admit_from_source(
            1,
            engine_bytes,
            CapturedFrameSource::BrowserEngine,
            CapturedFrameMediaKind::EngineRgba8,
            SYNTHETIC_FRAME_WIDTH,
            SYNTHETIC_FRAME_HEIGHT,
        )
        .expect_err("engine source");
        assert_eq!(
            engine_err.code,
            crate::error::HarnessErrorCode::BackendUnavailable
        );
    }

    #[test]
    fn public_forms_do_not_serialize_raw_bytes_or_paths() {
        let capture = BoundedCapturedFrame::admit_simulator_capture(2, true).expect("admit");
        let frame = capture.to_guest_frame(true);
        let json = serde_json::to_string(&frame).expect("json");
        let needle = std::str::from_utf8(SYNTHETIC_FRAME_PAYLOAD_NEEDLE).expect("needle");
        assert!(!json.contains(needle));
        assert!(!json.contains("GROKPTAH-SYNTHETIC-BROWSER-FRAME-v1"));
        assert!(!json.contains("/tmp"));
        assert!(!json.contains("\\\\"));
        assert!(!json.contains("capturedBytes"));
        assert!(!json.contains("raw-synthetic-frame"));
        let debug = format!("{capture:?}");
        assert!(!debug.contains(needle));
        assert!(json.contains("synthetic_simulator"));
        assert!(json.contains("synthetic_payload"));
    }

    #[test]
    fn engine_capture_entry_fails_closed() {
        let err =
            admit_browser_engine_capture(vec![0x01, 0x02, 0x03], 8, 8).expect_err("engine unwired");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("not wired"));
        assert!(err.message.contains("refusing to fabricate"));
        assert!(!err.message.contains("isolation is proven"));
    }
}
