//! Read-only source inspection for GrokPtah desktop.
//!
//! Clicking a file in GrokPtah used to compose a `read <path>` prompt and
//! hand it to the model. This crate is the direct alternative: it resolves a
//! path inside an explicitly approved boundary — the operator's workspace or
//! one isolated run worktree — and returns a bounded, read-only projection of
//! the bytes. It performs no writes, spawns no processes, holds no
//! credentials, and follows no symbolic links.
//!
//! The two halves are deliberately separate:
//!
//! * [`root`] decides *whether* a path may be read (lexical normalisation
//!   plus a per-component symlink walk, both fail-closed).
//! * [`document`] decides *how much* of it may be read (byte, line, and
//!   line-width ceilings, with UTF-8 and binary classification).

#![forbid(unsafe_code)]

mod approval;
mod document;
mod error;
mod root;

pub use approval::{boundary_message, is_managed_run_worktree, short_root_label};
pub use document::{
    Eol, MAX_BYTES_CEILING, MAX_LINE_CHARS_CEILING, MAX_LINES_CEILING, SourceDocument,
    SourceLimits, SourceLine, TextEncoding, language_for, read_document,
};
pub use error::SourceViewError;
pub use root::{
    ResolvedSource, RootKind, SourceRoot, SourceRootRegistry, normalize_relative, resolve_in_root,
};

/// Resolve and read in one step against an approved registry.
pub fn open_in_registry(
    registry: &SourceRootRegistry,
    root_id: &str,
    requested_path: &str,
    limits: SourceLimits,
) -> Result<SourceDocument, SourceViewError> {
    let resolved = registry.resolve(root_id, requested_path)?;
    read_document(&resolved, limits)
}

#[cfg(test)]
mod tests;
