//! Read-only source inspection for GrokPtah.
//!
//! Opening a file in GrokPtah is a direct, contained read rather than a prompt
//! handed to a model. This crate is that read, and nothing else: it performs
//! no writes, spawns no processes, opens no sockets, and holds no credentials.
//!
//! # One authority path
//!
//! ```text
//!   authorization context ─┐
//!                          ├─ SnapshotStore::issue ─→ RootSnapshot { opaque tokens }
//!   candidate roots ───────┘                                    │
//!                                                               ▼
//!   token + acting context ─→ SnapshotStore::resolve ─→ ResolvedRoot
//!                                                               │
//!                                    lexical containment ───────┤
//!                                                               ▼
//!                          handle-relative no-follow open ─→ OpenedDocument
//!                                                               │
//!                                    bounded chunk read ────────┤
//!                                                               ▼
//!                                                       SourceDocument
//! ```
//!
//! Each arrow is a refusal point, and every refusal is one of the closed codes
//! in [`SourceViewError::CODES`]. There is no route that reaches a byte of a
//! file without passing all of them, and no route that picks a root on the
//! caller's behalf: a request that cannot name exactly one authorized root is
//! refused rather than resolved to a default.
//!
//! # What never crosses the boundary
//!
//! Absolute paths and file content never appear in a descriptor, a receipt, or
//! an error. Callers identify a tree by [`RootDescriptor::path_digest`] and a
//! file by [`DocumentIdentity`]; people read a short label.

#![forbid(unsafe_code)]

mod approval;
mod clock;
mod digest;
mod error;
mod identity;
mod open;
mod path;
mod principal;
mod read;
mod snapshot;
mod utf8;
mod winpath;

pub use approval::{is_git_worktree_pointer, is_managed_run_worktree, short_root_label};
pub use clock::{Clock, SystemClock, TestClock};
pub use digest::{constant_time_eq, digest_label, from_hex, to_hex};
pub use error::{SourceViewError, boundary_message};
pub use identity::{IdentityBasis, IdentityStability, NodeIdentity, path_digest};
pub use open::{OpenedDocument, RootHandle, observe_root, open_contained};
pub use path::{ContainedPath, PathPolicy, normalize_request};
pub use principal::{AuthorizationContext, PolicyInputs, Principal};
pub use read::{
    BINARY_SCAN_BYTES, CONTENT_DIGEST_BUDGET, ContentClass, ContentVerdict, DocumentIdentity,
    DocumentProjection, EffectiveLimits, Eol, LineAssembler, MAX_CHUNK_BYTES, MAX_CHUNK_LINES,
    MAX_DOCUMENT_BYTES, MAX_LINE_CHARS, ReadCursor, RequestedLimits, SourceChunk, SourceLine,
    language_for, read_projection,
};
pub use snapshot::{
    CandidateRoot, REPLAY_POLICY, ResolvedRoot, RootDescriptor, RootKind, RootSnapshot,
    SnapshotStore, TOKEN_VERSION,
};
pub use utf8::{DecodedChunk, Utf8Decoder};
pub use winpath::{
    WindowsPathForm, case_fold, classify as classify_windows_path, has_alternate_data_stream,
    has_illegal_character, has_stripped_tail, is_reserved_device_name, segments_equal_folded,
};

/// The contract version this crate implements. Mirrored by the TypeScript
/// parser and the JSON Schema; a mismatch is a contract test failure.
pub const SOURCE_VIEW_CONTRACT: &str = "grokptah.source-view.v1";

/// One read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRequest {
    /// Opaque root token. There is no other way to name a root.
    pub token: String,
    /// Root-relative or root-contained absolute path.
    pub path: String,
    /// Byte offset for a fresh range read. Ignored when `cursor` is present.
    pub start_byte: u64,
    /// Continuation of a previous read.
    pub cursor: Option<ReadCursor>,
    pub limits: RequestedLimits,
}

impl SourceRequest {
    pub fn new(token: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            path: path.into(),
            start_byte: 0,
            cursor: None,
            limits: RequestedLimits::default(),
        }
    }

    pub fn at_byte(mut self, start_byte: u64) -> Self {
        self.start_byte = start_byte;
        self
    }

    pub fn resume(mut self, cursor: ReadCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    pub fn with_limits(mut self, limits: RequestedLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// A bounded, read-only projection of one file inside one authorized root.
///
/// Nothing here names an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDocument {
    pub contract: &'static str,
    pub root: RootDescriptor,
    pub snapshot_id: String,
    pub revision: u64,
    pub relative_path: String,
    pub language: String,
    pub byte_len: u64,
    pub content: ContentClass,
    pub identity: DocumentIdentity,
    pub limits: EffectiveLimits,
    pub chunk: SourceChunk,
}

/// Resolve, contain, open, and read — the only entry point that returns bytes.
pub fn open_document(
    store: &SnapshotStore,
    context: &AuthorizationContext,
    request: &SourceRequest,
    policy: PathPolicy,
) -> Result<SourceDocument, SourceViewError> {
    let resolved = store.resolve(&request.token, context)?;
    let contained = normalize_request(resolved.handle.path(), &request.path, policy)?;
    let opened = open_contained(&resolved.handle, &contained)?;
    let projection = read_projection(
        &opened,
        request.start_byte,
        request.cursor.as_ref(),
        request.limits,
    )?;
    let relative_path = contained.display();
    Ok(SourceDocument {
        contract: SOURCE_VIEW_CONTRACT,
        language: language_for(&relative_path).to_string(),
        relative_path,
        root: resolved.descriptor,
        snapshot_id: resolved.snapshot_id,
        revision: resolved.revision,
        byte_len: projection.byte_len,
        content: projection.content,
        identity: projection.identity,
        limits: projection.limits,
        chunk: projection.chunk,
    })
}

#[cfg(test)]
mod tests;
