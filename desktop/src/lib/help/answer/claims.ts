/**
 * Claim-bound citation coverage.
 *
 * The previous check was a ratio: total quoted code points against total
 * answer length, with a loose multiplier. A ratio is not a binding. An answer
 * could make five claims, quote one long passage that supports only the first,
 * and pass — because nothing anywhere said *which* claim each citation was
 * evidence for. "The article says so somewhere" was rejected at the article
 * level and then re-admitted at the sentence level.
 *
 * This module makes the binding explicit and checkable:
 *
 * 1. **The answer is segmented into claims** deterministically, from its own
 *    bytes, by the validator — not by the provider. A provider that could
 *    choose its own segmentation could declare the whole answer one claim.
 * 2. **Every material claim must be covered.** Each citation names the claim
 *    index it supports; a material claim with no citation fails the response.
 *    Materiality is deliberately generous: a segment is immaterial only when
 *    it carries no letters or digits at all, so the fail-closed direction is
 *    "more sentences need evidence".
 * 3. **Every citation must be used.** A citation naming no claim, or a claim
 *    index that does not exist, fails the response. An unrelated quote pulled
 *    from context to pad the support budget has nowhere to attach.
 * 4. **Every citation must be relevant to the claim it names.** The quote must
 *    share content vocabulary with the claim. This is what rejects a citation
 *    that is verbatim, in-context, correctly bound — and about something else.
 *    Claims too short to have vocabulary are exempt from this one test, and
 *    only this one; see `HELP_CLAIM_MIN_TOKENS_FOR_RELEVANCE`.
 * 5. **Spans may not overlap.** Coverage is decided over distinct source
 *    bytes, so the same passage cannot be cited twice to look like two
 *    independent pieces of evidence.
 *
 * Offsets here are UTF-8 byte offsets into the answer, matching the
 * coordinate system spans use for the source side.
 */
import { helpSpansOverlap, type HelpClaimSpan } from "../retrieval/spans";
import { isStopWord, stem, rawTokens } from "../retrieval/text";

/** Most claims one answer may be segmented into. */
export const HELP_MAX_CLAIMS = 32;

/**
 * How much of a claim's vocabulary its quotes must actually contain.
 *
 * Calibrated for paraphrase, not for quotation: an answer is expected to
 * restate the source in its own words, so requiring every token would make
 * honest answers fail. Half of the distinct content tokens is enough to
 * separate "this quote is about this claim" from "this quote is about
 * something else in the same article", which is the distinction that matters.
 */
export const HELP_CLAIM_SUPPORT_FRACTION = 0.5;

/**
 * Fewest distinct content tokens a claim needs before relevance is decided.
 *
 * With one token there is nothing to measure: a quote either happens to
 * contain that word or it does not, and neither outcome distinguishes evidence
 * from coincidence. Short affirmatives — "Yes.", "No.", "Rarely." — are real
 * claims that no quote can share vocabulary with, so applying the test to them
 * would reject an honest answer rather than an unsupported one.
 *
 * Such claims still require a bound, verbatim, in-context, non-overlapping
 * citation. What is skipped is only the vocabulary comparison.
 *
 * Worth stating plainly: vocabulary overlap was never entailment. It separates
 * "this quote is about this claim" from "this quote is about something else in
 * the same article". It does not, and is not intended to, prove the quote
 * supports what the claim asserts.
 */
export const HELP_CLAIM_MIN_TOKENS_FOR_RELEVANCE = 2;

export type HelpAnswerClaim = {
  readonly index: number;
  /** The claim text, exactly as it appears in the answer. */
  readonly text: string;
  /** UTF-8 byte offsets into the answer. */
  readonly startUtf8: number;
  readonly endUtf8: number;
  /** False only for segments carrying no letters or digits at all. */
  readonly material: boolean;
  /** Distinct stemmed content tokens, for the relevance check. */
  readonly tokens: readonly string[];
};

export type HelpCoverageFailure =
  | "no-claims"
  | "too-many-claims"
  | "uncovered-claim"
  | "unbound-citation"
  | "unrelated-citation"
  | "overlapping-spans";

