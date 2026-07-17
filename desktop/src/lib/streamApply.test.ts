import { describe, expect, it } from "vitest";
import {
  applyAssistantStreamChunk,
  nextAssistantText,
  shouldStickToBottom,
  streamVisualDelta,
} from "./streamApply";

describe("applyAssistantStreamChunk", () => {
  it("appends true deltas", () => {
    expect(nextAssistantText("Hello", " world")).toBe("Hello world");
  });

  it("replaces with cumulative full buffer", () => {
    expect(nextAssistantText("Hel", "Hello")).toBe("Hello");
    expect(nextAssistantText("Hello", "Hello world")).toBe("Hello world");
  });

  it("appends when chunk equals existing (2nd identical unit)", () => {
    expect(nextAssistantText("Hi", "Hi")).toBe("HiHi");
    const long = "x".repeat(100);
    // Long equal is still a legitimate second copy of a line, not skip
    expect(nextAssistantText(long, long)).toBe(long + long);
  });

  it("does not drop 2+ identical short code lines", () => {
    const line = "  return 1;\n";
    let t = "";
    t = nextAssistantText(t, line) as string;
    t = nextAssistantText(t, line) as string;
    expect(t).toBe(line + line);
  });

  it("does not drop 3+ identical short code lines (startsWith rewind trap)", () => {
    // After two lines, existing.startsWith(chunk) is true — old logic skipped.
    const line = "  return 1;\n";
    let t = "";
    for (let i = 0; i < 5; i++) {
      const next = nextAssistantText(t, line);
      expect(next).not.toBe("skip");
      t = next as string;
    }
    expect(t).toBe(line.repeat(5));
  });

  it("does not drop long repeated code lines (no length>=80 skip)", () => {
    // ≥80 chars so this would have hit the old exact-equality length heuristic
    const line = `  const value = ${"x".repeat(90)};\n`;
    expect(line.length).toBeGreaterThanOrEqual(80);
    let t = "";
    t = nextAssistantText(t, line) as string;
    t = nextAssistantText(t, line) as string;
    t = nextAssistantText(t, line) as string;
    expect(t).toBe(line + line + line);
    expect(t.split("\n").filter(Boolean)).toHaveLength(3);
  });

  it("seq drops stale chunks", () => {
    const r = applyAssistantStreamChunk("ab", "c", { seq: 2, lastSeq: 5 });
    expect(r.kind).toBe("skip");
    const r2 = applyAssistantStreamChunk("ab", "c", { seq: 6, lastSeq: 5 });
    expect(r2).toEqual({ kind: "append", text: "abc" });
  });

  it("handles empty / first chunk", () => {
    expect(nextAssistantText("", "")).toBe("skip");
    expect(nextAssistantText("", "Hi")).toBe("Hi");
  });
});

describe("streamVisualDelta", () => {
  it("returns suffix when next extends prev by content", () => {
    expect(streamVisualDelta("Hello", "Hello world")).toEqual({
      reset: false,
      added: " world",
    });
  });

  it("resets when text is not a pure extension (length-only would garble)", () => {
    const d = streamVisualDelta("abcXX", "abcYY");
    expect(d.reset).toBe(true);
    expect(d.added).toBe("abcYY");
  });

  it("no-op when equal", () => {
    expect(streamVisualDelta("same", "same")).toEqual({
      reset: false,
      added: "",
    });
  });
});

describe("shouldStickToBottom", () => {
  it("sticks when near bottom", () => {
    expect(shouldStickToBottom(0)).toBe(true);
    expect(shouldStickToBottom(40)).toBe(true);
    expect(shouldStickToBottom(80)).toBe(true);
  });
  it("does not stick when user scrolled up", () => {
    expect(shouldStickToBottom(120)).toBe(false);
    expect(shouldStickToBottom(500)).toBe(false);
  });
});
