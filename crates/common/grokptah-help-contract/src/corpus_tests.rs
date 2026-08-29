//! Adversarial tests for what a corpus digest commits to.
//!
//! The digest used to length-prefix every field but say nothing about which
//! sub-list a field came from. `aliases ++ keywords ++ capability_ids` is the
//! same flat sequence however the items are partitioned, so an article's
//! `capability_ids` could be emptied into `aliases` and the article digest —
//! and the corpus digest above it — stayed byte-identical. `verify` passed,
//! and `Authority::manifest_for`, which gates on `capability_ids`, then served
//! a gated article to a principal holding no capability at all.
//!
//! These tests pin the four ways that partition can be attacked —
//! repartition, reorder, omission, duplication — plus the empty-versus-absent
//! case the counts exist to separate.

use crate::corpus::{
    ArticleSeed, Chunk, Corpus, CorpusError, SourceSeed, Topic, Visibility, article_digest_of,
};

/// The digest inputs of one article, as a base for single-mutation cases.
struct Article {
    aliases: Vec<&'static str>,
    keywords: Vec<&'static str>,
    capability_ids: Vec<&'static str>,
    source_digests: Vec<&'static str>,
}

impl Article {
    fn base() -> Self {
        Self {
            aliases: vec!["alias.a"],
            keywords: vec!["keyword.a"],
            capability_ids: vec!["cap.a"],
            source_digests: vec!["sha256:aa"],
        }
    }

    fn digest(&self) -> String {
        article_digest_of(
            "article.test",
            "Title",
            "operations",
            "Summary.",
            "Body.",
            "gated",
            &self.aliases,
            &self.keywords,
            &self.capability_ids,
            &self.source_digests,
        )
    }
}

#[test]
fn folding_every_list_into_aliases_changes_the_article_digest() {
    // The exact bypass: the flat sequence is unchanged, only the partition
    // moves. A capability that is now an alias is no longer required.
    let base = Article::base();
    let repartitioned = Article {
        aliases: vec!["alias.a", "keyword.a", "cap.a"],
        keywords: vec![],
        capability_ids: vec![],
        source_digests: vec!["sha256:aa"],
    };
    assert_ne!(base.digest(), repartitioned.digest());
}

#[test]
fn moving_one_capability_one_list_over_changes_the_article_digest() {
    let base = Article::base();
    let moved = Article {
        aliases: vec!["alias.a"],
        keywords: vec!["keyword.a", "cap.a"],
        capability_ids: vec![],
        source_digests: vec!["sha256:aa"],
    };
    assert_ne!(base.digest(), moved.digest());
}

#[test]
fn moving_a_source_digest_into_capabilities_changes_the_article_digest() {
    // The trailing list is not exempt: dropping a citation while parking its
    // digest in `capability_ids` also left the flat sequence unchanged.
    let base = Article::base();
    let moved = Article {
        aliases: vec!["alias.a"],
        keywords: vec!["keyword.a"],
        capability_ids: vec!["cap.a", "sha256:aa"],
        source_digests: vec![],
    };
    assert_ne!(base.digest(), moved.digest());
}

#[test]
fn reordering_within_a_list_changes_the_article_digest() {
    let forward = Article {
        aliases: vec!["alias.a", "alias.b"],
        ..Article::base()
    };
    let reversed = Article {
        aliases: vec!["alias.b", "alias.a"],
        ..Article::base()
    };
    assert_ne!(forward.digest(), reversed.digest());
}

#[test]
fn omitting_an_element_changes_the_article_digest() {
    let base = Article::base();
    let omitted = Article {
        capability_ids: vec![],
        ..Article::base()
    };
    assert_ne!(base.digest(), omitted.digest());
}

#[test]
fn duplicating_an_element_changes_the_article_digest() {
    let base = Article::base();
    let duplicated = Article {
        capability_ids: vec!["cap.a", "cap.a"],
        ..Article::base()
    };
    assert_ne!(base.digest(), duplicated.digest());
}

#[test]
fn an_absent_list_differs_from_a_list_holding_one_empty_string() {
    // Without the count these encode identically: an empty string contributes
    // `0:` and so does no element at all.
    let absent = Article {
        aliases: vec![],
        keywords: vec![],
        capability_ids: vec![],
        ..Article::base()
    };
    let one_empty = Article {
        aliases: vec![""],
        keywords: vec![],
        capability_ids: vec![],
        ..Article::base()
    };
    assert_ne!(absent.digest(), one_empty.digest());
}

