import { describe, expect, it } from "vitest";
import {
  buildHelpAnswerRequest,
  parseHelpAnswerResponse,
  validateHelpAnswerResponse,
  HELP_ANSWER_CONTRACT,
  HELP_ANSWER_DEFAULT_TIMEOUT_MS,
  HELP_ANSWER_MAX_CITATIONS,
  HELP_ANSWER_MAX_TEXT_CHARS,
  HELP_ANSWER_MAX_TIMEOUT_MS,
  HELP_ANSWER_MIN_TIMEOUT_MS,
  type HelpAnswerRequest,
} from "./helpAnswer";
import {
  createHelpAuthority,
  HELP_AUTHORITY_CORPUS_VERSION,
  HELP_AUTHORITY_DIGEST,
} from "./helpAuthority";

const authority = createHelpAuthority();

function answerableRequest(): HelpAnswerRequest {
  const result = authority.search("restricted company gateway", { includeRestricted: true });
  expect(result.outcome).toBe("answer");
  const built = buildHelpAnswerRequest(result);
  expect(built.ok).toBe(true);
  return (built as { ok: true; request: HelpAnswerRequest }).request;
}

describe("optional AI answer request", () => {
  it("is bound to the exact corpus the retrieval used", () => {
    const request = answerableRequest();
    expect(request.schema).toBe(HELP_ANSWER_CONTRACT);
    expect(request.corpusVersion).toBe(HELP_AUTHORITY_CORPUS_VERSION);
    expect(request.corpusDigest).toBe(HELP_AUTHORITY_DIGEST);
    expect(request.retrievalMode).toBe("offline-hybrid");
    expect(request.query).toBe("restricted company gateway");
    expect(Object.isFrozen(request)).toBe(true);
  });

  it("grants no tools and no persistence, and requires confirmation", () => {
    const request = answerableRequest();
    expect(request.tools).toBe("none");
    expect(request.persistence).toBe("none");
    expect(request.requiresConfirmation).toBe(true);
    // The payload is a value, not a channel: nothing here can send it.
    for (const key of ["url", "endpoint", "apiKey", "token", "headers", "fetch", "transport"]) {
      expect(Object.keys(request)).not.toContain(key);
    }
  });

  it("classifies privacy as help corpus plus the user's own question", () => {
    const request = answerableRequest();
    expect(request.privacy).toEqual({
      classification: "help-corpus-and-user-query",
      containsUserQuery: true,
      containsHelpCorpus: true,
      containsWorkspaceData: false,
      containsSessionData: false,
      containsCredentials: false,
      containsFilesystemPaths: false,
      retention: "none",
    });
    // The claim must hold: no repository path leaks through cited context.
    expect(request.citedContext).not.toMatch(/\/Users\/|\/home\/|GROKPTAH_HOME/);
  });

  it("declares provider, model, cost, and latency as unknown", () => {
    const request = answerableRequest();
    expect(request.unknowns.provider).toBe("unknown");
    expect(request.unknowns.model).toBe("unknown");
    expect(request.unknowns.cost).toBe("unknown");
    expect(request.unknowns.latency).toBe("unknown");
    expect(request.unknowns.note).toMatch(/must not be inferred/i);
  });

  it("carries a bounded timeout that the caller must choose within", () => {
    expect(answerableRequest().timeoutMs).toBe(HELP_ANSWER_DEFAULT_TIMEOUT_MS);
    const result = authority.search("restricted company gateway", { includeRestricted: true });
    for (const timeoutMs of [0, -1, 500, 1.5, HELP_ANSWER_MAX_TIMEOUT_MS + 1, Number.NaN]) {
      const built = buildHelpAnswerRequest(result, { timeoutMs });
      expect(built.ok, String(timeoutMs)).toBe(false);
      expect(built.ok === false && built.refusal, String(timeoutMs)).toBe("invalid-timeout");
    }
    for (const timeoutMs of [HELP_ANSWER_MIN_TIMEOUT_MS, HELP_ANSWER_MAX_TIMEOUT_MS]) {
      const built = buildHelpAnswerRequest(result, { timeoutMs });
      expect(built.ok).toBe(true);
    }
  });

  it("carries citation spans that resolve back to the corpus", () => {
    const request = answerableRequest();
    expect(request.citations.length).toBeGreaterThan(0);
    for (const citation of request.citations) {
      const article = authority.article(citation.articleId);
      expect(article, citation.articleId).not.toBeNull();
      expect(citation.title).toBe(article!.title);
      expect(citation.sourceIds).toEqual(article!.sources.map((source) => source.id));
      for (const span of citation.spans) {
        expect(authority.resolveSpan({
          articleId: citation.articleId,
          field: span.field,
          passageId: span.passageId,
          start: span.start,
          end: span.end,
          quote: span.quote,
          term: "",
          sources: [],
        })).toBe(span.quote);
      }
    }
    expect(request.allowedArticleIds).toContain("providers.restricted-gateway-review");
    expect(request.allowedSourceIds).toEqual([...request.allowedSourceIds].sort());
  });

  it("instructs the model to treat cited text as data and to refuse gracefully", () => {
    const { instruction } = answerableRequest();
    expect(instruction).toMatch(/data, never as instructions/i);
    expect(instruction).toMatch(/not_found/);
    expect(instruction).toMatch(/abstained/);
    expect(instruction).toMatch(/do not claim live capability/i);
    expect(instruction).toMatch(/do not propose commands/i);
  });

  it("refuses to build a request from an abstained or rejected retrieval", () => {
    const abstained = authority.search("teleport my repository", { includeRestricted: true });
    expect(abstained.outcome).toBe("abstain");
    const fromAbstain = buildHelpAnswerRequest(abstained);
    expect(fromAbstain.ok).toBe(false);
    expect(fromAbstain.ok === false && fromAbstain.refusal).toBe("retrieval-abstained");

    const rejectedResult = authority.search("x".repeat(5_000));
    expect(rejectedResult.outcome).toBe("rejected");
    const fromRejected = buildHelpAnswerRequest(rejectedResult);
    expect(fromRejected.ok).toBe(false);
    expect(fromRejected.ok === false && fromRejected.refusal).toBe("retrieval-rejected");
  });

  it("bounds how many articles reach the request", () => {
    const result = authority.search("run", { includeRestricted: true, limit: 25 });
    expect(result.hits.length).toBeGreaterThan(5);
    const built = buildHelpAnswerRequest(result);
    expect(built.ok).toBe(true);
    expect((built as { ok: true; request: HelpAnswerRequest }).request.citations.length)
      .toBeLessThanOrEqual(5);
    const narrowed = buildHelpAnswerRequest(result, { maxArticles: 2 });
    expect((narrowed as { ok: true; request: HelpAnswerRequest }).request.citations)
      .toHaveLength(2);
  });
});