export type HelpCoverageResult =
  | { readonly ok: true; readonly claims: readonly HelpAnswerClaim[] }
  | {
      readonly ok: false;
      readonly reason: HelpCoverageFailure;
      readonly detail: string;
      readonly claims: readonly HelpAnswerClaim[];
    };

/** The minimum a citation must expose for coverage to be decided. */
export type HelpCoverageCitation = {
  readonly claimIndex: number;
  readonly quote: string;
  readonly span: HelpClaimSpan;
};

const ENCODER = new TextEncoder();

/**
 * Sentence-final punctuation that ends a claim.
 *
 * A period is only a boundary when the next character starts a new sentence,
 * which is what keeps `v1.2`, `0.5`, and `e.g.` from being split into
 * fragments that could never be supported.
 */
function isBoundary(text: string, index: number): boolean {
  const character = text[index];
  if (character === "\n") return true;
  if (character !== "." && character !== "!" && character !== "?") return false;

  // Trailing punctuation runs ("?!") belong to the same boundary.
  let cursor = index;
  while (cursor + 1 < text.length && ".!?".includes(text[cursor + 1] ?? "")) cursor += 1;

  const next = text.slice(cursor + 1);
  if (next.length === 0) return true;
  // A boundary needs whitespace after it: `v1.2` and `docs/a.md` do not split.
  if (!/^\s/.test(next)) return false;
  if (character !== ".") return true;

  // `e.g. like this` — a single letter or a known abbreviation before the dot
  // is not a sentence end.
  const before = text.slice(0, index);
  const lastWord = before.slice(before.lastIndexOf(" ") + 1).toLowerCase();
  if (/^[a-z]$/.test(lastWord)) return false;
  if (ABBREVIATIONS.has(lastWord)) return false;
  return true;
}

const ABBREVIATIONS = new Set(["e.g", "i.e", "etc", "vs", "cf", "fig", "no", "approx"]);

/**
 * Segment an answer into claims.
 *
 * Deterministic and validator-owned. Two identical answers always segment
 * identically, and a provider has no input into the segmentation at all.
 */
export function segmentHelpClaims(answer: string): readonly HelpAnswerClaim[] {
  const claims: HelpAnswerClaim[] = [];
  let segmentStartUtf16 = 0;
  let byteOffset = 0;
  let segmentStartUtf8 = 0;

  const push = (endUtf16: number, endUtf8: number) => {
    const raw = answer.slice(segmentStartUtf16, endUtf16);
    if (raw.trim().length === 0) return;
    // Trim only whitespace, and move the byte offsets with it, so a claim's
    // range names the claim rather than the space around it.
    const leading = raw.length - raw.trimStart().length;
    const trailing = raw.length - raw.trimEnd().length;
    const text = raw.trim();
    const startUtf8 =
      segmentStartUtf8 + ENCODER.encode(raw.slice(0, leading)).byteLength;
    const stopUtf8 = endUtf8 - ENCODER.encode(raw.slice(raw.length - trailing)).byteLength;
    const tokens = [
      ...new Set(
        rawTokens(text)
          .filter((token) => token.length >= 2 && !isStopWord(token))
          .map(stem),
      ),
    ];
    claims.push(
      Object.freeze({
        index: claims.length,
        text,
        startUtf8,
        endUtf8: stopUtf8,
        material: /[\p{L}\p{N}]/u.test(text),
        tokens: Object.freeze(tokens),
      }),
    );
  };

  for (let index = 0; index < answer.length; ) {
    const point = answer.codePointAt(index);
    const width = point !== undefined && point > 0xffff ? 2 : 1;
    const characterBytes = ENCODER.encode(answer.slice(index, index + width)).byteLength;
    byteOffset += characterBytes;
    if (isBoundary(answer, index)) {
      // Consume any run of trailing sentence punctuation into this claim.
      let cursor = index;
      let runBytes = byteOffset;
      while (cursor + 1 < answer.length && ".!?".includes(answer[cursor + 1] ?? "")) {
        cursor += 1;
        runBytes += 1;
      }
      push(cursor + 1, runBytes);
      segmentStartUtf16 = cursor + 1;
      segmentStartUtf8 = runBytes;
      byteOffset = runBytes;
      index = cursor + 1;
      continue;
    }
    index += width;
  }
  if (segmentStartUtf16 < answer.length) push(answer.length, byteOffset);
  return Object.freeze(claims);
}