#[test]
fn a_label_in_the_data_cannot_impersonate_a_region_marker() {
    // This is the case the *counts* exist for, not just the labels. Labelling
    // each region without counting it is still ambiguous: an alias whose text
    // is the next region's label produces the identical label sequence.
    //
    //   aliases ["keywords"], keywords []  -> aliases "keywords" keywords ...
    //   aliases [],  keywords ["keywords"] -> aliases keywords "keywords" ...
    //
    // Both flatten to the same list of strings. Only the element counts tell
    // them apart, so this test fails if a port keeps the labels and drops the
    // counts.
    let alias_named_like_a_label = Article {
        aliases: vec!["keywords"],
        keywords: vec![],
        capability_ids: vec![],
        ..Article::base()
    };
    let keyword_of_that_name = Article {
        aliases: vec![],
        keywords: vec!["keywords"],
        capability_ids: vec![],
        ..Article::base()
    };
    assert_ne!(
        alias_named_like_a_label.digest(),
        keyword_of_that_name.digest()
    );
}

// ---------------------------------------------------------------------------
// The same attacks against a whole document.
// ---------------------------------------------------------------------------

fn corpus() -> Corpus {
    crate::build_corpus()
}

fn gated_article_id(corpus: &Corpus) -> String {
    corpus
        .articles
        .iter()
        .find(|article| !article.capability_ids.is_empty())
        .expect("the authored corpus gates at least one article")
        .id
        .clone()
}

#[test]
fn the_authored_corpus_verifies() {
    corpus().verify().expect("authored corpus is consistent");
}

#[test]
fn every_authored_source_resolves_in_the_exact_repository_tree() {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .ancestors()
        .nth(3)
        .expect("Help contract crate is three levels below the repository root");
    corpus()
        .verify_source_anchors(root)
        .expect("every canonical path and Markdown heading resolves");
}

#[test]
fn source_anchor_verification_rejects_missing_headings_and_path_escape() {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .ancestors()
        .nth(3)
        .expect("Help contract crate is three levels below the repository root");

    let mut missing = corpus();
    missing.sources[0].heading = "This heading does not exist".to_string();
    assert!(matches!(
        missing.verify_source_anchors(root),
        Err(super::corpus::SourceAnchorError::MissingHeading { .. })
    ));

    let mut escaping = corpus();
    escaping.sources[0].path = "../README.md".to_string();
    assert!(matches!(
        escaping.verify_source_anchors(root),
        Err(super::corpus::SourceAnchorError::UnsafePath { .. })
    ));
}

#[test]
fn verify_rejects_a_stale_source_set_digest() {
    let mut tampered = corpus();
    tampered.source_digest = "sha256:stale".to_string();
    let error = tampered
        .verify()
        .expect_err("the source-set digest is verified");
    assert!(
        matches!(error, CorpusError::DigestMismatch { ref record, .. } if record == "source-set"),
        "expected a source-set mismatch, got {error:?}"
    );
}

#[test]
fn verify_rejects_a_stale_top_level_corpus_digest() {
    let mut tampered = corpus();
    tampered.content_version.push_str(".tampered");
    let error = tampered
        .verify()
        .expect_err("the corpus digest is verified");
    assert!(
        matches!(error, CorpusError::DigestMismatch { ref record, .. } if record == "corpus"),
        "expected a corpus mismatch, got {error:?}"
    );
}

#[test]
fn source_set_digest_has_no_path_heading_separator_alias() {
    let left = crate::corpus::build(
        &[SourceSeed {
            id: "source",
            path: "a#b",
            heading: "c",
            visibility: Visibility::Public,
        }],
        &[],
    )
    .expect("left corpus builds");
    let right = crate::corpus::build(
        &[SourceSeed {
            id: "source",
            path: "a",
            heading: "b#c",
            visibility: Visibility::Public,
        }],
        &[],
    )
    .expect("right corpus builds");
    assert_ne!(left.source_digest, right.source_digest);
}

#[test]
fn verify_rejects_duplicate_source_ids_before_lookup() {
    let mut tampered = corpus();
    tampered.sources.push(tampered.sources[0].clone());
    let id = tampered.sources[0].id.clone();
    let error = tampered
        .verify()
        .expect_err("duplicate sources are ambiguous");
    assert_eq!(error, CorpusError::DuplicateId { id });
}

#[test]
fn verify_rejects_duplicate_article_ids_before_visibility_projection() {
    let mut tampered = corpus();
    let public = tampered
        .articles
        .iter()
        .find(|article| article.visibility == Visibility::Public)
        .expect("public article")
        .clone();
    let mut restricted = tampered
        .articles
        .iter()
        .find(|article| article.visibility != Visibility::Public)
        .expect("restricted article")
        .clone();
    restricted.id.clone_from(&public.id);
    tampered.articles.push(restricted);
    let error = tampered
        .verify()
        .expect_err("a restricted duplicate cannot shadow a public record");
    assert_eq!(error, CorpusError::DuplicateId { id: public.id });
}

