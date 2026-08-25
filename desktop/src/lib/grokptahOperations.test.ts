import { describe, expect, it, vi } from "vitest";
import { CAPABILITY_CONTRACT } from "./capabilities";
import { GrokPtahClient } from "./grokptahClient";
import {
  GrokPtahCapabilityError,
  GrokPtahOperations,
  validateGrokPtahBounds,
} from "./grokptahOperations";

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function clientWithCapabilities(fetcher: typeof fetch, capabilities: unknown[]) {
  const client = new GrokPtahClient({
    baseUrl: "http://127.0.0.1:39200/mcp",
    token: "secret",
    fetcher,
  });
  Object.defineProperty(client, "sessionId", { value: "transport-1", writable: true });
  Object.defineProperty(client, "protocolVersion", { value: "2025-03-26", writable: true });
  Object.defineProperty(client, "capabilitySet", {
    value: { contract: CAPABILITY_CONTRACT, capabilities },
    writable: true,
  });
  return client;
}

const execute = {
  id: "run.execute",
  tier: "execute",
  mutating: true,
  human_gate: false,
  availability: "available",
  description: "Submit runs",
};

const promote = {
  id: "run.promote",
  tier: "promote",
  mutating: true,
  human_gate: true,
  availability: "gated",
  description: "Promote isolated runs",
};

describe("GrokPtahOperations", () => {
  it("rejects malformed execution bounds before transport", () => {
    expect(() => validateGrokPtahBounds({ maxRounds: 0 })).toThrow(
      "maxRounds must be a positive safe integer",
    );
    expect(() => validateGrokPtahBounds({ maxRounds: 25 })).toThrow(
      "maxRounds must be at most 24",
    );
    expect(() => validateGrokPtahBounds({ maxPromptBytes: 1.5 })).toThrow(
      "maxPromptBytes must be a positive safe integer",
    );
    expect(() => validateGrokPtahBounds({ maxDurationMs: -1 })).toThrow(
      "maxDurationMs must be a positive safe integer",
    );
  });

  it("builds a scope-fenced submit payload", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({
        jsonrpc: "2.0",
        id: 1,
        result: { runId: "run-1" },
      }),
    );
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));

    const result = await operations.submitTask(
      { sessionId: "session-1", workspace: "/repo" },
      "request-1",
      "review the change",
      { executionMode: "isolated_worktree", allowQueue: true },
    );

    expect(result.value).toEqual({ runId: "run-1" });
    const body = JSON.parse(String(fetcher.mock.calls[0][1]?.body));
    expect(body.params.arguments).toEqual({
      request_id: "request-1",
      session_id: "session-1",
      workspace: "/repo",
      prompt: "review the change",
      execution_mode: "isolated_worktree",
      allow_queue: true,
    });
  });

  it("validates bounds before sending a submit request", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));
    await expect(
      operations.submitTask(
        { sessionId: "session-1", workspace: "/repo" },
        "request-1",
        "review",
        { bounds: { maxRounds: 0 } },
      ),
    ).rejects.toThrow("maxRounds must be a positive safe integer");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("requires an explicit gate before promotion", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { promoted: true } }),
    );
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [promote]));
    const scope = { sessionId: "session-1", workspace: "/repo", runId: "run-1" };

    await expect(operations.promoteRun(scope, "request-1", "approval-1")).rejects.toEqual(
      expect.objectContaining<GrokPtahCapabilityError>({
        capabilityId: "run.promote",
        state: "requires_gate",
      }),
    );
    await expect(operations.promoteRun(scope, "request-1", "approval-1", true)).resolves.toEqual(
      expect.objectContaining({ value: { promoted: true } }),
    );
  });

  it("rejects empty identity fields before transport", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));

    await expect(
      operations.getRun({ sessionId: "", workspace: "/repo", runId: "run-1" }),
    ).rejects.toThrow("sessionId must not be empty");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed when the negotiated set omits a requested capability", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, []));
    await expect(
      operations.submitTask(
        { sessionId: "session-1", workspace: "/repo" },
        "request-1",
        "review",
      ),
    ).rejects.toMatchObject({ capabilityId: "run.execute", state: "unavailable" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("gates read operations on their negotiated capability", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, []));
    await expect(operations.listSessions()).rejects.toMatchObject({
      capabilityId: "session.observe",
    });
    await expect(
      operations.getRun({ sessionId: "session-1", workspace: "/repo", runId: "run-1" }),
    ).rejects.toMatchObject({ capabilityId: "run.review" });
    await expect(
      operations.getPersistentAgent({ sessionId: "session-1", workspace: "/repo" }, "agent-1"),
    ).rejects.toMatchObject({ capabilityId: "agent.continuity" });
    await expect(
      operations.listPersistentAgents({ sessionId: "session-1", workspace: "/repo" }),
    ).rejects.toMatchObject({ capabilityId: "agent.continuity" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("uses the empty-args wire shape for allowlist-wide persistent-agent listing", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { agents: [] } }),
    );
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [
        {
          id: "agent.continuity",
          tier: "observe",
          mutating: false,
          human_gate: false,
          availability: "available",
          description: "List durable agents",
        },
      ]),
    );

    await operations.listPersistentAgents({ sessionId: "session-1", workspace: "/repo" });
    const body = JSON.parse(String(fetcher.mock.calls[0][1]?.body)) as {
      params?: { arguments?: unknown };
    };
    expect(body.params?.arguments).toEqual({});
  });

  it("exposes queue CAS operations through the same capability fence", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { ok: true } }),
    );
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [
        {
          id: "run.queue",
          tier: "execute",
          mutating: true,
          human_gate: false,
          availability: "available",
          description: "Queue",
        },
      ]),
    );
    await operations.reorderQueue(
      { sessionId: "session-1", workspace: "/repo" },
      "request-1",
      "entry-1",
      0,
      3,
      9,
    );
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body)).params.arguments).toMatchObject({
      entry_id: "entry-1",
      to_index: 0,
      expected_version: 3,
      expected_revision: 9,
    });
  });

  it("launches an external worker through external.execute without computer or promote authority", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { localRunId: "local-1" } }),
    );
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [
        {
          id: "external.execute",
          tier: "execute",
          mutating: true,
          human_gate: false,
          availability: "available",
          description: "Launch isolated external workers",
        },
      ]),
    );
    const result = await operations.launchExternalWorker(
      { sessionId: "session-1", workspace: "/repo" },
      {
        requestId: "ext-1",
        provider: "cursor_cloud",
        repository: "org/repo",
        startingRef: "refs/heads/review",
        prompt: "Review the exact candidate",
      },
    );
    expect(result.value).toEqual({ localRunId: "local-1" });
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body)).params.arguments).toEqual({
      request_id: "ext-1",
      session_id: "session-1",
      workspace: "/repo",
      provider: "cursor_cloud",
      repository: "org/repo",
      starting_ref: "refs/heads/review",
      prompt: "Review the exact candidate",
      execution_mode: "isolated",
      auto_create_pr: false,
    });
    await expect(
      operations.promoteRun(
        { sessionId: "session-1", workspace: "/repo", runId: "run-1" },
        "request-1",
        "approval-1",
      ),
    ).rejects.toMatchObject({ capabilityId: "run.promote", state: "unavailable" });
    await expect(
      operations.authorizeComputerRun(
        { sessionId: "session-1", workspace: "/repo", runId: "run-1" },
        "request-1",
        1,
        ["semantic"],
        1000,
      ),
    ).rejects.toMatchObject({ capabilityId: "computer.control", state: "unavailable" });
  });
});
