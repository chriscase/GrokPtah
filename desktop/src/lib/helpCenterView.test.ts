import { describe, expect, it } from "vitest";
import {
  buildHelpAnswerRequest,
  parseHelpAnswerResponse,
  validateHelpAnswerResponse,
  HELP_ANSWER_DEFAULT_TIMEOUT_MS,
} from "./helpAnswer";
import { createHelpAuthority, type HelpCitationSpan } from "./helpAuthority";
import {
  HELP_CENTER_VIEW_CONTRACT,
  HELP_LIVE_AVAILABILITY_NOTE,
  describeHelpAskTimeout,
  describeHelpAskUnknowns,
  helpAccessLabel,
  helpBrowseArticles,
  helpCapabilityLabel,
  helpViewLabels,
  helpViewState,
  helpViewStatus,
  summarizeHelpAnswer,
  verifyHelpSpans,
} from "./helpCenterView";
import {
  HELP_VIEW_FIXTURES,
  HELP_VIEW_FIXTURE_ARTICLES,
  HELP_VIEW_REPLY_FIXTURES,
  createHelpViewFixtureAuthority,
} from "./helpCenterView.fixtures";

const authority = createHelpViewFixtureAuthority();

describe("Help view fixtures", () => {
  it("builds a fixture corpus that survives the real fail-closed checks", () => {
    // The fixture corpus goes through the same validation and digest path as
    // the shipped one, so a fixture that could not ship cannot pass here.
    expect(authority.verify().ok).toBe(true);
    expect(authority.articles).toHaveLength(HELP_VIEW_FIXTURE_ARTICLES.length);
  });

  it.each(HELP_VIEW_FIXTURES.map((fixture) => [fixture.name, fixture] as const))(
    "%s",
    (_name, fixture) => {
      const result = authority.search(fixture.query, fixture.request);
      const state = helpViewState(result, authority);

      expect(state.status).toBe(fixture.expectedStatus);
      expect(state.answer?.articleId ?? null).toBe(fixture.expectedAnswerId);
      expect(state.candidates.map((candidate) => candidate.articleId)).toEqual(
        [...fixture.expectedCandidateIds],
      );
    },
  );

  it("never populates an answer outside the answer status", () => {
    for (const fixture of HELP_VIEW_FIXTURES) {
      const state = helpViewState(authority.search(fixture.query, fixture.request), authority);
      if (state.status === "answer") {
        expect(state.answer).not.toBeNull();
        expect(state.canAskModel).toBe(true);
      } else {
        expect(state.answer).toBeNull();
        expect(state.canAskModel).toBe(false);
      }
    }
  });

  it("carries the retriever's own verdict alongside the derived status", () => {
    const ambiguous = authority.search("beacon");
    const state = helpViewState(ambiguous, authority);

    expect(state.contract).toBe(HELP_CENTER_VIEW_CONTRACT);
    expect(state.status).toBe("ambiguous");
    // The consumer's wording never replaces what the authority actually said.
    expect(state.outcome).toBe("abstain");
    expect(state.abstainReason).toBe("ambiguous");
    expect(state.digest).toBe(ambiguous.digest);
    expect(state.retrievalMode).toBe("offline-hybrid");
  });

  it("keeps a rejection distinguishable from an abstention", () => {
    const rejected = helpViewState(authority.search("x".repeat(9_000)), authority);
    const abstained = helpViewState(authority.search("zzzz qqqq"), authority);

    expect(rejected.status).toBe("rejected");
    expect(rejected.rejection).toBe("query-too-long");
    expect(rejected.abstainReason).toBeNull();
    expect(abstained.status).toBe("no-match");
    expect(abstained.rejection).toBeNull();
    expect(rejected.headline).not.toBe(abstained.headline);
  });

  it("treats an unrecognised abstain reason as the weakest state, never an answer", () => {
    const base = authority.search("beacon");
    const unknown = { ...base, abstainReason: "reason-from-a-newer-contract" as never };

    expect(helpViewStatus(unknown)).toBe("low-confidence");
    expect(helpViewState(unknown, authority).answer).toBeNull();
  });
});