#[test]
fn verify_rejects_duplicate_chunk_ids_before_lookup() {
    let mut tampered = corpus();
    tampered.chunks.push(tampered.chunks[0].clone());
    let id = tampered.chunks[0].id.clone();
    let error = tampered
        .verify()
        .expect_err("duplicate chunks are ambiguous");
    assert_eq!(error, CorpusError::DuplicateId { id });
}

#[test]
fn builder_rejects_a_chunk_id_colliding_with_a_source_id() {
    const ARTICLE_ID: &str = "article";
    const COLLISION: &str = "article#title.0";
    const SOURCE_IDS: &[&str] = &[COLLISION];
    let error = crate::corpus::build(
        &[SourceSeed {
            id: COLLISION,
            path: "README.md",
            heading: "Help",
            visibility: Visibility::Public,
        }],
        &[ArticleSeed {
            id: ARTICLE_ID,
            title: "Title",
            topic: Topic::GettingStarted,
            summary: "Summary.",
            body: "Body.",
            aliases: &[],
            keywords: &[],
            source_ids: SOURCE_IDS,
            visibility: Visibility::Public,
            capability_ids: &[],
        }],
    )
    .expect_err("record ids share one namespace");
    assert_eq!(
        error,
        CorpusError::DuplicateId {
            id: COLLISION.to_string()
        }
    );
}

#[test]
fn repartitioning_a_capability_is_rejected_by_verify() {
    let mut tampered = corpus();
    let target = gated_article_id(&tampered);
    for article in &mut tampered.articles {
        if article.id == target {
            let mut folded = article.aliases.clone();
            folded.extend(article.keywords.clone());
            folded.extend(article.capability_ids.clone());
            article.aliases = folded;
            article.keywords = Vec::new();
            // The gate is gone; the stored digest is deliberately untouched.
            article.capability_ids = Vec::new();
        }
    }
    let error = tampered.verify().expect_err("repartition must be refused");
    assert!(
        matches!(error, CorpusError::DigestMismatch { ref record, .. } if record == &format!("article:{target}")),
        "expected a digest mismatch on the repartitioned article, got {error:?}"
    );
}

#[test]
fn a_chunk_more_restricted_than_its_article_is_rejected() {
    // The projection leak: filtering chunks by article would hand this
    // operator chunk to a public reader.
    let mut tampered = corpus();
    let article_id = tampered
        .articles
        .iter()
        .find(|article| article.visibility == Visibility::Public)
        .expect("a public article")
        .id
        .clone();
    let chunk_id = retag_first_chunk(&mut tampered, &article_id, Visibility::Operator);
    let error = tampered
        .verify()
        .expect_err("a mismatched chunk is refused");
    assert!(
        matches!(
            error,
            CorpusError::ChunkVisibilityMismatch { ref chunk, .. } if chunk == &chunk_id
        ),
        "expected a chunk visibility mismatch, got {error:?}"
    );
}

#[test]
fn a_chunk_less_restricted_than_its_article_is_rejected() {
    // The other direction, which `bundle_at` would ship: a public chunk whose
    // article is operator-only carries restricted prose into a public bundle.
    let mut tampered = corpus();
    let article_id = tampered
        .articles
        .iter()
        .find(|article| article.visibility == Visibility::Operator)
        .expect("an operator article")
        .id
        .clone();
    let chunk_id = retag_first_chunk(&mut tampered, &article_id, Visibility::Public);
    let error = tampered
        .verify()
        .expect_err("a mismatched chunk is refused");
    assert!(
        matches!(
            error,
            CorpusError::ChunkVisibilityMismatch { ref chunk, .. } if chunk == &chunk_id
        ),
        "expected a chunk visibility mismatch, got {error:?}"
    );
}

#[test]
fn a_chunk_naming_an_unknown_article_is_rejected() {
    let mut tampered = corpus();
    let orphan = tampered.chunks[0].clone();
    tampered
        .articles
        .retain(|article| article.id != orphan.article_id);
    let error = tampered.verify().expect_err("an orphan chunk is refused");
    assert!(
        matches!(error, CorpusError::UnknownArticle { ref chunk, .. } if chunk == &orphan.id),
        "expected an unknown-article error, got {error:?}"
    );
}

#[test]
fn a_chunk_without_sources_is_rejected_even_after_redigesting() {
    let mut tampered = corpus();
    let chunk = &mut tampered.chunks[0];
    chunk.source_ids.clear();
    chunk.digest = chunk_digest_of(chunk);
    tampered.rebind_set_digests();
    assert!(matches!(
        tampered.verify(),
        Err(CorpusError::EmptyChunkSources { .. })
    ));
}

#[test]
fn a_chunk_citing_an_unknown_source_is_rejected_even_after_redigesting() {
    let mut tampered = corpus();
    let chunk = &mut tampered.chunks[0];
    chunk.source_ids = vec!["unknown.source".to_string()];
    chunk.digest = chunk_digest_of(chunk);
    tampered.rebind_set_digests();
    assert!(matches!(
        tampered.verify(),
        Err(CorpusError::UnknownChunkSource { .. })
    ));
}

