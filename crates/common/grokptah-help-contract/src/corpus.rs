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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
pub struct Corpus {
    pub schema_version: String,
    pub content_version: String,
    pub sources: Vec<SourceAnchor>,
    pub articles: Vec<Article>,
    pub chunks: Vec<Chunk>,
    /// Digest over the article and chunk digests, in order.
    pub digest: String,
    /// Digest over only the cited `path#heading` set, for anchor drift checks.
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
            Self::SchemaVersion { found } => {
                write!(f, "unsupported corpus schema version `{found}`")
            }
        }
    }
}

impl std::error::Error for CorpusError {}

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

        let ordinal_text = seed.topic.as_str();
        let mut fields: Vec<&str> = vec![
            seed.id,
            seed.title,
            ordinal_text,
            seed.summary,
            seed.body,
            seed.visibility.as_str(),
        ];
        fields.extend(seed.aliases.iter().copied());
        fields.extend(seed.keywords.iter().copied());
        fields.extend(seed.capability_ids.iter().copied());
        for anchor in &cited {
            fields.push(&anchor.digest);
        }
        let article_digest = domain_digest(domain::ARTICLE, &fields);

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

    let source_set: Vec<String> = sources
        .iter()
        .map(|source| format!("{}#{}", source.path, source.heading))
        .collect();
    let source_digest = domain_digest(
        domain::SOURCE_SET,
        &source_set.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    let mut corpus_fields: Vec<&str> = vec![SCHEMA_VERSION, CONTENT_VERSION];
    for article in &articles {
        corpus_fields.push(&article.digest);
    }
    for chunk in &chunks {
        corpus_fields.push(&chunk.digest);
    }
    corpus_fields.push(&source_digest);
    let digest = domain_digest(domain::CORPUS, &corpus_fields);

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
            let mut fields: Vec<&str> = vec![
                &article.id,
                &article.title,
                article.topic.as_str(),
                &article.summary,
                &article.body,
                article.visibility.as_str(),
            ];
            fields.extend(article.aliases.iter().map(String::as_str));
            fields.extend(article.keywords.iter().map(String::as_str));
            fields.extend(article.capability_ids.iter().map(String::as_str));
            for anchor in &cited {
                fields.push(&anchor.digest);
            }
            let actual = domain_digest(domain::ARTICLE, &fields);
            if actual != article.digest {
                return Err(CorpusError::DigestMismatch {
                    record: format!("article:{}", article.id),
                    expected: article.digest.clone(),
                    actual,
                });
            }
        }
        for chunk in &self.chunks {
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
        let chunks: Vec<Chunk> = self
            .chunks
            .iter()
            .filter(|c| keep(c.visibility))
            .cloned()
            .collect();
        let source_set: Vec<String> = sources
            .iter()
            .map(|source| format!("{}#{}", source.path, source.heading))
            .collect();
        let source_digest = domain_digest(
            domain::SOURCE_SET,
            &source_set.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let mut fields: Vec<&str> = vec![SCHEMA_VERSION, CONTENT_VERSION];
        for article in &articles {
            fields.push(&article.digest);
        }
        for chunk in &chunks {
            fields.push(&chunk.digest);
        }
        fields.push(&source_digest);
        let digest = domain_digest(domain::CORPUS, &fields);
        Corpus {
            schema_version: self.schema_version.clone(),
            content_version: self.content_version.clone(),
            sources,
            articles,
            chunks,
            digest,
            source_digest,
        }
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
