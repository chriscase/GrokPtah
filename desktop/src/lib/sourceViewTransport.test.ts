import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const { tauriSourceViewTransport } = await import("./api");
const { GrokPtahBrokerClient, createBrokerSourceViewTransport } = await import(
  "./grokptahBrokerClient"
);
const {
  SOURCE_VIEW_OPERATIONS,
  SourceViewRequestError,
  assertTransportComplete,
  parseRevokeResponse,
  sourceReadPayload,
  validateSnapshotId,
  validateSourceReadRequest,
  validateSourceToken,
} = await import("./sourceViewTransport");
const { SOURCE_VIEW_CONTRACT, SOURCE_VIEW_REPLAY_POLICY } = await import("./sourceView");

const SNAP = "0123456789abcdef0123456789abcdef";
const DIGEST_A = `${"0".repeat(63)}1`;
const DIGEST_B = `${"a".repeat(63)}b`;
const TOKEN = `sv1.${SNAP}.0.00112233445566778899aabbccddeeff`;

const ROOT = {
  token: TOKEN,
  kind: "workspace" as const,
  label: "repo/project",
  pathDigest: DIGEST_A,
  identityDigest: DIGEST_B,
  runId: null,
};

const SNAPSHOT = {
  snapshotId: SNAP,
  revision: 1,
  issuedAtMs: 1_700_000_000_000,
  expiresAtMs: 1_700_000_900_000,
  principalFingerprint: DIGEST_A,
  policyFingerprint: DIGEST_B,
  replayPolicy: SOURCE_VIEW_REPLAY_POLICY,
  roots: [ROOT],
};

const DOCUMENT = {
  contract: SOURCE_VIEW_CONTRACT,
  root: ROOT,
  snapshotId: SNAP,
  revision: 1,
  relativePath: "src/main.rs",
  language: "rust",
  byteLen: 13,
  content: { verdict: "text", scannedBytes: 13, completeScan: true },
  identity: { kind: "content", digest: DIGEST_A },
  limits: { maxBytes: 524_288, maxLines: 1_200, maxLineChars: 2_000 },
  chunk: {
    lines: [{ number: 1, text: "fn main() {}", truncated: false }],
    startByte: 0,
    bytesConsumed: 13,
    lossyReplacements: 0,
    eol: "lf",
    continuesPrevious: false,
    continuesNext: false,
    nextCursor: null,
    eof: true,
  },
};

const CURSOR = {
  byteOffset: 4,
  nextLineNumber: 1,
  carryHex: "f0",
  continuesLine: true,
  documentDigest: DIGEST_A,
};

