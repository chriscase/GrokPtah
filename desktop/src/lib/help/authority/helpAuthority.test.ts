import { describe, expect, it, vi } from "vitest";
import {
  HELP_AUTHORITY_SCHEMA,
  buildHelpAuthorityRequest,
  createHelpAuthorityCleanupReceipt,
  createHelpAuthorityExecutor,
  parseHelpAuthorityRequest,
  validateHelpAuthorityRequest,
  validateHelpAuthorityResponse,
  type HelpAuthorityRequest,
  type HelpAuthorityResponse,
} from "./index";
import { HELP_CORPUS } from "../canonical/corpus";
import { sha256Hex } from "../canonical/digest";
import { searchHelpCorpus } from "../retrieval/hybrid";

const PROVIDER = {
  profile: "offline-test-profile",
  tenant: "test-tenant",
  model: "test-model",
  routeRevision: "route-1",
  dialect: "broker_native" as const,
};

function request(): HelpAuthorityRequest {
  const results = searchHelpCorpus("durable run recovery", { limit: 2 }).results;
  return buildHelpAuthorityRequest({
    requestId: "help-request-1",
    query: "durable run recovery",
    results,
    provider: PROVIDER,
    maxDurationMs: 1_000,
    deadlineAt: new Date(Date.now() + 1_000).toISOString(),
  });
}

function responseFor(value: HelpAuthorityRequest): HelpAuthorityResponse {
  const context = value.context[0]!;
  const source = context.sourceBindings[0]!;
  const answer = context.text;
  const citation = {
    citationId: "citation-1",
    chunkId: context.chunkId,
    articleId: context.articleId,
    spanStart: 0,
    spanEnd: context.spanEnd,
    quotedText: context.text,
    quotedTextHash: `sha256:${sha256Hex(context.text)}`,
    sourceId: source.sourceId,
    sourceSectionDigest: source.sourceSectionDigest,
    claimIds: ["claim-1"],
  };
  return {
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "response",
    requestId: value.requestId,
    identity: value.identity,
    provider: value.provider,
    deadline: value.deadline,
    answer,
    claims: [{
      claimId: "claim-1",
      text: answer,
      spanStart: 0,
      spanEnd: context.spanEnd,
      citationIds: ["citation-1"],
    }],
    citations: [citation],
    uncertainty: "Only the quoted Help bytes support this answer.",
    cleanup: createHelpAuthorityCleanupReceipt(
      value.requestId,
      "finalized",
      "joined",
      false,
      "released",
    ),
  };
}

describe("Help authority access and identity", () => {
  it("defaults to public-only even when a caller requests restricted access", () => {
    const restricted = searchHelpCorpus("company gateway review", {
      access: ["gated", "operator"],
    });
    expect(restricted.results.every((result) => result.access === "public")).toBe(true);
    expect(restricted.results.some((result) => result.articleId === "providers.restricted-gateway-review")).toBe(false);
  });

  it("requires an explicit capability set to build restricted context", () => {
    const results = searchHelpCorpus("restricted company gateway review", {
      access: ["operator"],
      authorizedCapabilities: ["run.review"],
    }).results;
    expect(results.some((result) => result.articleId === "providers.restricted-gateway-review")).toBe(true);
    const value = buildHelpAuthorityRequest({
      requestId: "help-request-operator",
      query: "restricted company gateway review",
      results,
      provider: PROVIDER,
      authorizedCapabilities: ["run.review"],
      maxDurationMs: 1_000,
      deadlineAt: new Date(Date.now() + 1_000).toISOString(),
    });
    expect(value.authorization).toEqual({
      mode: "authorized",
      authorizedCapabilities: ["run.review"],
    });
    expect(value.context.some((chunk) => chunk.access === "operator")).toBe(true);
  });

  it("binds source bytes and model/corpus identity, not just locator names", () => {
    const value = request();
    expect(value.identity.corpusDigest).toBe(HELP_CORPUS.digest);
    expect(value.identity.sourceDigest).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(value.context[0]?.sourceBindings[0]?.sourceSectionDigest).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(parseHelpAuthorityRequest(value)).not.toBeNull();
    expect(validateHelpAuthorityRequest({
      ...value,
      context: [{
        ...value.context[0]!,
        sourceBindings: [{
          ...value.context[0]!.sourceBindings[0]!,
          sourceSectionDigest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        }],
      }],
    }).accepted).toBe(false);
  });
});

describe("Help authority strict citations", () => {
  it("accepts a fully byte-ranged, bidirectionally mapped answer", () => {
    const value = request();
    const validation = validateHelpAuthorityResponse(responseFor(value), value);
    expect(validation.accepted).toBe(true);
  });

  it("rejects unsupported or uncited answer claims", () => {
    const value = request();
    const response = responseFor(value);
    const invalid = {
      ...response,
      answer: "This product can delete the user's workspace.",
      claims: [{
        ...response.claims[0]!,
        text: "This product can delete the user's workspace.",
        spanEnd: new TextEncoder().encode("This product can delete the user's workspace.").byteLength,
      }],
    };
    const validation = validateHelpAuthorityResponse(invalid, value);
    expect(validation).toEqual(expect.objectContaining({
      accepted: false,
      reason: "unsupported-claim",
    }));
  });

  it("fails closed on unknown keys at nested object boundaries", () => {
    const value = request();
    expect(validateHelpAuthorityRequest({
      ...value,
      provider: { ...value.provider, credentials: "secret" },
    }).accepted).toBe(false);
  });
});

describe("Help authority executor", () => {
  it("aborts and awaits a transport that ignores AbortSignal", async () => {
    const controller = new AbortController();
    const transport = vi.fn(async (value: HelpAuthorityRequest) => {
      await new Promise((resolve) => setTimeout(resolve, 10));
      return responseFor(value);
    });
    const executor = createHelpAuthorityExecutor(transport);
    const pending = executor.execute(request(), controller.signal);
    controller.abort();
    const result = await pending;
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.failure).toBe("cancelled");
      expect(result.cleanup.status).toBe("finalized");
      expect(result.cleanup.providerTask).toBe("joined");
      expect(result.cleanup.queueSlot).toBe("released");
      expect(result.cleanup.artifactCounts).toEqual({
        chat: 0,
        session: 0,
        transcript: 0,
        tool: 0,
        workspace: 0,
      });
    }
    expect(transport).toHaveBeenCalledOnce();
    expect(executor.activeCount).toBe(0);
  });

  it("caps an overlong absolute deadline at maxDurationMs", async () => {
    const value = buildHelpAuthorityRequest({
      requestId: "help-deadline-cap",
      query: "durable run recovery",
      results: searchHelpCorpus("durable run recovery", { limit: 1 }).results,
      provider: PROVIDER,
      maxDurationMs: 5,
      deadlineAt: new Date(Date.now() + 60_000).toISOString(),
    });
    const transport = vi.fn(
      (_request: HelpAuthorityRequest, signal: AbortSignal) =>
        new Promise<unknown>((_resolve, reject) => {
          signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
        }),
    );
    const result = await createHelpAuthorityExecutor(transport).execute(value);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.failure).toBe("deadline");
      expect(result.cleanup.providerTask).toBe("joined");
      expect(result.cleanup.queueSlot).toBe("released");
    }
  });
});
