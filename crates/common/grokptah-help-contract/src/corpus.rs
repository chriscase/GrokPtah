//! The one canonical Help corpus.
//!
//! There is exactly one hand-maintained corpus in the tree. It is authored as
//! Rust seed data, built here into a digest-bound document, and emitted as a
//! single JSON artifact that Rust and TypeScript both read. Neither side owns
//! a second copy, so the two cannot disagree about what the corpus says.
//!
//! Every digest covers exact bytes:
//!
//! * a **source** digest covers its id, path and heading;
//! * a **chunk** digest covers its id, article, kind, ordinal, locale, the
//!   chunk text itself, and the sources backing it;
//! * an **article** digest covers its fields and its sources' digests;
//! * the **corpus** digest covers the article and chunk digests in order.
//!
//! A citation span therefore commits to the bytes it indexes rather than to a
//! name. Rebuild the corpus with different text and every span over it is
//! invalidated instead of silently re-pointing at new content.
//!
//! Where a digest covers a *list*, it covers the list's identity and length as
//! well as its items: each variable-length region is opened by its own label
//! and element count (see [`region`]). Length prefixing alone makes a flat
//! field list injective but records nothing about which sub-list a field came
//! from, so concatenating an article's `aliases`, `keywords` and
//! `capability_ids` digested identically however they were partitioned — and a
//! capability moved into an alias left the article and corpus digests
//! unchanged while removing the gate that `Authority::manifest_for` enforces.
//!
//! A chunk must also carry its article's visibility. `visible_corpus` selects
//! chunks by article and `bundle_at` selects them by their own label, so a
//! document where the two disagree is served differently by each; [`Corpus::verify`]
//! refuses it rather than letting the two resolve it differently.

use serde::{Deserialize, Serialize};

use crate::digest::{canonical_digest, domain, domain_digest};

/// Bumped when the shape of a canonical record changes.
pub const SCHEMA_VERSION: &str = "grokptah.help-canonical.v1";
/// Bumped when the content changes in a way consumers should notice.
pub const CONTENT_VERSION: &str = "help-canonical-2026.08.1";

/// Longest chunk text emitted by the builder, in characters.
pub const CHUNK_MAX_CHARS: usize = 480;

/// Who a source may be shown to. Only `Public` may enter a published bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Shippable in the public `@grokptah/client` bundle.
    Public,
    /// Requires an authenticated principal with the matching capability.
    Gated,
    /// Requires an operator principal.
    Operator,
}

impl Visibility {
    /// Rank used for "at least as restricted as" comparisons.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Gated => 1,
            Self::Operator => 2,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Gated => "gated",
            Self::Operator => "operator",
        }
    }
}

/// A citation target: an exact repository path plus an exact heading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAnchor {
    /// Stable citation id used in answers, e.g. `provider.profiles`.
    pub id: String,
    /// Repository-relative path. Must exist in the tree.
    pub path: String,
    /// Exact Markdown heading text within `path`.
    pub heading: String,
    pub visibility: Visibility,
    /// `domain_digest(SOURCE, [id, path, heading, visibility])`.
    pub digest: String,
}

/// The part of an article a chunk was cut from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Title,
    Summary,
    Body,
}

impl ChunkKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Summary => "summary",
            Self::Body => "body",
        }
    }
}

/// A retrievable unit. Ids are stable across rebuilds because they derive from
/// the article id, the chunk kind, and a stable ordinal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    /// `${article_id}#${kind}.${ordinal}` — stable and citable.
    pub id: String,
    pub article_id: String,
    pub kind: ChunkKind,
    pub ordinal: usize,
    /// The exact bytes a citation span indexes into.
    pub text: String,
    pub locale: String,
    /// Source anchor ids backing this chunk. Never empty.
    pub source_ids: Vec<String>,
    pub visibility: Visibility,
    /// `domain_digest(CHUNK, [id, article, kind, ordinal, locale, text, ..sources])`.
    pub digest: String,
}