/** A broker transport backed by a scripted fetcher. */
function brokerTransport(respond: (path: string, init?: RequestInit) => unknown) {
  const calls: Array<{ path: string; init?: RequestInit }> = [];
  const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    calls.push({ path, init });
    return new Response(JSON.stringify(respond(path, init)), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;
  const client = new GrokPtahBrokerClient({
    baseUrl: "https://desk.example",
    fetcher,
    csrfToken: "csrf-token",
  });
  let counter = 0;
  return {
    calls,
    transport: createBrokerSourceViewTransport(client, "binding-1", () => `idem-${(counter += 1)}`),
  };
}

beforeEach(() => invoke.mockReset());
afterEach(() => vi.clearAllMocks());

describe("request validation", () => {
  it("accepts a well-formed read and normalises its optional fields", () => {
    const validated = validateSourceReadRequest({ token: TOKEN, path: "src/main.rs" });
    expect(validated).toEqual({
      token: TOKEN,
      path: "src/main.rs",
      sessionId: null,
      startByte: 0,
      cursor: null,
      maxBytes: undefined,
      maxLines: undefined,
      maxLineChars: undefined,
    });
  });

  it("refuses a request that names both a start byte and a cursor", () => {
    expect(() =>
      validateSourceReadRequest({ token: TOKEN, path: "a.rs", startByte: 10, cursor: CURSOR }),
    ).toThrow(/never both/);
  });

  it("allows a cursor alongside an explicit zero start byte", () => {
    const validated = validateSourceReadRequest({
      token: TOKEN,
      path: "a.rs",
      startByte: 0,
      cursor: CURSOR,
    });
    expect(validated.cursor).toEqual(CURSOR);
    expect(validated.startByte).toBeUndefined();
  });

  it("refuses a malformed token, path, or cursor", () => {
    expect(() => validateSourceReadRequest({ token: "nope", path: "a.rs" })).toThrow(
      SourceViewRequestError,
    );
    expect(() => validateSourceReadRequest({ token: TOKEN, path: "" })).toThrow(/path is required/);
    expect(() => validateSourceReadRequest({ token: TOKEN, path: "a\0b" })).toThrow(/NUL/);
    expect(() => validateSourceReadRequest({ token: TOKEN, path: "x".repeat(5000) })).toThrow(
      /may not exceed/,
    );
    expect(() =>
      validateSourceReadRequest({
        token: TOKEN,
        path: "a.rs",
        cursor: { ...CURSOR, carryHex: "zz" },
      }),
    ).toThrow(/cursor is malformed/);
  });

  it("refuses limits outside the published ceilings", () => {
    expect(() =>
      validateSourceReadRequest({ token: TOKEN, path: "a.rs", maxBytes: 99_999_999 }),
    ).toThrow(/maxBytes/);
    expect(() => validateSourceReadRequest({ token: TOKEN, path: "a.rs", maxLines: 0 })).toThrow(
      /maxLines/,
    );
    expect(() =>
      validateSourceReadRequest({ token: TOKEN, path: "a.rs", maxLineChars: 99_999 }),
    ).toThrow(/maxLineChars/);
  });

  it("validates opaque identifiers without interpreting them", () => {
    expect(validateSourceToken(TOKEN)).toBe(TOKEN);
    expect(() => validateSourceToken(123)).toThrow(SourceViewRequestError);
    expect(validateSnapshotId(SNAP)).toBe(SNAP);
    expect(() => validateSnapshotId("short")).toThrow(SourceViewRequestError);
  });

  it("parses a revoke response in either shape", () => {
    expect(parseRevokeResponse(true)).toBe(true);
    expect(parseRevokeResponse({ revoked: false })).toBe(false);
    expect(() => parseRevokeResponse("yes")).toThrow(/whether a snapshot was revoked/);
  });

  it("refuses an incomplete transport at construction, not at first use", () => {
    expect(() =>
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      assertTransportComplete({ channel: "tauri", snapshot: async () => SNAPSHOT } as any),
    ).toThrow(/must implement `read`/);
    expect(() =>
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      assertTransportComplete({ channel: "carrier-pigeon" } as any),
    ).toThrow(/must implement/);
  });
});

describe("tauri transport", () => {
  it("issues a snapshot and parses it", async () => {
    invoke.mockResolvedValueOnce(SNAPSHOT);
    const snapshot = await tauriSourceViewTransport.snapshot({ sessionId: "session-1" });
    expect(invoke).toHaveBeenCalledWith("source_view_snapshot", { sessionId: "session-1" });
    expect(snapshot.roots[0].token).toBe(TOKEN);
  });

  it("sends the shared read payload", async () => {
    invoke.mockResolvedValueOnce(DOCUMENT);
    await tauriSourceViewTransport.read({ token: TOKEN, path: "src/main.rs", maxBytes: 4096 });
    expect(invoke).toHaveBeenCalledWith(
      "source_view_read",
      sourceReadPayload({ token: TOKEN, path: "src/main.rs", maxBytes: 4096 }),
    );
  });

  it("refuses a malformed request before it reaches the wire", async () => {
    await expect(tauriSourceViewTransport.read({ token: "nope", path: "a.rs" })).rejects.toThrow(
      SourceViewRequestError,
    );
    expect(invoke).not.toHaveBeenCalled();
  });

  it("refuses a response that does not match the contract", async () => {
    invoke.mockResolvedValueOnce({ ...DOCUMENT, absolutePath: "/approved/repo/src/main.rs" });
    await expect(
      tauriSourceViewTransport.read({ token: TOKEN, path: "src/main.rs" }),
    ).rejects.toThrow(/must not carry `absolutePath`/);
  });
});

describe("broker transport", () => {
  it("issues a snapshot with a GET, because issuing one changes nothing", async () => {
    const { transport, calls } = brokerTransport(() => SNAPSHOT);
    const snapshot = await transport.snapshot({ sessionId: "session-1" });
    expect(snapshot.snapshotId).toBe(SNAP);
    expect(calls[0].path).toContain("/bindings/binding-1/source-view/snapshot");
    expect(calls[0].path).toContain("sessionId=session-1");
    expect(calls[0].init?.method ?? "GET").toBe("GET");
  });

  it("reads with a POST carrying the shared payload, CSRF, and idempotency", async () => {
    const { transport, calls } = brokerTransport(() => DOCUMENT);
    await transport.read({ token: TOKEN, path: "src/main.rs" });
    const [call] = calls;
    expect(call.path).toContain("/bindings/binding-1/source-view/read");
    expect(call.init?.method).toBe("POST");
    const headers = new Headers(call.init?.headers);
    expect(headers.get("Idempotency-Key")).toBe("idem-1");
    expect(JSON.parse(String(call.init?.body))).toEqual(
      sourceReadPayload({ token: TOKEN, path: "src/main.rs" }),
    );
  });

  it("refuses a malformed request before it reaches the network", async () => {
    const { transport, calls } = brokerTransport(() => DOCUMENT);
    await expect(transport.read({ token: "nope", path: "a.rs" })).rejects.toThrow(
      SourceViewRequestError,
    );
    expect(calls).toHaveLength(0);
  });

  it("refuses a broker response that leaks a path", async () => {
    const { transport } = brokerTransport(() => ({ ...DOCUMENT, rootPath: "/approved/repo" }));
    await expect(transport.read({ token: TOKEN, path: "src/main.rs" })).rejects.toThrow(
      /must not carry `rootPath`/,
    );
  });

  it("revokes a snapshot by id", async () => {
    const { transport, calls } = brokerTransport(() => ({ revoked: true }));
    await expect(transport.revoke(SNAP)).resolves.toBe(true);
    expect(calls[0].path).toContain("/source-view/revoke");
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({ snapshotId: SNAP });
  });

  it("refuses to revoke a malformed snapshot id", async () => {
    const { transport, calls } = brokerTransport(() => ({ revoked: true }));
    await expect(transport.revoke("nope")).rejects.toThrow(SourceViewRequestError);
    expect(calls).toHaveLength(0);
  });
});

describe("desktop and broker parity", () => {
  it("both channels implement the same closed operation set", () => {
    const { transport: broker } = brokerTransport(() => SNAPSHOT);
    for (const transport of [tauriSourceViewTransport, broker]) {
      expect(() => assertTransportComplete(transport)).not.toThrow();
      for (const operation of SOURCE_VIEW_OPERATIONS) {
        expect(typeof (transport as unknown as Record<string, unknown>)[operation]).toBe("function");
      }
    }
    expect(tauriSourceViewTransport.channel).toBe("tauri");
    expect(broker.channel).toBe("broker");
  });

  it("both channels return the identical parsed document for identical bytes", async () => {
    invoke.mockResolvedValueOnce(DOCUMENT);
    const viaTauri = await tauriSourceViewTransport.read({ token: TOKEN, path: "src/main.rs" });
    const { transport: broker } = brokerTransport(() => DOCUMENT);
    const viaBroker = await broker.read({ token: TOKEN, path: "src/main.rs" });
    expect(viaBroker).toEqual(viaTauri);
  });

  it("both channels refuse the same malformed requests", async () => {
    const { transport: broker } = brokerTransport(() => DOCUMENT);
    const bad = [
      { token: "nope", path: "a.rs" },
      { token: TOKEN, path: "" },
      { token: TOKEN, path: "a\0b" },
      { token: TOKEN, path: "a.rs", maxLines: 99_999 },
      { token: TOKEN, path: "a.rs", startByte: 5, cursor: CURSOR },
    ];
    for (const request of bad) {
      await expect(tauriSourceViewTransport.read(request)).rejects.toThrow(SourceViewRequestError);
      await expect(broker.read(request)).rejects.toThrow(SourceViewRequestError);
    }
  });

  it("both channels refuse the same malformed responses", async () => {
    const leaky = { ...DOCUMENT, cwd: "/approved/repo" };
    invoke.mockResolvedValueOnce(leaky);
    await expect(
      tauriSourceViewTransport.read({ token: TOKEN, path: "src/main.rs" }),
    ).rejects.toThrow(/must not carry `cwd`/);
    const { transport: broker } = brokerTransport(() => leaky);
    await expect(broker.read({ token: TOKEN, path: "src/main.rs" })).rejects.toThrow(
      /must not carry `cwd`/,
    );
  });
});
