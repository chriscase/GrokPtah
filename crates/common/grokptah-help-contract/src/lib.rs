//! The Rust-owned Semantic Help contract.
//!
//! This crate is the single definition of Semantic Help's data:
//!
//! * [`corpus`] — the canonical corpus shape and its byte-exact digests.
//! * [`data`] — the one authored corpus, as seeds.
//! * [`dto`] — every authority, request, result, and receipt type.
//! * [`codegen`] — the model the JSON Schema and TypeScript are emitted from.
//! * [`digest`] — the injective, domain-separated digest rules both languages
//!   follow.
//!
//! # One corpus, two readers
//!
//! [`CORPUS_JSON`] embeds the exact bytes of the committed corpus artifact.
//! TypeScript imports that same file, so there is one document rather than two
//! copies that a rebuild could desynchronize. The bytes are produced by the
//! `help-codegen` binary from [`data`]; `help-codegen --verify` re-emits and
//! compares, so a hand edit to the artifact fails the gate instead of becoming
//! an unreviewed second source of truth.

pub mod codegen;
pub mod corpus;
pub mod data;
pub mod digest;
pub mod dto;

#[cfg(test)]
mod dto_tests;

/// Repository-relative path of the one canonical corpus artifact.
pub const CORPUS_ARTIFACT_PATH: &str = "desktop/src/lib/help/canonical/help-corpus.v1.json";
/// Repository-relative path of the generated TypeScript contract.
pub const TYPESCRIPT_ARTIFACT_PATH: &str = "desktop/src/lib/help/generated/contract.ts";
/// Repository-relative path of the generated JSON Schema.
pub const SCHEMA_ARTIFACT_PATH: &str = "docs/schemas/grokptah-help.v1.schema.json";
/// Repository-relative path of the Rust-emitted digest parity vectors.
pub const PARITY_ARTIFACT_PATH: &str = "desktop/src/lib/help/generated/digest-parity.json";
/// Repository-relative path of the public-only bundle the package ships.
pub const PUBLIC_CORPUS_ARTIFACT_PATH: &str =
    "desktop/src/lib/help/canonical/help-corpus-public.v1.json";

/// The exact bytes of the committed corpus artifact, embedded at compile time.
///
/// Embedding rather than reading at runtime means a host binary carries the
/// corpus it was built against. A corpus file swapped on disk after the build
/// cannot change what this process serves.
pub const CORPUS_JSON: &str =
    include_str!("../../../../desktop/src/lib/help/canonical/help-corpus.v1.json");

/// Build the corpus from the authored seeds.
///
/// # Panics
/// Panics if the authored seeds are internally inconsistent — an unknown
/// citation, a duplicate id, or an article less restricted than a source it
/// cites. That is a bug in this crate's own data, not a runtime condition, so
/// it fails loudly at the first call rather than degrading.
#[must_use]
pub fn build_corpus() -> corpus::Corpus {
    corpus::build(data::SOURCES, data::ARTICLES).expect("authored Help corpus is consistent")
}

/// Parse and verify the embedded corpus artifact.
///
/// # Errors
/// Returns [`corpus::CorpusError`] when a stored digest disagrees with the
/// bytes it describes, or when the document is structurally inconsistent.
///
/// # Panics
/// Panics if the embedded artifact is not valid JSON for a corpus, which would
/// mean the committed artifact and this crate's types disagree — a build-time
/// inconsistency rather than a runtime one.
pub fn embedded_corpus() -> Result<corpus::Corpus, corpus::CorpusError> {
    let parsed: corpus::Corpus =
        serde_json::from_str(CORPUS_JSON).expect("embedded corpus artifact parses");
    parsed.verify()?;
    Ok(parsed)
}

/// Render the corpus artifact exactly as it is committed.
#[must_use]
pub fn render_corpus_artifact(value: &corpus::Corpus) -> String {
    let json = serde_json::to_value(value).expect("corpus serializes");
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    serde::Serialize::serialize(&json, &mut serializer).expect("corpus serializes");
    let mut text = String::from_utf8(buffer).expect("corpus is utf-8");
    text.push('\n');
    text
}
