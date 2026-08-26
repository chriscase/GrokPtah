import { describe, expect, it } from "vitest";
import {
  HELP_MAX_CLAIMS,
  checkHelpClaimCoverage,
  segmentHelpClaims,
  type HelpCoverageCitation,
} from "./answer/claims";
import type { HelpClaimSpan } from "./retrieval/spans";

const ENCODER = new TextEncoder();

function span(chunkId: string, startUtf8: number, endUtf8: number, quote: string): HelpClaimSpan {
  return {
    chunkId,
    chunkDigest: "sha256:chunk",
    startUtf16: startUtf8,
    endUtf16: endUtf8,
    startCodePoint: startUtf8,
    endCodePoint: endUtf8,
    startUtf8,
    endUtf8,
    quote,
  };
}

function citation(claimIndex: number, quote: string, chunkId = "c1", start = 0): HelpCoverageCitation {
  return {
    claimIndex,
    quote,
    span: span(chunkId, start, start + ENCODER.encode(quote).byteLength, quote),
  };
}

describe("claim segmentation", () => {
  it("splits on sentence boundaries and reports UTF-8 ranges", () => {
    const answer = "A durable run resumes from its checkpoint. Quota is enforced separately.";
    const claims = segmentHelpClaims(answer);
    expect(claims.map((claim) => claim.text)).toEqual([
      "A durable run resumes from its checkpoint.",
      "Quota is enforced separately.",
    ]);
    for (const claim of claims) {
      const bytes = ENCODER.encode(answer).slice(claim.startUtf8, claim.endUtf8);
      expect(new TextDecoder().decode(bytes)).toBe(claim.text);
    }
  });

  it("keeps UTF-8 ranges correct across multi-byte and astral characters", () => {
    const answer = "Reanudación segura ✅. Prueba 𝔘nicode aquí.";
    const claims = segmentHelpClaims(answer);
    expect(claims.length).toBe(2);
    const bytes = ENCODER.encode(answer);
    for (const claim of claims) {
      expect(new TextDecoder().decode(bytes.slice(claim.startUtf8, claim.endUtf8))).toBe(claim.text);
    }
  });

  it("does not split inside a version, a path, or a common abbreviation", () => {
    // Each of these would become an unsupportable fragment if `.` alone ended
    // a sentence.
    for (const answer of [
      "Upgrade to v1.2 before resuming.",
      "The anchor lives in docs/OPERATIONS.md today.",
      "Some steps, e.g. rotation, need a lease.",
      "Ask J. Doe to approve it.",
    ]) {
      expect(segmentHelpClaims(answer).length, answer).toBe(1);
    }
  });

  it("treats a run of terminal punctuation as one boundary", () => {
    expect(segmentHelpClaims("Really?! Yes.").map((claim) => claim.text)).toEqual([
      "Really?!",
      "Yes.",
    ]);
  });

  it("splits on newlines so a bulleted answer is not one giant claim", () => {
    const claims = segmentHelpClaims("- Resume from a checkpoint\n- Re-check the lease");
    expect(claims.length).toBe(2);
  });

  it("marks a segment with no letters or digits immaterial", () => {
    const claims = segmentHelpClaims("Resume safely.\n---\nQuota is separate.");
    const immaterial = claims.filter((claim) => !claim.material);
    expect(immaterial.map((claim) => claim.text)).toEqual(["---"]);
  });
});

describe("claim coverage", () => {
  const answer = "A durable run resumes from its checkpoint. Quota is enforced separately.";
  const forFirst = "A durable run resumes from its checkpoint";
  const forSecond = "Quota is enforced separately";

  it("accepts an answer whose every claim is covered by a relevant quote", () => {
    const result = checkHelpClaimCoverage(answer, [
      citation(0, forFirst, "c1", 0),
      citation(1, forSecond, "c2", 0),
    ]);
    expect(result.ok).toBe(true);
  });

  it("rejects an answer with an uncited sentence", () => {
    const result = checkHelpClaimCoverage(answer, [citation(0, forFirst)]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("uncovered-claim");
  });

  it("rejects a citation bound to no claim of this answer", () => {
    const result = checkHelpClaimCoverage(answer, [
      citation(0, forFirst, "c1", 0),
      citation(1, forSecond, "c2", 0),
      citation(9, forSecond, "c3", 0),
    ]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("unbound-citation");
  });

  it("rejects a quote that shares no vocabulary with the claim it names", () => {
    const result = checkHelpClaimCoverage(answer, [
      citation(0, "Loopback providers rotate credentials hourly", "c1", 0),
      citation(1, forSecond, "c2", 0),
    ]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("unrelated-citation");
  });

  it("rejects a quote that pins only an incidental word of the claim", () => {
    // Shares "run", so the per-citation relevance check passes; still nowhere
    // near covering what the sentence asserts.
    const result = checkHelpClaimCoverage(answer, [
      citation(0, "run", "c1", 0),
      citation(1, forSecond, "c2", 0),
    ]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("unrelated-citation");
  });

  it("rejects two citations quoting the same source bytes", () => {
    const result = checkHelpClaimCoverage(answer, [
      citation(0, forFirst, "c1", 0),
      { ...citation(0, forFirst, "c1", 10), quote: forFirst },
      citation(1, forSecond, "c2", 0),
    ]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("overlapping-spans");
  });

  it("counts adjacent, disjoint ranges as separate evidence", () => {
    const first = "A durable run resumes";
    const second = "from its checkpoint";
    const result = checkHelpClaimCoverage("A durable run resumes from its checkpoint.", [
      citation(0, first, "c1", 0),
      citation(0, second, "c1", ENCODER.encode(first).byteLength),
    ]);
    expect(result.ok).toBe(true);
  });

  it("still requires a citation for a claim too short to have vocabulary", () => {
    const uncited = checkHelpClaimCoverage("Yes.", []);
    expect(uncited.ok).toBe(false);
    if (!uncited.ok) expect(uncited.reason).toBe("uncovered-claim");

    // One token is not vocabulary, so relevance has nothing to measure. The
    // binding still applies, and that is what this asserts.
    const cited = checkHelpClaimCoverage("Yes.", [citation(0, "Resume from a checkpoint")]);
    expect(cited.ok).toBe(true);
  });

  it("applies relevance as soon as a claim has vocabulary to compare", () => {
    // Two tokens is the threshold; a two-token claim is checked normally, so
    // the exemption cannot be reached by padding a claim with one more word.
    const result = checkHelpClaimCoverage("Quota enforced.", [
      citation(0, "Resume from a checkpoint"),
    ]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("unrelated-citation");
  });

  it("does not require evidence for a purely structural segment", () => {
    const result = checkHelpClaimCoverage("Resume safely.\n---", [
      citation(0, "Resume safely"),
    ]);
    expect(result.ok).toBe(true);
  });

  it("refuses an answer segmented into more claims than it will decide", () => {
    const many = Array.from({ length: HELP_MAX_CLAIMS + 1 }, (_, index) => `Claim ${index}.`).join(" ");
    const result = checkHelpClaimCoverage(many, []);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("too-many-claims");
  });

  it("refuses an answer that segments into nothing", () => {
    const result = checkHelpClaimCoverage("   ", []);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("no-claims");
  });
});