describe("citation verification", () => {
  it("keeps spans the corpus reproduces", () => {
    const hit = authority.search("lantern workspace").hits[0];
    const { spans, unverified } = verifyHelpSpans(hit.citation.spans, authority);

    expect(spans.length).toBeGreaterThan(0);
    expect(unverified).toBe(0);
    for (const span of spans) {
      expect(span.verified).toBe(true);
      expect(authority.resolveSpan(span)).toBe(span.quote);
      expect(span.sources.length).toBeGreaterThan(0);
    }
  });

  it("drops a span whose quote the corpus does not confirm, and counts it", () => {
    const hit = authority.search("lantern workspace").hits[0];
    const forged: HelpCitationSpan = {
      ...hit.citation.spans[0],
      quote: "a quote the corpus never contained",
    };
    const { spans, unverified } = verifyHelpSpans([forged, ...hit.citation.spans], authority);

    expect(unverified).toBe(1);
    expect(spans.every((span) => span.quote !== forged.quote)).toBe(true);
  });

  it("reports the drop through the rendered answer so the gap is disclosable", () => {
    const result = authority.search("lantern workspace");
    const hit = result.hits[0];
    const tampered = {
      ...result,
      hits: [
        {
          ...hit,
          citation: {
            ...hit.citation,
            spans: [{ ...hit.citation.spans[0], quote: "not in the corpus" }],
          },
        },
      ],
    };
    const state = helpViewState(tampered, authority);

    expect(state.answer?.spans).toHaveLength(0);
    expect(state.answer?.unverifiedSpanCount).toBe(1);
  });
});

describe("capability and permission labels", () => {
  it("names the documented capability vocabulary", () => {
    expect(helpCapabilityLabel("run.promote")).toBe("Promote a run");
    expect(helpCapabilityLabel("computer.control")).toBe("Control the computer");
  });

  it("formats a capability outside the shipped vocabulary instead of guessing", () => {
    expect(helpCapabilityLabel("lantern.relay_rotation")).toBe("Lantern · Relay rotation");
  });

  it("never claims live availability for a documented capability", () => {
    const article = HELP_VIEW_FIXTURE_ARTICLES.find((entry) => entry.id === "synthetic.sealed-vault");
    const labels = helpViewLabels(article!);

    expect(labels.capabilities.map((capability) => capability.id))
      .toEqual(["run.promote", "run.review"]);
    for (const capability of labels.capabilities) {
      expect(capability.documented).toBe(true);
      expect(capability.liveAvailability).toBe("unknown");
    }
    expect(labels.liveAvailabilityNote).toBe(HELP_LIVE_AVAILABILITY_NOTE);
  });

  it("labels each access level and says what it does not grant", () => {
    expect(helpAccessLabel("public").label).toBe("Open to everyone");
    expect(helpAccessLabel("gated").detail).toMatch(/does not grant it/);
    expect(helpAccessLabel("operator").detail).toMatch(/does not confer the role/);
  });

  it("reads an unrecognised access level as restricted, not as open", () => {
    const label = helpAccessLabel("something-new" as never);

    expect(label.label).toBe("Restricted");
    expect(label.label).not.toBe("Open to everyone");
  });
});

describe("browse listing", () => {
  it("agrees with the authority about what each filter hides", () => {
    // Browsing and searching must show the same set, or a reader could see an
    // article listed that they can never retrieve (or the reverse).
    const cases = [
      { includeRestricted: false },
      { includeRestricted: true },
      { includeRestricted: true, topic: "operations" as const },
      { includeRestricted: true, audience: "everyone" as const },
    ];
    for (const request of cases) {
      const browsed = new Set(
        helpBrowseArticles(authority.articles, request).map((entry) => entry.articleId),
      );
      for (const article of authority.articles) {
        // Query an article by its own title: if retrieval can reach it under
        // these filters, browsing must list it.
        const retrievable = authority
          .search(article.title, { ...request, limit: 25 })
          .hits.some((hit) => hit.article.id === article.id);
        expect(browsed.has(article.id)).toBe(retrievable);
      }
    }
  });

  it("orders by topic and then by article ID, not by corpus storage order", () => {
    const listed = helpBrowseArticles(authority.articles, { includeRestricted: true });

    expect(listed.map((entry) => entry.topic)).toEqual([
      "getting-started", "providers", "computer-use", "operations", "operations",
    ]);
    expect(listed.map((entry) => entry.articleId)).toEqual([
      "synthetic.lantern-workspace",
      "synthetic.sealed-vault",
      "synthetic.shared-screen",
      "synthetic.northern-relay",
      "synthetic.southern-relay",
    ]);
  });

  it("reports no ranking for a listing, because there is no query", () => {
    for (const entry of helpBrowseArticles(authority.articles)) {
      expect(entry.confidence).toBe(0);
      expect(entry.matchedTerms).toHaveLength(0);
    }
  });

  it("hides a restricted article until the caller declares it wants one", () => {
    const open = helpBrowseArticles(authority.articles).map((entry) => entry.articleId);
    const all = helpBrowseArticles(authority.articles, { includeRestricted: true })
      .map((entry) => entry.articleId);

    expect(open).not.toContain("synthetic.sealed-vault");
    expect(all).toContain("synthetic.sealed-vault");
  });
});