#[test]
fn a_public_chunk_citing_a_restricted_source_is_rejected() {
    let mut tampered = corpus();
    let restricted = tampered
        .sources
        .iter()
        .find(|source| source.visibility == Visibility::Operator)
        .expect("operator source")
        .id
        .clone();
    let chunk = tampered
        .chunks
        .iter_mut()
        .find(|chunk| chunk.visibility == Visibility::Public)
        .expect("public chunk");
    chunk.source_ids = vec![restricted];
    chunk.digest = chunk_digest_of(chunk);
    tampered.rebind_set_digests();
    assert!(matches!(
        tampered.verify(),
        Err(CorpusError::ChunkSourceVisibilityMismatch { .. })
    ));
}

#[test]
fn a_chunk_cannot_substitute_a_different_public_source() {
    let mut tampered = corpus();
    let index = tampered
        .chunks
        .iter()
        .position(|chunk| chunk.visibility == Visibility::Public)
        .expect("public chunk");
    let original_sources = tampered.chunks[index].source_ids.clone();
    let replacement = tampered
        .sources
        .iter()
        .find(|source| {
            source.visibility == Visibility::Public && !original_sources.contains(&source.id)
        })
        .expect("another public source")
        .id
        .clone();
    let chunk = &mut tampered.chunks[index];
    chunk.source_ids = vec![replacement];
    chunk.digest = chunk_digest_of(chunk);
    tampered.rebind_set_digests();
    assert!(matches!(
        tampered.verify(),
        Err(CorpusError::ChunkSourcesMismatch { .. })
    ));
}

#[test]
fn the_corpus_digest_binds_the_article_and_chunk_counts() {
    // Dropping a record changes the document even though every surviving
    // record digest is untouched.
    let full = corpus();
    let mut shortened = full.clone();
    shortened.chunks.pop();
    shortened.rebind_set_digests();
    assert_ne!(full.digest, shortened.digest);
}

#[test]
fn a_bundle_drops_a_chunk_whose_article_did_not_survive() {
    // Independently-filtered lists could otherwise ship a chunk of a
    // restricted article as a free-standing public record.
    let mut crafted = corpus();
    let operator_article = crafted
        .articles
        .iter()
        .find(|article| article.visibility == Visibility::Operator)
        .expect("an operator article")
        .id
        .clone();
    let chunk_id = retag_first_chunk(&mut crafted, &operator_article, Visibility::Public);
    let bundle = crafted.bundle_at(Visibility::Public);
    assert!(
        !bundle.chunks.iter().any(|chunk| chunk.id == chunk_id),
        "a public bundle must not carry a chunk of an article it excluded"
    );
}

/// Re-label the first chunk of `article_id` and re-mint only that chunk's own
/// digest, so the document fails on the visibility rule rather than trivially
/// on a stale chunk digest.
fn retag_first_chunk(corpus: &mut Corpus, article_id: &str, visibility: Visibility) -> String {
    let mut retagged = String::new();
    for chunk in &mut corpus.chunks {
        if chunk.article_id == article_id && retagged.is_empty() {
            chunk.visibility = visibility;
            chunk.digest = chunk_digest_of(chunk);
            retagged = chunk.id.clone();
        }
    }
    assert!(!retagged.is_empty(), "article `{article_id}` has a chunk");
    corpus.rebind_set_digests();
    retagged
}

fn chunk_digest_of(chunk: &Chunk) -> String {
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
    crate::digest::domain_digest(crate::digest::domain::CHUNK, &fields)
}

// ---------------------------------------------------------------------------
// Unknown fields at the deserialization boundary.
// ---------------------------------------------------------------------------

#[test]
fn a_corpus_carrying_an_unknown_field_is_refused() {
    // A digest covers the fields it knows about. An unrecognised field rides
    // alongside untouched by verification, so it is refused at parse instead.
    let mut document: serde_json::Value =
        serde_json::from_str(crate::CORPUS_JSON).expect("the artifact parses");
    document["articles"][0]["injected"] = serde_json::json!("payload");

    let parsed: Result<Corpus, _> = serde_json::from_value(document);
    let error = parsed.expect_err("an unknown article field must be refused");
    assert!(
        error.to_string().contains("injected"),
        "the refusal should name the unknown field, got: {error}"
    );
}

#[test]
fn the_committed_artifact_still_parses_under_the_stricter_rule() {
    // The other half of the check above: strictness that also rejected the
    // real artifact would be a broken gate rather than a tighter one.
    let parsed: Corpus =
        serde_json::from_str(crate::CORPUS_JSON).expect("the committed artifact parses");
    parsed.verify().expect("and verifies");
}