// kebab-case, not snake_case: `as_str` below feeds the article digest while
// serde feeds the JSON, and the two must be the same string. When they were
// not, Rust digested `computer-use` and TypeScript digested `computer_use`,
// so the shipped corpus failed verification in one language and passed in the
// other. `model_enum_variants_match_rust_serde` now covers this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Topic {
    GettingStarted,
    Providers,
    ComputerUse,
    Operations,
}

impl Topic {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GettingStarted => "getting-started",
            Self::Providers => "providers",
            Self::ComputerUse => "computer-use",
            Self::Operations => "operations",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Article {
    pub id: String,
    pub title: String,
    pub topic: Topic,
    pub summary: String,
    pub body: String,
    /// Natural-language phrasings a user might type.
    pub aliases: Vec<String>,
    /// Expert / identifier terminology.
    pub keywords: Vec<String>,
    pub source_ids: Vec<String>,
    pub visibility: Visibility,
    /// Capability ids a principal must hold to be served this article.
    pub capability_ids: Vec<String>,
    /// `domain_digest(ARTICLE, [..fields, ..source digests])`.
    pub digest: String,
}

/// The frozen, digest-bound corpus handed to every retriever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    pub schema_version: String,
    pub content_version: String,
    pub sources: Vec<SourceAnchor>,
    pub articles: Vec<Article>,
    pub chunks: Vec<Chunk>,
    /// Digest over the article and chunk digests, in order.
    pub digest: String,
    /// Digest over the ordered source-record digests, for anchor drift checks.
    pub source_digest: String,
}

/// Authoring shape for a source, before its digest exists.
pub struct SourceSeed {
    pub id: &'static str,
    pub path: &'static str,
    pub heading: &'static str,
    pub visibility: Visibility,
}

/// Authoring shape for an article, before its digests exist.
pub struct ArticleSeed {
    pub id: &'static str,
    pub title: &'static str,
    pub topic: Topic,
    pub summary: &'static str,
    pub body: &'static str,
    pub aliases: &'static [&'static str],
    pub keywords: &'static [&'static str],
    pub source_ids: &'static [&'static str],
    pub visibility: Visibility,
    pub capability_ids: &'static [&'static str],
}

/// Why a corpus document was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusError {
    UnknownSource {
        article: String,
        source: String,
    },
    EmptySources {
        article: String,
    },
    DuplicateId {
        id: String,
    },
    /// A stored digest disagrees with the bytes it claims to describe.
    DigestMismatch {
        record: String,
        expected: String,
        actual: String,
    },
    /// An article is less restricted than a source it cites, which would leak
    /// the source's existence to a principal not entitled to it.
    VisibilityInversion {
        article: String,
        source: String,
    },
    /// A chunk names an article the document does not carry, so nothing
    /// decides who may see it.
    UnknownArticle {
        chunk: String,
        article: String,
    },
    /// A chunk's visibility disagrees with its article's. Filtering by article
    /// and filtering by chunk would then serve different content.
    ChunkVisibilityMismatch {
        chunk: String,
        article: String,
        chunk_visibility: Visibility,
        article_visibility: Visibility,
    },
    SchemaVersion {
        found: String,
    },
}

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSource { article, source } => {
                write!(f, "article `{article}` cites unknown source `{source}`")
            }
            Self::EmptySources { article } => write!(f, "article `{article}` cites no source"),
            Self::DuplicateId { id } => write!(f, "duplicate id `{id}`"),
            Self::DigestMismatch {
                record,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "digest mismatch for `{record}`: expected {expected}, recomputed {actual}"
                )
            }
            Self::VisibilityInversion { article, source } => write!(
                f,
                "article `{article}` is less restricted than the source `{source}` it cites"
            ),
            Self::UnknownArticle { chunk, article } => {
                write!(f, "chunk `{chunk}` names unknown article `{article}`")
            }
            Self::ChunkVisibilityMismatch {
                chunk,
                article,
                chunk_visibility,
                article_visibility,
            } => write!(
                f,
                "chunk `{chunk}` is {} but its article `{article}` is {}",
                chunk_visibility.as_str(),
                article_visibility.as_str()
            ),
            Self::SchemaVersion { found } => {
                write!(f, "unsupported corpus schema version `{found}`")
            }
        }
    }
}

