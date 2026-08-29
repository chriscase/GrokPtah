//! Validate a raw provider reply against the exact bytes it was given.
//!
//! # The validator owns the claims
//!
//! A provider does not get to declare what it asserted or what supports it. It
//! returns prose; this module decides where one claim ends and the next
//! begins, and then decides — by searching the corpus bytes — whether anything
//! supports it. A model that labelled its own claims and attached its own
//! citations would be grading its own work, and the citation would prove only
//! that it was willing to write one.
//!
//! # Support means bytes, not resemblance
//!
//! A claim is supported when a long enough run of its own text occurs
//! *verbatim* in a chunk the request actually carried. The resulting
//! [`CitationSpan`] is a byte range into that chunk, carrying the chunk's
//! digest and its source's digest. That is what makes a citation checkable by
//! someone who did not produce it: the span names bytes, and the digest names
//! which bytes, so a rebuilt corpus invalidates the span instead of silently
//! re-pointing it at new text.
//!
//! Claims with no support are dropped rather than shown with a caveat. An
//! answer whose claims are all dropped is an abstention, and abstaining is a
//! result — not an error and not an empty answer with a hedge attached.

use grokptah_help_contract::corpus::Corpus;
use grokptah_help_contract::dto::{
    CitationSpan, Claim, HelpRequest, RedactionCount, ValidatedAnswer,
};

use crate::redact::redact;

/// Shortest verbatim run that counts as support, in characters.
///
/// Short enough that a real quoted phrase qualifies; long enough that common
/// connective text ("the provider", "you can") cannot make an unsupported
/// sentence look cited.
pub const MIN_QUOTE_CHARS: usize = 24;

/// Longest reply the validator will consider, in bytes.
pub const MAX_REPLY_BYTES: usize = 32_768;

/// Most claims one answer may contain.
pub const MAX_CLAIMS: usize = 32;

/// The outcome of validating one reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub answer: ValidatedAnswer,
    pub redactions: Vec<RedactionCount>,
    /// Claims the validator found but could not tie to corpus bytes.
    pub dropped_claims: usize,
}

impl Validation {
    #[must_use]
    pub fn span_count(&self) -> usize {
        self.answer
            .claims
            .iter()
            .map(|claim| claim.spans.len())
            .sum()
    }
}

/// Split prose into claims, keeping sentence terminators.
///
/// This is the validator's own segmentation. Splitting on the model's chosen
/// line breaks or numbering would let the reply decide how many claims it made
/// and therefore how much each citation appeared to cover.
fn split_claims(text: &str) -> Vec<String> {
    let mut claims: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        current.push(character);
        if matches!(character, '.' | '!' | '?') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                claims.push(trimmed.to_string());
            }
            current.clear();
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        claims.push(trimmed.to_string());
    }
    claims
}

/// The longest run of `needle` occurring verbatim in `haystack`.
///
/// Returns the byte range within `haystack`, which is always on character
/// boundaries because the search is over character indices.
fn longest_common_run(needle: &str, haystack: &str) -> Option<(usize, usize, usize)> {
    let needle_chars: Vec<(usize, char)> = needle.char_indices().collect();
    let hay_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    if needle_chars.is_empty() || hay_chars.is_empty() {
        return None;
    }

    // Rolling two-row DP: `previous[j]` is the run length ending at (i-1, j-1).
    let mut previous = vec![0usize; hay_chars.len() + 1];
    let mut current = vec![0usize; hay_chars.len() + 1];
    let mut best_len = 0usize;
    let mut best_hay_end = 0usize;

    for i in 1..=needle_chars.len() {
        for j in 1..=hay_chars.len() {
            current[j] = if needle_chars[i - 1].1 == hay_chars[j - 1].1 {
                previous[j - 1] + 1
            } else {
                0
            };
            if current[j] > best_len {
                best_len = current[j];
                best_hay_end = j;
            }
        }
        std::mem::swap(&mut previous, &mut current);
        current.iter_mut().for_each(|slot| *slot = 0);
    }

    if best_len == 0 {
        return None;
    }
    let start_char = best_hay_end - best_len;
    let start_byte = hay_chars[start_char].0;
    let end_byte = hay_chars
        .get(best_hay_end)
        .map_or(haystack.len(), |(offset, _)| *offset);
    Some((start_byte, end_byte, best_len))
}

/// Why a reply was rejected outright, before claim analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    Empty,
    TooLarge,
}