/**
 * Decide whether a set of citations covers an answer's claims.
 *
 * Every branch fails closed. There is no partial acceptance: an answer whose
 * third sentence is uncited is not shown with two sentences highlighted, it is
 * rejected, because the reader cannot tell which part was the unsupported one.
 */
export function checkHelpClaimCoverage(
  answer: string,
  citations: readonly HelpCoverageCitation[],
): HelpCoverageResult {
  const claims = segmentHelpClaims(answer);
  if (claims.length === 0) {
    return { ok: false, reason: "no-claims", detail: "answer segmented into nothing", claims };
  }
  if (claims.length > HELP_MAX_CLAIMS) {
    return { ok: false, reason: "too-many-claims", detail: String(claims.length), claims };
  }

  // No two citations may claim the same source bytes.
  for (let left = 0; left < citations.length; left += 1) {
    for (let right = left + 1; right < citations.length; right += 1) {
      const a = citations[left];
      const b = citations[right];
      if (a && b && helpSpansOverlap(a.span, b.span)) {
        return {
          ok: false,
          reason: "overlapping-spans",
          detail: `citations ${left} and ${right} quote the same bytes of ${a.span.chunkId}`,
          claims,
        };
      }
    }
  }

  const covered = new Map<number, HelpCoverageCitation[]>();
  for (const [position, citation] of citations.entries()) {
    const claim = claims[citation.claimIndex];
    if (!claim || !claim.material) {
      return {
        ok: false,
        reason: "unbound-citation",
        detail: `citation ${position} names claim ${citation.claimIndex}, which is not a material claim of this answer`,
        claims,
      };
    }
    const existing = covered.get(citation.claimIndex);
    if (existing) existing.push(citation);
    else covered.set(citation.claimIndex, [citation]);
  }

  for (const claim of claims) {
    if (!claim.material) continue;
    const supporting = covered.get(claim.index);
    if (!supporting || supporting.length === 0) {
      return {
        ok: false,
        reason: "uncovered-claim",
        detail: `claim ${claim.index} has no citation: ${claim.text.slice(0, 80)}`,
        claims,
      };
    }
    // A claim too short to carry vocabulary ("Yes.") still needs a citation,
    // but there is nothing to measure relevance against. Bounding the citation
    // is the check that still applies.
    if (claim.tokens.length < HELP_CLAIM_MIN_TOKENS_FOR_RELEVANCE) continue;

    const quoted = new Set<string>();
    for (const citation of supporting) {
      const citationTokens = new Set(
        rawTokens(citation.quote)
          .filter((token) => token.length >= 2 && !isStopWord(token))
          .map(stem),
      );
      // Each individual citation must be about this claim. A quote sharing no
      // vocabulary with the sentence it is attached to is an unrelated
      // citation, however verbatim and in-context it is.
      if (!claim.tokens.some((token) => citationTokens.has(token))) {
        return {
          ok: false,
          reason: "unrelated-citation",
          detail: `a quote from ${citation.span.chunkId} shares no vocabulary with claim ${claim.index}`,
          claims,
        };
      }
      for (const token of citationTokens) quoted.add(token);
    }

    const matched = claim.tokens.filter((token) => quoted.has(token)).length;
    if (matched < Math.ceil(claim.tokens.length * HELP_CLAIM_SUPPORT_FRACTION)) {
      return {
        ok: false,
        reason: "unrelated-citation",
        detail: `claim ${claim.index} has ${matched}/${claim.tokens.length} of its vocabulary quoted`,
        claims,
      };
    }
  }

  return { ok: true, claims };
}
