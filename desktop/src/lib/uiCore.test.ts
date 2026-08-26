import { describe, expect, it } from "vitest";
import {
  applyAssistantStreamChunk,
  emptyPromptQueueState,
  promptQueueReducer,
  queueKind,
  searchHelpCorpus,
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

  it("exposes offline Help retrieval over the public corpus", () => {
    const outcome = searchHelpCorpus("recover an interrupted run");
    expect(outcome.kind).toBe("results");
    if (outcome.kind !== "results") return;
    expect(outcome.results[0]?.articleId).toBe("operations.durable-recovery");
    expect(outcome.mode).toBe("offline-hybrid");
  });

  it("abstains rather than guessing when the corpus cannot answer", () => {
    // The barrel ships the abstention behaviour, not just the ranking: a
    // consumer must be able to tell "no answer" from "a weak answer".
    const outcome = searchHelpCorpus("what is the capital of Portugal");
    expect(outcome.kind).toBe("abstained");
  });
});