/// Validate `reply` against the exact bytes `request` carried.
///
/// # Errors
/// Returns [`RejectReason`] when the reply is empty or exceeds
/// [`MAX_REPLY_BYTES`]. Everything else is expressed as claims kept or
/// dropped, because a reply that is merely unsupported is an abstention rather
/// than a failure.
pub fn validate(
    reply: &str,
    request: &HelpRequest,
    corpus: &Corpus,
) -> Result<Validation, RejectReason> {
    if reply.trim().is_empty() {
        return Err(RejectReason::Empty);
    }
    if reply.len() > MAX_REPLY_BYTES {
        return Err(RejectReason::TooLarge);
    }

    let redacted = redact(reply);
    let mut claims: Vec<Claim> = Vec::new();
    let mut dropped = 0usize;

    for (ordinal, claim_text) in split_claims(&redacted.text).into_iter().enumerate() {
        if claims.len() >= MAX_CLAIMS {
            dropped += 1;
            continue;
        }
        let mut spans: Vec<CitationSpan> = Vec::new();

        for chunk in &request.context {
            // The request may only carry chunks that are still in the corpus,
            // with the bytes they claim. A drifted chunk supports nothing.
            let Some(known) = corpus.chunk(&chunk.chunk_id) else {
                continue;
            };
            if known.digest != chunk.chunk_digest || known.text != chunk.text {
                continue;
            }
            let Some((start, end, length)) = longest_common_run(&claim_text, &known.text) else {
                continue;
            };
            if length < MIN_QUOTE_CHARS {
                continue;
            }
            // Byte offsets must land on character boundaries of the exact text.
            if !known.text.is_char_boundary(start) || !known.text.is_char_boundary(end) {
                continue;
            }
            let Some(source_id) = known.source_ids.first() else {
                continue;
            };
            let Some(source) = corpus.source(source_id) else {
                continue;
            };

            spans.push(CitationSpan {
                chunk_id: known.id.clone(),
                chunk_digest: known.digest.clone(),
                source_id: source.id.clone(),
                source_digest: source.digest.clone(),
                start,
                end,
            });
        }

        // Keep only non-overlapping spans, longest first, so one claim cannot
        // count the same bytes twice to look better supported than it is.
        spans.sort_by(|left, right| {
            (right.end - right.start)
                .cmp(&(left.end - left.start))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        let mut kept: Vec<CitationSpan> = Vec::new();
        for span in spans {
            let overlaps = kept.iter().any(|existing| {
                existing.chunk_id == span.chunk_id
                    && span.start < existing.end
                    && existing.start < span.end
            });
            if !overlaps {
                kept.push(span);
            }
        }
        kept.sort_by(|left, right| {
            left.chunk_id
                .cmp(&right.chunk_id)
                .then_with(|| left.start.cmp(&right.start))
        });

        if kept.is_empty() {
            dropped += 1;
            continue;
        }
        claims.push(Claim {
            ordinal,
            text: claim_text,
            spans: kept,
        });
    }

    // Renumber so ordinals describe the answer that is actually served.
    for (position, claim) in claims.iter_mut().enumerate() {
        claim.ordinal = position;
    }

    let redactions: Vec<RedactionCount> = redacted
        .counts
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(kind, count)| RedactionCount {
            kind: *kind,
            count: *count,
        })
        .collect();

    let abstained = claims.is_empty();
    Ok(Validation {
        answer: ValidatedAnswer {
            request_id: request.request_id.clone(),
            corpus_digest: request.corpus_digest.clone(),
            claims,
            redactions: redacted.kinds(),
            abstained,
        },
        redactions,
        dropped_claims: dropped,
    })
}

/// Re-check a validated answer against the corpus.
///
/// Used to prove that everything served survives an independent pass, and
/// mirrored in TypeScript as defence in depth. It can only make a consumer
/// stricter: it never admits a claim the validator dropped.
#[must_use]
pub fn spans_resolve(answer: &ValidatedAnswer, corpus: &Corpus) -> bool {
    if answer.corpus_digest != corpus.digest {
        return false;
    }
    for claim in &answer.claims {
        if claim.spans.is_empty() {
            return false;
        }
        for span in &claim.spans {
            let Some(chunk) = corpus.chunk(&span.chunk_id) else {
                return false;
            };
            if chunk.digest != span.chunk_digest {
                return false;
            }
            if span.end > chunk.text.len() || span.start >= span.end {
                return false;
            }
            if !chunk.text.is_char_boundary(span.start) || !chunk.text.is_char_boundary(span.end) {
                return false;
            }
            let Some(source) = corpus.source(&span.source_id) else {
                return false;
            };
            if source.digest != span.source_digest {
                return false;
            }
        }
        // Non-overlap within a claim, per chunk.
        for (index, left) in claim.spans.iter().enumerate() {
            for right in claim.spans.iter().skip(index + 1) {
                if left.chunk_id == right.chunk_id
                    && left.start < right.end
                    && right.start < left.end
                {
                    return false;
                }
            }
        }
    }
    true
}

/// The exact quoted bytes a span names.
#[must_use]
pub fn quote_for<'a>(span: &CitationSpan, corpus: &'a Corpus) -> Option<&'a str> {
    let chunk = corpus.chunk(&span.chunk_id)?;
    if chunk.digest != span.chunk_digest {
        return None;
    }
    chunk.text.get(span.start..span.end)
}
