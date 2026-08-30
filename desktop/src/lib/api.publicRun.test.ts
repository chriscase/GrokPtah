import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

import { api } from "./api";
import { PUBLIC_RUN_SCHEMA_VERSION, PublicRunDtoError } from "./publicRun";

const REQUEST_SESSION = "11111111-1111-4111-8111-111111111111";
const REQUEST_WORKSPACE = "/tmp/project";
const BODY_SESSION = "22222222-2222-4222-8222-222222222222";
const BODY_WORKSPACE = "/tmp/secret-chat";
const SECRET_PROMPT = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
const SECRET_RESPONSE = "wrote tokens to /tmp/secret-chat/credentials.env";
const TS = "2026-08-01T00:00:00Z";
const RUN_ID = "run_public_dto_1";

function publicDocument() {
  return {
    schemaVersion: PUBLIC_RUN_SCHEMA_VERSION,
    runId: RUN_ID,
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

afterEach(() => {
  invoke.mockReset();
});

describe("remote public run API", () => {
  it("lists and gets with explicit request scope, ignoring body session/workspace", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "remote_service_public_run_list") {
        return {
          sessionId: BODY_SESSION,
          workspace: BODY_WORKSPACE,
          schemaVersion: PUBLIC_RUN_SCHEMA_VERSION,
          runs: [{ sessionId: BODY_SESSION, workspace: BODY_WORKSPACE, ...publicDocument() }],
        };
      }
      if (command === "remote_service_public_run_get") {
        return { sessionId: BODY_SESSION, workspace: BODY_WORKSPACE, ...publicDocument() };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const listed = await api.remoteServicePublicRunList(REQUEST_SESSION, REQUEST_WORKSPACE);
    expect(invoke).toHaveBeenCalledWith("remote_service_public_run_list", {
      sessionId: REQUEST_SESSION,
      workspace: REQUEST_WORKSPACE,
    });
    expect(listed.sessionId).toBe(REQUEST_SESSION);
    expect(listed.workspace).toBe(REQUEST_WORKSPACE);
    expect(listed.runs[0]?.sessionId).toBe(REQUEST_SESSION);
    expect(listed.runs[0]?.workspace).toBe(REQUEST_WORKSPACE);
    expect(JSON.stringify(listed)).not.toContain(BODY_SESSION);
    expect(JSON.stringify(listed)).not.toContain(SECRET_PROMPT);

    invoke.mockClear();
    const got = await api.remoteServicePublicRunGet(REQUEST_SESSION, REQUEST_WORKSPACE, RUN_ID);
    expect(invoke).toHaveBeenCalledWith("remote_service_public_run_get", {
      sessionId: REQUEST_SESSION,
      workspace: REQUEST_WORKSPACE,
      runId: RUN_ID,
    });
    expect(got.sessionId).toBe(REQUEST_SESSION);
    expect(got.workspace).toBe(REQUEST_WORKSPACE);
    expect(got.runId).toBe(RUN_ID);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("remote_service_run_list");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("remote_service_run_get");
  });

  it("fails closed on private fields instead of mapping them", async () => {
    invoke.mockResolvedValue({
      ...publicDocument(),
      promptPreview: SECRET_PROMPT,
      finalResponse: SECRET_RESPONSE,
    });

    await expect(
      api.remoteServicePublicRunGet(REQUEST_SESSION, REQUEST_WORKSPACE, RUN_ID),
    ).rejects.toBeInstanceOf(PublicRunDtoError);
    expect(invoke).toHaveBeenCalledWith("remote_service_public_run_get", {
      sessionId: REQUEST_SESSION,
      workspace: REQUEST_WORKSPACE,
      runId: RUN_ID,
    });
  });
});
