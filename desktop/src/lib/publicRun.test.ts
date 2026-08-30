import { describe, expect, it } from "vitest";

import {
  PUBLIC_RUN_SCHEMA_VERSION,
  PublicRunDtoError,
  parsePublicRunHandoffV1,
  parsePublicRunListV1,
  parsePublicRunProgressV1,
  parsePublicRunV1,
  type PublicRunHandoffV1,
  type PublicRunListV1,
  type PublicRunProgressV1,
  type PublicRunV1,
} from "./publicRun";

const SECRET_PROMPT = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
const SECRET_RESPONSE = "wrote tokens to /tmp/secret-chat/credentials.env";
const SECRET_PATH = "/tmp/secret-chat/credentials.env";
const SECRET_CWD = "/tmp/secret-chat";
const SECRET_TOOL = "cat /tmp/secret-chat/credentials.env";
const OPAQUE_RUN = "run_public_dto_1";
const TS = "2026-08-01T00:00:00Z";

const PRIVATE_KEYS = [
  "promptPreview",
  "finalResponse",
  "workspace",
  "clientId",
  "requestId",
  "sessionId",
  "providerId",
  "leaseId",
  "attemptId",
  "workAttemptId",
  "execution",
  "approval",
  "aggregates",
  "progress",
  "path",
  "sourceWorkspace",
  "executionWorkspace",
  "changedFiles",
  "lastTool",
  "bounds",
  "agentId",
] as const;

function publicRun(): PublicRunV1 {
  return {
    schemaVersion: PUBLIC_RUN_SCHEMA_VERSION,
    runId: OPAQUE_RUN,
    state: "completed",
    createdAt: TS,
    updatedAt: TS,
    queuePosition: 2,
    eventStartSeq: 3,
    eventEndSeq: 8,
    changeCount: 1,
    testCount: 1,
    permissionRequestedCount: 2,
    permissionGrantedCount: 1,
    permissionDeniedCount: 1,
    usagePromptTokens: 10,
    usageCompletionTokens: 4,
    usageTotalTokens: 14,
    usageRequestCount: 1,
    usageComplete: true,
    usagePendingRequestCount: 0,
    progressRound: 3,
    progressMaxRounds: 4,
  };
}

function publicList(): PublicRunListV1 {
  return { schemaVersion: PUBLIC_RUN_SCHEMA_VERSION, runs: [publicRun()] };
}

function publicProgress(): PublicRunProgressV1 {
  return {
    schemaVersion: PUBLIC_RUN_SCHEMA_VERSION,
    runId: OPAQUE_RUN,
    state: "completed",
    busy: true,
    createdAt: TS,
    updatedAt: TS,
    queuePosition: 2,
    eventStartSeq: 3,
    eventEndSeq: 8,
    progressRound: 3,
    progressMaxRounds: 4,
  };
}