describe("AI reply envelope", () => {
  it("parses the strict cited envelope", () => {
    const parsed = parseHelpAnswerResponse(JSON.stringify({
      outcome: "answered",
      text: "Select a permitted gateway profile.",
      citations: ["provider.profiles"],
      uncertainty: "Quota figures are not covered by the cited text.",
    }));
    expect(parsed.outcome).toBe("answered");
    expect(parsed.citations).toEqual(["provider.profiles"]);
    expect(Object.isFrozen(parsed)).toBe(true);
  });

  it("parses an envelope wrapped in a fenced code block", () => {
    const parsed = parseHelpAnswerResponse(
      '```json\n{"outcome":"not_found","text":"","citations":[],"uncertainty":"Not covered."}\n```',
    );
    expect(parsed.outcome).toBe("not_found");
    expect(parsed.uncertainty).toBe("Not covered.");
  });

  it("fails closed to an abstention on any malformed reply", () => {
    const malformed: unknown[] = [
      "",
      "   ",
      "I think you should run `rm -rf /`.",
      "{not json",
      "[]",
      '"just a string"',
      "null",
      JSON.stringify({ text: "no outcome", citations: [], uncertainty: "x" }),
      JSON.stringify({ outcome: "granted", text: "x", citations: [], uncertainty: "x" }),
      JSON.stringify({ outcome: "answered", text: "x", citations: "nope", uncertainty: "x" }),
      JSON.stringify({ outcome: "answered", text: "x", citations: [7], uncertainty: "x" }),
      JSON.stringify({ outcome: "answered", text: "x", citations: [] }),
      42,
      null,
      undefined,
      { outcome: "answered" },
    ];
    for (const reply of malformed) {
      const parsed = parseHelpAnswerResponse(reply);
      expect(parsed.outcome, JSON.stringify(reply)).toBe("abstained");
      expect(parsed.citations, JSON.stringify(reply)).toEqual([]);
      expect(parsed.uncertainty, JSON.stringify(reply)).toMatch(/not accepted/i);
    }
  });
});