describe("optional model seam presentation", () => {
  const answerable = authority.search("lantern workspace");
  const request = (() => {
    const built = buildHelpAnswerRequest(answerable);
    if (!built.ok) throw new Error(`fixture retrieval was not answerable: ${built.refusal}`);
    return built.request;
  })();

  it("presents an accepted, cited reply as a draft and keeps the corpus authoritative", () => {
    const sourceId = request.allowedSourceIds[0];
    const response = parseHelpAnswerResponse(HELP_VIEW_REPLY_FIXTURES.citedAnswer(sourceId));
    const summary = summarizeHelpAnswer(response, validateHelpAnswerResponse(response, request));

    expect(summary.status).toBe("answered");
    expect(summary.corpusRemainsAuthority).toBe(true);
  });

  it("never presents an uncited assertion as an answer", () => {
    const response = parseHelpAnswerResponse(HELP_VIEW_REPLY_FIXTURES.uncitedAssertion);
    const summary = summarizeHelpAnswer(response, validateHelpAnswerResponse(response, request));

    expect(summary.status).toBe("rejected");
    expect(summary.detail).toMatch(/without citing anything/);
  });

  it("never presents a citation from outside the request bundle", () => {
    const response = parseHelpAnswerResponse(HELP_VIEW_REPLY_FIXTURES.foreignCitation);
    const summary = summarizeHelpAnswer(response, validateHelpAnswerResponse(response, request));

    expect(summary.status).toBe("rejected");
    expect(summary.detail).toMatch(/not in the request/);
  });

  it("turns prose that is not the envelope into a visible abstention", () => {
    const response = parseHelpAnswerResponse(HELP_VIEW_REPLY_FIXTURES.prose);
    const summary = summarizeHelpAnswer(response, validateHelpAnswerResponse(response, request));

    expect(response.outcome).toBe("abstained");
    expect(summary.status).toBe("declined");
    expect(summary.corpusRemainsAuthority).toBe(true);
  });

  it("shows a well-formed refusal as a refusal, not as a failure", () => {
    const response = parseHelpAnswerResponse(HELP_VIEW_REPLY_FIXTURES.notFound);
    const summary = summarizeHelpAnswer(response, validateHelpAnswerResponse(response, request));

    expect(summary.status).toBe("declined");
    expect(summary.headline).toMatch(/found no answer/);
  });

  it("refuses to build a request from an abstained retrieval", () => {
    const built = buildHelpAnswerRequest(authority.search("beacon"));

    expect(built.ok).toBe(false);
    expect(built.ok === false && built.refusal).toBe("retrieval-abstained");
  });

  it("keeps provider, model, cost, and latency unknown", () => {
    const unknowns = describeHelpAskUnknowns(request);

    expect(unknowns.provider).toBe("unknown");
    expect(unknowns.model).toBe("unknown");
    expect(unknowns.cost).toBe("unknown");
    expect(unknowns.latency).toBe("unknown");
  });

  it("marks a caller-supplied provider label as a route, not an identity", () => {
    const unknowns = describeHelpAskUnknowns(request, "Company gateway");

    expect(unknowns.provider).toMatch(/Company gateway/);
    expect(unknowns.provider).toMatch(/identity unverified/);
    // The model is not inferable from the route, so it stays unknown.
    expect(unknowns.model).toBe("unknown");
  });

  it("reports a timeout against the declared budget and asserts no elapsed time", () => {
    const summary = describeHelpAskTimeout(request);

    expect(request.timeoutMs).toBe(HELP_ANSWER_DEFAULT_TIMEOUT_MS);
    expect(summary.status).toBe("timeout");
    expect(summary.detail).toMatch(/20s budget/);
    expect(summary.detail).toMatch(/unknown/);
    expect(summary.corpusRemainsAuthority).toBe(true);
  });
});

describe("shipped corpus", () => {
  it("serves the real corpus through the same consumer contract", () => {
    // The fixtures deliberately avoid the shipped corpus; this one case proves
    // the contract is not fixture-shaped.
    const shipped = createHelpAuthority();
    const state = helpViewState(
      shipped.search("restricted company gateway", { includeRestricted: true }),
      shipped,
    );

    expect(state.status).toBe("answer");
    expect(state.answer?.articleId).toBe("providers.restricted-gateway-review");
    expect(state.answer?.spans.length).toBeGreaterThan(0);
    expect(state.answer?.unverifiedSpanCount).toBe(0);
    expect(state.answer?.labels.access.value).toBe("operator");
  });

  it("abstains, and offers nothing as an answer, on an undocumented question", () => {
    const shipped = createHelpAuthority();
    const state = helpViewState(
      shipped.search("teleport my repository", { includeRestricted: true }),
      shipped,
    );

    expect(state.status).toBe("low-confidence");
    expect(state.answer).toBeNull();
    expect(state.canAskModel).toBe(false);
  });
});