function publicHandoff(): PublicRunHandoffV1 {
  return {
    schemaVersion: PUBLIC_RUN_SCHEMA_VERSION,
    runId: OPAQUE_RUN,
    state: "completed",
    createdAt: TS,
    updatedAt: TS,
    eventStartSeq: 3,
    eventEndSeq: 8,
    changeCount: 1,
    testCount: 1,
    usagePromptTokens: 10,
    usageCompletionTokens: 4,
    usageTotalTokens: 14,
    usageRequestCount: 1,
    usageComplete: true,
    usagePendingRequestCount: 0,
  };
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function runRecordWire(): Record<string, unknown> {
  return {
    runId: OPAQUE_RUN,
    sessionId: "11111111-1111-4111-8111-111111111111",
    workspace: SECRET_CWD,
    requestId: "request-secret",
    clientId: "mcp",
    state: "completed",
    promptPreview: SECRET_PROMPT,
    startSeq: 3,
    endSeq: 8,
    createdAt: TS,
    updatedAt: TS,
    finalResponse: SECRET_RESPONSE,
    bounds: { maxPromptBytes: 1000, maxRounds: 4, maxDurationMs: 1000 },
    aggregates: {
      changes: [{ path: SECRET_PATH, summary: "leaked" }],
      tests: [{ callId: "t1", command: SECRET_TOOL, status: "passed" }],
      permissionsRequested: 2,
      permissionsGranted: 1,
      permissionsDenied: 1,
      usage: { promptTokens: 10, completionTokens: 4, totalTokens: 14, requests: 1 },
    },
    progress: {
      round: 3,
      maxRounds: 4,
      lastTool: SECRET_TOOL,
      detail: SECRET_PATH,
      updatedAt: TS,
    },
  };
}

function errorBlob(error: unknown): string {
  return `${String(error)} ${JSON.stringify(error, Object.getOwnPropertyNames(error as object))}`;
}

function expectDecode(error: unknown): PublicRunDtoError {
  expect(error).toBeInstanceOf(PublicRunDtoError);
  const dtoError = error as PublicRunDtoError;
  expect(dtoError.kind).toBe("decode");
  expect(dtoError.message).toBe("public-run dto decode failed");
  return dtoError;
}

function expectNoSecrets(error: unknown): void {
  const blob = errorBlob(error);
  for (const needle of [SECRET_PROMPT, SECRET_RESPONSE, SECRET_PATH, SECRET_CWD, SECRET_TOOL]) {
    expect(blob, `error leaked ${needle}`).not.toContain(needle);
  }
}

describe("parsePublicRunV1", () => {
  it("round-trips an allowlisted get document", () => {
    const expected = publicRun();
    expect(parsePublicRunV1(clone(expected))).toEqual(expected);
  });

  it("treats missing optional counters as null", () => {
    const row = clone(publicRun()) as Record<string, unknown>;
    delete row.queuePosition;
    delete row.eventStartSeq;
    delete row.eventEndSeq;
    delete row.progressRound;
    delete row.progressMaxRounds;
    expect(parsePublicRunV1(row)).toMatchObject({
      queuePosition: null,
      eventStartSeq: null,
      eventEndSeq: null,
      progressRound: null,
      progressMaxRounds: null,
    });
  });

  it("rejects an unknown schema version", () => {
    const row = clone(publicRun()) as Record<string, unknown>;
    row.schemaVersion = "grokptah.public-run.v2";
    try {
      parsePublicRunV1(row);
      throw new Error("expected unknown schema version");
    } catch (error) {
      expect(error).toBeInstanceOf(PublicRunDtoError);
      const dtoError = error as PublicRunDtoError;
      expect(dtoError.kind).toBe("unknown_schema_version");
      expect(dtoError.schemaVersion).toBe("grokptah.public-run.v2");
      expect(dtoError.message).toBe("unknown public-run schema version: grokptah.public-run.v2");
      expectNoSecrets(error);
    }
  });

  it("rejects a missing schema version", () => {
    const row = clone(publicRun()) as Record<string, unknown>;
    delete row.schemaVersion;
    try {
      parsePublicRunV1(row);
      throw new Error("expected missing schema version");
    } catch (error) {
      const dtoError = expectDecode(error);
      expect(dtoError.problem).toBe("missing required field");
      expectNoSecrets(error);
    }
  });

  it("rejects private and unknown keys without leaking values", () => {
    for (const key of PRIVATE_KEYS) {
      const row = clone(publicRun()) as Record<string, unknown>;
      row[key] = key === "promptPreview" || key === "finalResponse" ? SECRET_PROMPT : SECRET_PATH;
      try {
        parsePublicRunV1(row);
        throw new Error(`expected ${key} to be rejected`);
      } catch (error) {
        const dtoError = expectDecode(error);
        expect(dtoError.problem).toBe("unknown field");
        expectNoSecrets(error);
      }
    }
  });

  it("rejects current RunRecord wire", () => {
    try {
      parsePublicRunV1(runRecordWire());
      throw new Error("expected RunRecord rejection");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }
  });
});

describe("parsePublicRunListV1", () => {
  it("round-trips an allowlisted list envelope", () => {
    const expected = publicList();
    expect(parsePublicRunListV1(clone(expected))).toEqual(expected);
  });

  it("rejects a nested unknown schema version", () => {
    const row = clone(publicList()) as { runs: Array<Record<string, unknown>> };
    row.runs[0].schemaVersion = "grokptah.public-run.v0";
    try {
      parsePublicRunListV1(row);
      throw new Error("expected nested unknown schema version");
    } catch (error) {
      expect(error).toBeInstanceOf(PublicRunDtoError);
      const dtoError = error as PublicRunDtoError;
      expect(dtoError.kind).toBe("unknown_schema_version");
      expect(dtoError.schemaVersion).toBe("grokptah.public-run.v0");
      expectNoSecrets(error);
    }
  });

  it("rejects private keys on the envelope and nested runs", () => {
    const envelope = clone(publicList()) as Record<string, unknown>;
    envelope.workspace = SECRET_CWD;
    try {
      parsePublicRunListV1(envelope);
      throw new Error("expected list envelope rejection");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }

    const nested = clone(publicList()) as { runs: Array<Record<string, unknown>> };
    nested.runs[0].promptPreview = SECRET_PROMPT;
    try {
      parsePublicRunListV1(nested);
      throw new Error("expected nested private field rejection");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }
  });
});

describe("parsePublicRunProgressV1", () => {
  it("round-trips an allowlisted progress document", () => {
    const expected = publicProgress();
    expect(parsePublicRunProgressV1(clone(expected))).toEqual(expected);
  });

  it("rejects nested progress aggregates and private keys", () => {
    const row = clone(publicProgress()) as Record<string, unknown>;
    row.progress = { round: 3, lastTool: SECRET_TOOL, detail: SECRET_PATH };
    row.promptPreview = SECRET_PROMPT;
    try {
      parsePublicRunProgressV1(row);
      throw new Error("expected progress private field rejection");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }
  });

  it("does not accept a get document as progress", () => {
    try {
      parsePublicRunProgressV1(publicRun());
      throw new Error("expected get-as-progress rejection");
    } catch (error) {
      expectDecode(error);
    }
  });
});

describe("parsePublicRunHandoffV1", () => {
  it("round-trips an allowlisted handoff document", () => {
    const expected = publicHandoff();
    expect(parsePublicRunHandoffV1(clone(expected))).toEqual(expected);
  });

  it("rejects finalResponse, paths, and nested aggregates", () => {
    const row = clone(publicHandoff()) as Record<string, unknown>;
    row.finalResponse = SECRET_RESPONSE;
    row.changes = [{ path: SECRET_PATH, summary: "leaked" }];
    row.aggregates = { tests: [{ command: SECRET_TOOL }] };
    try {
      parsePublicRunHandoffV1(row);
      throw new Error("expected handoff private field rejection");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }
  });

  it("rejects missing schema version without leaking the rest of the body", () => {
    const row = clone(publicHandoff()) as Record<string, unknown>;
    row.finalResponse = SECRET_RESPONSE;
    delete row.schemaVersion;
    try {
      parsePublicRunHandoffV1(row);
      throw new Error("expected missing handoff schema version");
    } catch (error) {
      expectDecode(error);
      expectNoSecrets(error);
    }
  });
});

describe("public-run shape isolation", () => {
  it("does not parse current RunRecord as list, progress, or handoff", () => {
    const wire = runRecordWire();
    for (const parse of [
      () => parsePublicRunListV1({ runs: [wire] }),
      () => parsePublicRunProgressV1(wire),
      () => parsePublicRunHandoffV1(wire),
    ]) {
      try {
        parse();
        throw new Error("expected RunRecord rejection");
      } catch (error) {
        expectDecode(error);
        expectNoSecrets(error);
      }
    }
  });
});