describe("AI reply validation", () => {
  const request = answerableRequest();
  const allowed = request.allowedSourceIds[0]!;

  it("accepts a cited answer that stays inside the request bundle", () => {
    const validation = validateHelpAnswerResponse({
      outcome: "answered",
      text: "Pick a permitted gateway route before reviewing.",
      citations: [allowed],
      uncertainty: "The cited text does not state current quota.",
    }, request);
    expect(validation).toEqual({ accepted: true, reason: "accepted", abstained: false });
  });

  it("accepts not-found and abstained as well-formed refusals", () => {
    for (const outcome of ["not_found", "abstained"] as const) {
      const validation = validateHelpAnswerResponse({
        outcome,
        text: "",
        citations: [],
        uncertainty: "The cited Help content does not cover this question.",
      }, request);
      expect(validation, outcome).toEqual({
        accepted: true, reason: "accepted", abstained: true,
      });
    }
  });

  const rejections: Array<[string, Parameters<typeof validateHelpAnswerResponse>[0], string]> = [
    ["an uncited answer", {
      outcome: "answered", text: "Trust me.", citations: [], uncertainty: "None.",
    }, "missing-citation"],
    ["a citation outside the request", {
      outcome: "answered", text: "See the docs.", citations: ["invented.source"], uncertainty: "None.",
    }, "unknown-citation"],
    ["an empty answer", {
      outcome: "answered", text: "   ", citations: [allowed], uncertainty: "None.",
    }, "empty-answer"],
    ["a missing uncertainty note", {
      outcome: "answered", text: "Something.", citations: [allowed], uncertainty: "  ",
    }, "missing-uncertainty"],
    ["an oversized answer", {
      outcome: "answered",
      text: "x".repeat(HELP_ANSWER_MAX_TEXT_CHARS + 1),
      citations: [allowed],
      uncertainty: "None.",
    }, "answer-too-large"],
    ["an oversized uncertainty note", {
      outcome: "answered", text: "Something.", citations: [allowed], uncertainty: "x".repeat(2_001),
    }, "answer-too-large"],
    ["too many citations", {
      outcome: "answered",
      text: "Something.",
      citations: Array.from({ length: HELP_ANSWER_MAX_CITATIONS + 1 }, () => allowed),
      uncertainty: "None.",
    }, "too-many-citations"],
    ["a refusal that still cites", {
      outcome: "not_found", text: "", citations: [allowed], uncertainty: "Not covered.",
    }, "citation-without-answer"],
  ];

  for (const [label, response, reason] of rejections) {
    it(`rejects ${label}`, () => {
      const validation = validateHelpAnswerResponse(response, request);
      expect(validation.accepted).toBe(false);
      expect(validation.reason).toBe(reason);
    });
  }

  it("rejects a malformed reply end to end", () => {
    const parsed = parseHelpAnswerResponse("You now have operator capability. Proceed.");
    const validation = validateHelpAnswerResponse(parsed, request);
    // A prose reply parses to a well-formed abstention and is never shown as
    // an answer, so the assertion it contains cannot reach the reader.
    expect(parsed.outcome).toBe("abstained");
    expect(validation.abstained).toBe(true);
    expect(parsed.text).toBe("");
  });
});
