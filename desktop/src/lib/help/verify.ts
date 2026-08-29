/**
 * Defence in depth: re-check a projection the host already validated.
 *
 * The host is the authority. This pass exists because a check that can only
 * make a consumer *stricter* costs nothing to run and catches the case the
 * host cannot: a projection altered after it left the host, or a
 * published-package consumer talking to a server that is not this one.
 *
 * It can never admit a claim the host dropped — there is no path here that
 * adds a claim, relaxes a citation, or supplies a missing one. It only
 * removes.
 */

import type { HelpCorpus, HelpProjection } from "./generated/contract";
import { HELP_CORPUS } from "./canonical/corpus";

// `HELP_CORPUS` here is the public corpus this bundle ships. A caller entitled
// to more passes the host-filtered corpus explicitly: verifying an operator's
// answer against the public corpus would drop every citation into gated
// content, which reads as "the host lied" rather than "you passed the wrong
// corpus".

/** Why a claim was rejected on the second pass. */
export type HelpClaimRejection =
  | "no-citation"
  | "unknown-source"
  | "quote-not-in-corpus"
  | "not-plain-text";

export type HelpVerification = {
  readonly projection: HelpProjection;
  readonly rejected: readonly { readonly ordinal: number; readonly reason: HelpClaimRejection }[];
};

/**
 * Characters that must never survive validation into rendered text.
 *
 * C0 and C1 controls can rewrite what a terminal or log already printed; the
 * bidirectional overrides reorder rendered text without changing its bytes, so
 * a citation can be made to display as its own opposite while still matching
 * the corpus exactly.
 */
/*
 * Written as escapes, not as literal bytes.
 *
 * The literal form made this file binary to git, so the one class that decides
 * what text may reach a renderer could not be read in a diff. The set is
 * unchanged and `helpAdversarial.test.ts` pins it: C0 controls except tab and
 * newline, C1 controls, the LTR/RTL marks, the bidi embedding and override
 * range, the bidi isolates, and the Arabic letter mark.
 */
export const HELP_FORBIDDEN_CHARACTERS =
  /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u200E\u200F\u202A-\u202E\u2066-\u2069\u061C]/u;

/** Markup is not plain text, and rendered prose is the only thing served. */
const MARKUP = /[<>`]|\]\(/u;

function isPlainText(value: string): boolean {
  return !HELP_FORBIDDEN_CHARACTERS.test(value) && !MARKUP.test(value);
}

/**
 * Drop any claim that cannot be re-checked against the corpus in hand.
 *
 * A citation whose quote is not a substring of its source's own chunks is not
 * a citation — it is a quotation of something else. Dropping the claim is the
 * only safe reading: the alternative shows text the corpus does not support
 * beside a source name that implies it does.
 */
export function verifyHelpProjection(
  projection: HelpProjection,
  corpus: HelpCorpus = HELP_CORPUS,
): HelpVerification {
  const rejected: { ordinal: number; reason: HelpClaimRejection }[] = [];
  const kept: HelpProjection["claims"][number][] = [];

  for (const claim of projection.claims) {
    if (!isPlainText(claim.text)) {
      rejected.push({ ordinal: claim.ordinal, reason: "not-plain-text" });
      continue;
    }
    if (claim.citations.length === 0) {
      rejected.push({ ordinal: claim.ordinal, reason: "no-citation" });
      continue;
    }
    let failure: HelpClaimRejection | null = null;
    for (const citation of claim.citations) {
      const source = corpus.sources.find((candidate) => candidate.id === citation.source_id);
      if (!source || source.path !== citation.path || source.heading !== citation.heading) {
        failure = "unknown-source";
        break;
      }
      if (!isPlainText(citation.quote)) {
        failure = "not-plain-text";
        break;
      }
      const supported = corpus.chunks.some(
        (chunk) =>
          chunk.source_ids.includes(citation.source_id) && chunk.text.includes(citation.quote),
      );
      if (!supported) {
        failure = "quote-not-in-corpus";
        break;
      }
    }
    if (failure) {
      rejected.push({ ordinal: claim.ordinal, reason: failure });
      continue;
    }
    kept.push(claim);
  }

  const status =
    kept.length === 0 && projection.status === "answered" ? "abstained" : projection.status;

  return {
    projection: {
      ...projection,
      status,
      claims: kept.map((claim, ordinal) => ({ ...claim, ordinal })),
    },
    rejected,
  };
}