impl std::error::Error for CorpusError {}

/// Labels that open each variable-length region of a digest.
///
/// Length prefixing makes a *flat* field list injective, but it says nothing
/// about where one sub-list ends and the next begins. Concatenating
/// `aliases ++ keywords ++ capability_ids` therefore digested the same bytes
/// whichever list each item came from, so moving `computer.control` out of
/// `capability_ids` and into `aliases` left the article digest — and the
/// corpus digest above it — unchanged, and a capability gate could be removed
/// from a document that still passed [`Corpus::verify`].
///
/// Each region is now opened by its own label and its own element count, so
/// the partition is part of what is hashed. Repartitioning changes a label's
/// count, reordering changes the element sequence, and omitting or duplicating
/// an element changes the count: all four are visible to the digest.
mod region {
    pub const ALIASES: &str = "aliases";
    pub const KEYWORDS: &str = "keywords";
    pub const CAPABILITIES: &str = "capabilities";
    pub const SOURCES: &str = "sources";
    pub const ARTICLES: &str = "articles";
    pub const CHUNKS: &str = "chunks";
}

/// Append one labelled, counted region to a field list.
fn push_region<'a>(fields: &mut Vec<&'a str>, label: &'a str, count: &'a str, items: &[&'a str]) {
    fields.push(label);
    fields.push(count);
    fields.extend_from_slice(items);
}

/// The one definition of an article digest.
///
/// `build` and [`Corpus::verify`] both call this. They previously assembled
/// the field list separately, which is how the two could have drifted apart
/// without any test noticing.
///
/// Public because `codegen` emits the cross-language parity vectors from it:
/// TypeScript re-implements this encoding, and the vectors are what stop the
/// two from agreeing only by intention.
#[must_use]
pub fn article_digest_of(
    id: &str,
    title: &str,
    topic: &str,
    summary: &str,
    body: &str,
    visibility: &str,
    aliases: &[&str],
    keywords: &[&str],
    capability_ids: &[&str],
    source_digests: &[&str],
) -> String {
    let counts = [
        aliases.len().to_string(),
        keywords.len().to_string(),
        capability_ids.len().to_string(),
        source_digests.len().to_string(),
    ];
    let mut fields: Vec<&str> = vec![id, title, topic, summary, body, visibility];
    push_region(&mut fields, region::ALIASES, counts[0].as_str(), aliases);
    push_region(&mut fields, region::KEYWORDS, counts[1].as_str(), keywords);
    push_region(
        &mut fields,
        region::CAPABILITIES,
        counts[2].as_str(),
        capability_ids,
    );
    push_region(
        &mut fields,
        region::SOURCES,
        counts[3].as_str(),
        source_digests,
    );
    domain_digest(domain::ARTICLE, &fields)
}

/// The one definition of the set-level corpus digest.
///
/// The article and chunk lists are labelled and counted for the same reason
/// the article's sub-lists are: so the boundary between them is hashed rather
/// than inferred from where one kind of digest stops appearing.
fn corpus_digest_of(
    schema_version: &str,
    content_version: &str,
    article_digests: &[&str],
    chunk_digests: &[&str],
    source_digest: &str,
) -> String {
    let counts = [
        article_digests.len().to_string(),
        chunk_digests.len().to_string(),
    ];
    let mut fields: Vec<&str> = vec![schema_version, content_version];
    push_region(
        &mut fields,
        region::ARTICLES,
        counts[0].as_str(),
        article_digests,
    );
    push_region(
        &mut fields,
        region::CHUNKS,
        counts[1].as_str(),
        chunk_digests,
    );
    fields.push(region::SOURCES);
    fields.push(source_digest);
    domain_digest(domain::CORPUS, &fields)
}

