import { describe, expect, it } from "vitest";
import {
  applyAssistantStreamChunk,
  emptyPromptQueueState,
  promptQueueReducer,
  queueKind,
} from "./uiCore";

describe("headless UI integration barrel", () => {
  it("exposes pure stream and queue primitives without a desktop shell", () => {
    expect(applyAssistantStreamChunk("", "hello")).toEqual({
      kind: "replace",
      text: "hello",
    });
    expect(queueKind("/review")).toBe("command");
    const state = promptQueueReducer(emptyPromptQueueState, {
      type: "add",
      sessionId: "session-1",
      entry: {
        id: "entry-1",
        version: 0,
        text: "review",
        kind: "prompt",
        source: "composer",
        owner: "desktop",
        created_at: "2026-08-24T00:00:00Z",
        priority: false,
      },
    });
    expect(state.entries["session-1"]?.[0]?.text).toBe("review");
  });
});
