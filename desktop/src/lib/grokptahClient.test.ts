import { describe, expect, it, vi } from "vitest";
import { CAPABILITY_CONTRACT } from "./capabilities";
import { GrokPtahClient } from "./grokptahClient";

function response(body: unknown, status = 200, headers?: Record<string, string>): Response {
  return new Response(body === null ? null : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

describe("GrokPtahClient", () => {
  it("negotiates capabilities and keeps the transport session", async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        response(
          {
            jsonrpc: "2.0",
            id: 1,
            result: {
              protocolVersion: "2025-03-26",
              serverInfo: {
                capabilityContract: {
                  contract: CAPABILITY_CONTRACT,
                  capabilities: [
                    {
                      id: "run.execute",
                      tier: "execute",
                      mutating: true,
                      human_gate: false,
                      availability: "available",
                      description: "Submit runs",
                    },
                  ],
                },
              },
            },
          },
          200,
          { "mcp-session-id": "transport-1" },
        ),
      )
      .mockResolvedValueOnce(response(null, 202))
      .mockResolvedValueOnce(
        response({
          jsonrpc: "2.0",
          id: 2,
          result: { tools: [{ name: "ptah_get_capacity", inputSchema: { type: "object" } }] },
        }),
      );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp/",
      token: "secret",
      fetcher,
    });

    await client.initialize();
    expect(client.isInitialized).toBe(true);
    expect(client.transportSessionId).toBe("transport-1");
    expect(client.capabilities?.capabilities[0].id).toBe("run.execute");
    await expect(client.listTools()).resolves.toHaveLength(1);
    expect(fetcher.mock.calls[1][1]?.headers).toMatchObject({
      "mcp-session-id": "transport-1",
    });
  });

  it("fails closed before initialization and rejects malformed responses", async () => {
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher: vi.fn<typeof fetch>(),
    });
    await expect(client.listTools()).rejects.toThrow("not initialized");

    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValue(response({ jsonrpc: "2.0", id: 1, result: {} }));
    const malformed = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    await expect(malformed.initialize()).rejects.toThrow("protocolVersion");
  });

  it("rejects a valid protocol handshake without a valid capability contract", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({
        jsonrpc: "2.0",
        id: 1,
        result: { protocolVersion: "2025-03-26", serverInfo: {} },
      }),
    );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    await expect(client.initialize()).rejects.toThrow("valid capability contract");
    expect(client.isInitialized).toBe(false);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("keeps remote errors share-safe and bounded", async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        response(
          {
            jsonrpc: "2.0",
            id: 1,
            result: {
              protocolVersion: "2025-03-26",
              serverInfo: {
                capabilityContract: {
                  contract: CAPABILITY_CONTRACT,
                  capabilities: [],
                },
              },
            },
          },
          200,
          { "mcp-session-id": "transport-1" },
        ),
      )
      .mockResolvedValueOnce(response(null, 202))
      .mockResolvedValueOnce(
        response({
          jsonrpc: "2.0",
          id: 2,
          error: {
            code: -32000,
            message: "safe message",
            data: {
              code: "forbidden_scope",
              requestId: "req-1",
              privilegedPath: "/Users/private",
            },
          },
        }),
      );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    await client.initialize();
    await expect(client.callTool("ptah_get_run", {})).rejects.toMatchObject({
      name: "GrokPtahRemoteError",
      code: "forbidden_scope",
      message: "safe message",
      requestId: "req-1",
    });
  });

  it("parses typed safe errors returned over HTTP 4xx", async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        response(
          {
            jsonrpc: "2.0",
            id: 1,
            result: {
              protocolVersion: "2025-03-26",
              serverInfo: {
                capabilityContract: {
                  contract: CAPABILITY_CONTRACT,
                  capabilities: [],
                },
              },
            },
          },
          200,
          { "mcp-session-id": "transport-1" },
        ),
      )
      .mockResolvedValueOnce(response(null, 202))
      .mockResolvedValueOnce(
        response(
          {
            jsonrpc: "2.0",
            id: 2,
            error: {
              code: -32000,
              message: "safe stale response",
              data: {
                code: "stale_or_recovery",
                reasonCode: "cursor_expired",
                requestId: "req-2",
                secret: "drop",
              },
            },
          },
          409,
        ),
      );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    await client.initialize();
    await expect(client.callTool("ptah_get_run", {})).rejects.toMatchObject({
      name: "GrokPtahRemoteError",
      code: "stale_or_recovery",
      reasonCode: "cursor_expired",
      requestId: "req-2",
    });
  });

  it("rejects mismatched RPC response ids", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({
        jsonrpc: "2.0",
        id: 99,
        result: { protocolVersion: "2025-03-26" },
      }),
    );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    await expect(client.initialize()).rejects.toThrow("correlation");
    expect(client.transportSessionId).toBeNull();
  });

  it("replays scoped SSE events and stops at an explicit recovery frame", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        const text =
          'id: 4\nevent: message\ndata: {"jsonrpc":"2.0","method":"notifications/ptah_event","params":{"sessionId":"s","workspace":"/repo","runId":"r","seq":4,"ts":"now","update":{"type":"turn_complete"}}}\n\n' +
          'event: message\ndata: {"jsonrpc":"2.0","method":"notifications/ptah_recovery","params":{"sessionId":"s","workspace":"/repo","runId":"r","afterSeq":4,"reason":"gap","pollTool":"ptah_get_events"}}\n\n';
        controller.enqueue(new TextEncoder().encode(text));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher,
    });
    // The test only needs the initialized transport invariant.
    Object.defineProperty(client, "sessionId", { value: "transport-1", writable: true });
    Object.defineProperty(client, "protocolVersion", {
      value: "2025-03-26",
      writable: true,
    });

    const notifications = [];
    for await (const notification of client.streamRunEvents({
      sessionId: "s",
      workspace: "/repo",
      runId: "r",
    })) {
      notifications.push(notification);
    }
    expect(notifications.map((notification) => notification.kind)).toEqual(["event", "recovery"]);
    expect(fetcher.mock.calls[0][0].toString()).toContain("session_id=s");
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("Last-Event-ID");
  });
});