/// The digest over the ordered source records.
///
/// Hashing `path#heading` strings made the set ambiguous: `a#b` + `c` and
/// `a` + `b#c` produced the same input. Each source record already has an
/// injective digest over id, path, heading, and visibility, so the set binds
/// those record digests in a labelled, counted region instead.
pub(crate) fn source_set_digest_of(sources: &[SourceAnchor]) -> String {
    let count = sources.len().to_string();
    let mut fields = vec![region::SOURCES, count.as_str()];
    fields.extend(sources.iter().map(|source| source.digest.as_str()));
    domain_digest(domain::SOURCE_SET, &fields)
}

fn source_digest_of(seed_id: &str, path: &str, heading: &str, visibility: Visibility) -> String {
    domain_digest(
        domain::SOURCE,
        &[seed_id, path, heading, visibility.as_str()],
    )
}

/// Split prose into chunks bounded by [`CHUNK_MAX_CHARS`], on sentence
/// boundaries where possible so a chunk stays independently quotable.
fn split_body(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for sentence in split_sentences(text) {
        let candidate_len = current.chars().count() + sentence.chars().count();
        if !current.is_empty() && candidate_len > CHUNK_MAX_CHARS {
            chunks.push(current.trim().to_string());
            current = String::new();
        }
        current.push_str(&sentence);
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

/// Sentence split that keeps the terminator and the following space attached,
/// so concatenating the pieces reproduces the input exactly.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            // Consume the run of spaces that belongs to this sentence.
            while chars.peek() == Some(&' ') {
                current.push(' ');
                chars.next();
            }
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Build the digest-bound corpus from seed data.
///
/// # Errors
/// Returns [`CorpusError`] when a seed set is internally inconsistent: an
/// unknown or empty citation, a duplicate id, or an article that is less
/// restricted than a source it cites.
pub fn build(
    source_seeds: &[SourceSeed],
    article_seeds: &[ArticleSeed],
) -> Result<Corpus, CorpusError> {
    let mut sources: Vec<SourceAnchor> = source_seeds
        .iter()
        .map(|seed| SourceAnchor {
            id: seed.id.to_string(),
            path: seed.path.to_string(),
            heading: seed.heading.to_string(),
            visibility: seed.visibility,
            digest: source_digest_of(seed.id, seed.path, seed.heading, seed.visibility),
        })
        .collect();
    sources.sort_by(|left, right| left.id.cmp(&right.id));

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for source in &sources {
        if !seen.insert(source.id.clone()) {
            return Err(CorpusError::DuplicateId {
                id: source.id.clone(),
            });
        }
    }

    let find = |id: &str| sources.iter().find(|source| source.id == id);

    let mut articles: Vec<Article> = Vec::with_capacity(article_seeds.len());
    let mut chunks: Vec<Chunk> = Vec::new();

    for seed in article_seeds {
        if !seen.insert(seed.id.to_string()) {
            return Err(CorpusError::DuplicateId {
                id: seed.id.to_string(),
            });
        }
        if seed.source_ids.is_empty() {
            return Err(CorpusError::EmptySources {
                article: seed.id.to_string(),
            });
        }
        let mut cited: Vec<&SourceAnchor> = Vec::with_capacity(seed.source_ids.len());
        for source_id in seed.source_ids {
            let anchor = find(source_id).ok_or_else(|| CorpusError::UnknownSource {
                article: seed.id.to_string(),
                source: (*source_id).to_string(),
            })?;
            if anchor.visibility.rank() > seed.visibility.rank() {
                return Err(CorpusError::VisibilityInversion {
                    article: seed.id.to_string(),
                    source: (*source_id).to_string(),
                });
            }
            cited.push(anchor);
        }

        let source_digests: Vec<&str> = cited.iter().map(|anchor| anchor.digest.as_str()).collect();
        let article_digest = article_digest_of(
            seed.id,
            seed.title,
            seed.topic.as_str(),
            seed.summary,
            seed.body,
            seed.visibility.as_str(),
            seed.aliases,
            seed.keywords,
            seed.capability_ids,
            &source_digests,
        );

        let source_ids: Vec<String> = seed.source_ids.iter().map(|id| (*id).to_string()).collect();

        let mut push_chunk = |kind: ChunkKind, ordinal: usize, text: &str| {
            let id = format!("{}#{}.{}", seed.id, kind.as_str(), ordinal);
            let ordinal_string = ordinal.to_string();
            let mut chunk_fields: Vec<&str> = vec![
                &id,
                seed.id,
                kind.as_str(),
                &ordinal_string,
                "en",
                text,
                seed.visibility.as_str(),
            ];
            chunk_fields.extend(source_ids.iter().map(String::as_str));
            let digest = domain_digest(domain::CHUNK, &chunk_fields);
            chunks.push(Chunk {
                id,
                article_id: seed.id.to_string(),
                kind,
                ordinal,
                text: text.to_string(),
                locale: "en".to_string(),
                source_ids: source_ids.clone(),
                visibility: seed.visibility,
                digest,
            });
        };

        push_chunk(ChunkKind::Title, 0, seed.title);
        push_chunk(ChunkKind::Summary, 0, seed.summary);
        for (ordinal, text) in split_body(seed.body).into_iter().enumerate() {
            push_chunk(ChunkKind::Body, ordinal, &text);
        }

        articles.push(Article {
            id: seed.id.to_string(),
            title: seed.title.to_string(),
            topic: seed.topic,
            summary: seed.summary.to_string(),
            body: seed.body.to_string(),
            aliases: seed
                .aliases
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            keywords: seed
                .keywords
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            source_ids,
            visibility: seed.visibility,
            capability_ids: seed
                .capability_ids
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            digest: article_digest,
        });
    }

    articles.sort_by(|left, right| left.id.cmp(&right.id));
    chunks.sort_by(|left, right| left.id.cmp(&right.id));

    // Record ids form one lookup namespace. In particular, a generated chunk
    // id may not alias a source or article id even though those collections
    // are stored separately.
    for chunk in &chunks {
        if !seen.insert(chunk.id.clone()) {
            return Err(CorpusError::DuplicateId {
                id: chunk.id.clone(),
            });
        }
    }

    let source_digest = source_set_digest_of(&sources);
    let digest = corpus_digest_of(
        SCHEMA_VERSION,
        CONTENT_VERSION,
        &articles
            .iter()
            .map(|article| article.digest.as_str())
            .collect::<Vec<_>>(),
        &chunks
            .iter()
            .map(|chunk| chunk.digest.as_str())
            .collect::<Vec<_>>(),
        &source_digest,
    );

    Ok(Corpus {
        schema_version: SCHEMA_VERSION.to_string(),
        content_version: CONTENT_VERSION.to_string(),
        sources,
        articles,
        chunks,
        digest,
        source_digest,
    })
}

impl Corpus {
    /// Recompute every digest from the stored bytes and reject any drift.
    ///
    /// This is the check a host runs before serving anything: it proves the
    /// document in hand is the one its digest names, so a swapped corpus file
    /// fails closed rather than answering with content nobody reviewed.
    ///
    /// # Errors
    /// Returns [`CorpusError`] on the first record whose recomputed digest
    /// disagrees with the stored one, or on structural inconsistency.
    pub fn verify(&self) -> Result<(), CorpusError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(CorpusError::SchemaVersion {
                found: self.schema_version.clone(),
            });
        }

        // Reject ambiguity before performing any first-match lookup. Rust's
        // iterator lookup and TypeScript's Map construction otherwise select
        // different duplicate records, which can turn a restricted record
        // into public content depending on which reader serves it.
        let mut seen = std::collections::BTreeSet::new();
        for id in self
            .sources
            .iter()
            .map(|source| source.id.as_str())
            .chain(self.articles.iter().map(|article| article.id.as_str()))
            .chain(self.chunks.iter().map(|chunk| chunk.id.as_str()))
        {
            if !seen.insert(id) {
                return Err(CorpusError::DuplicateId { id: id.to_string() });
            }
        }

        for source in &self.sources {
            let actual =
                source_digest_of(&source.id, &source.path, &source.heading, source.visibility);
            if actual != source.digest {
                return Err(CorpusError::DigestMismatch {
                    record: format!("source:{}", source.id),
                    expected: source.digest.clone(),
                    actual,
                });
            }
        }

        let actual_source_digest = source_set_digest_of(&self.sources);
        if actual_source_digest != self.source_digest {
            return Err(CorpusError::DigestMismatch {
                record: "source-set".to_string(),
                expected: self.source_digest.clone(),
                actual: actual_source_digest,
            });
        }

        for article in &self.articles {
            let mut cited: Vec<&SourceAnchor> = Vec::new();
            for source_id in &article.source_ids {
                let anchor = self
                    .sources
                    .iter()
                    .find(|source| &source.id == source_id)
                    .ok_or_else(|| CorpusError::UnknownSource {
                        article: article.id.clone(),
                        source: source_id.clone(),
                    })?;
                if anchor.visibility.rank() > article.visibility.rank() {
                    return Err(CorpusError::VisibilityInversion {
                        article: article.id.clone(),
                        source: source_id.clone(),
                    });
                }
                cited.push(anchor);
            }
            if cited.is_empty() {
                return Err(CorpusError::EmptySources {
                    article: article.id.clone(),
                });
            }
            let actual = article_digest_of(
                &article.id,
                &article.title,
                article.topic.as_str(),
                &article.summary,
                &article.body,
                article.visibility.as_str(),
                &article
                    .aliases
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                &article
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                &article
                    .capability_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                &cited
                    .iter()
                    .map(|anchor| anchor.digest.as_str())
                    .collect::<Vec<_>>(),
            );
            if actual != article.digest {
                return Err(CorpusError::DigestMismatch {
                    record: format!("article:{}", article.id),
                    expected: article.digest.clone(),
                    actual,
                });
            }
        }
        for chunk in &self.chunks {
            // A chunk is only ever reachable through its article, so a chunk
            // whose article is absent is content no manifest can account for.
            let Some(article) = self.article(&chunk.article_id) else {
                return Err(CorpusError::UnknownArticle {
                    chunk: chunk.id.clone(),
                    article: chunk.article_id.clone(),
                });
            };
            // The builder gives a chunk its article's visibility, and both
            // filters downstream depend on that holding: `visible_corpus`
            // selects chunks by article, and `bundle_at` selects them by their
            // own label. A document where the two disagree is served
            // differently by each, so it is rejected here rather than
            // resolved differently in two places.
            if chunk.visibility != article.visibility {
                return Err(CorpusError::ChunkVisibilityMismatch {
                    chunk: chunk.id.clone(),
                    article: article.id.clone(),
                    chunk_visibility: chunk.visibility,
                    article_visibility: article.visibility,
                });
            }
            let ordinal = chunk.ordinal.to_string();
            let mut fields: Vec<&str> = vec![
                &chunk.id,
                &chunk.article_id,
                chunk.kind.as_str(),
                &ordinal,
                &chunk.locale,
                &chunk.text,
                chunk.visibility.as_str(),
            ];
            fields.extend(chunk.source_ids.iter().map(String::as_str));
            let actual = domain_digest(domain::CHUNK, &fields);
            if actual != chunk.digest {
                return Err(CorpusError::DigestMismatch {
                    record: format!("chunk:{}", chunk.id),
                    expected: chunk.digest.clone(),
                    actual,
                });
            }
        }

        let actual_corpus_digest = corpus_digest_of(
            &self.schema_version,
            &self.content_version,
            &self
                .articles
                .iter()
                .map(|article| article.digest.as_str())
                .collect::<Vec<_>>(),
            &self
                .chunks
                .iter()
                .map(|chunk| chunk.digest.as_str())
                .collect::<Vec<_>>(),
            &self.source_digest,
        );
        if actual_corpus_digest != self.digest {
            return Err(CorpusError::DigestMismatch {
                record: "corpus".to_string(),
                expected: self.digest.clone(),
                actual: actual_corpus_digest,
            });
        }
        Ok(())
    }

    /// Look up a chunk by id.
    #[must_use]
    pub fn chunk(&self, id: &str) -> Option<&Chunk> {
        self.chunks.iter().find(|chunk| chunk.id == id)
    }

    /// Look up a source anchor by id.
    #[must_use]
    pub fn source(&self, id: &str) -> Option<&SourceAnchor> {
        self.sources.iter().find(|source| source.id == id)
    }

    /// Look up an article by id.
    #[must_use]
    pub fn article(&self, id: &str) -> Option<&Article> {
        self.articles.iter().find(|article| article.id == id)
    }

    /// The subset of this corpus a bundle at `visibility` may contain.
    ///
    /// Used to build the published package: `Visibility::Public` yields public
    /// sources only. Record digests are preserved unchanged so a consumer's
    /// citation still verifies against the full corpus, while the corpus-level
    /// digest is recomputed over the retained records — a filtered bundle is
    /// honestly a different document, and says so.
    #[must_use]
    pub fn bundle_at(&self, visibility: Visibility) -> Corpus {
        let keep = |candidate: Visibility| candidate.rank() <= visibility.rank();
        let sources: Vec<SourceAnchor> = self
            .sources
            .iter()
            .filter(|s| keep(s.visibility))
            .cloned()
            .collect();
        let articles: Vec<Article> = self
            .articles
            .iter()
            .filter(|a| keep(a.visibility))
            .cloned()
            .collect();
        // Both conditions, not either: a chunk labelled `public` under an
        // article that did not survive would otherwise ship the text of a
        // restricted article as a free-standing public chunk.
        let retained: std::collections::BTreeSet<&str> =
            articles.iter().map(|a| a.id.as_str()).collect();
        let chunks: Vec<Chunk> = self
            .chunks
            .iter()
            .filter(|c| keep(c.visibility) && retained.contains(c.article_id.as_str()))
            .cloned()
            .collect();
        let mut bundle = Corpus {
            schema_version: self.schema_version.clone(),
            content_version: self.content_version.clone(),
            sources,
            articles,
            chunks,
            digest: String::new(),
            source_digest: String::new(),
        };
        bundle.rebind_set_digests();
        bundle
    }

    /// Recompute the set-level digests from the records this document carries.
    ///
    /// A filtered view is honestly a different document, so its corpus-level
    /// digest is recomputed while every record digest is preserved — a
    /// citation into it still verifies against the full corpus. This is one
    /// function because `bundle_at` and the host's `visible_corpus` must agree
    /// about what a filtered document's digest is; when each derived it
    /// separately they were free to diverge.
    pub fn rebind_set_digests(&mut self) {
        self.source_digest = source_set_digest_of(&self.sources);
        self.digest = corpus_digest_of(
            &self.schema_version,
            &self.content_version,
            &self
                .articles
                .iter()
                .map(|article| article.digest.as_str())
                .collect::<Vec<_>>(),
            &self
                .chunks
                .iter()
                .map(|chunk| chunk.digest.as_str())
                .collect::<Vec<_>>(),
            &self.source_digest,
        );
    }

    /// Canonical JSON bytes of this corpus, the exact form written to disk.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let value = serde_json::to_value(self).expect("corpus serializes");
        crate::digest::canonical_json(&value)
    }

    /// Digest over the corpus document as serialized, not over its records.
    #[must_use]
    pub fn document_digest(&self) -> String {
        let value = serde_json::to_value(self).expect("corpus serializes");
        canonical_digest(&value)
    }
}
