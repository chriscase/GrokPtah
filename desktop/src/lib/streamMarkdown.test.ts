import { describe, expect, it } from "vitest";
import {
  incompleteTableStart,
  splitStreamingMarkdown,
  unclosedFenceStart,
} from "./streamMarkdown";

describe("unclosedFenceStart", () => {
  it("detects open fence", () => {
    const t = "Intro\n\n```ts\nconst x = 1\n";
    expect(unclosedFenceStart(t)).toBeGreaterThan(0);
  });
  it("returns -1 when fence closed", () => {
    const t = "Intro\n\n```ts\nconst x = 1\n```\nmore";
    expect(unclosedFenceStart(t)).toBe(-1);
  });
});

describe("incompleteTableStart", () => {
  it("flags header without separator", () => {
    const t = "Before\n\n| A | B |\n| C |";
    expect(incompleteTableStart(t)).toBeGreaterThan(0);
  });
  it("allows complete mini table", () => {
    const t = "Before\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
    expect(incompleteTableStart(t)).toBe(-1);
  });
});

describe("splitStreamingMarkdown", () => {
  it("puts unclosed fence entirely in tail", () => {
    const { stable, tail } = splitStreamingMarkdown(
      "## Title\n\n```js\nconsole.log(1\n",
    );
    expect(stable).toContain("Title");
    expect(tail.startsWith("```")).toBe(true);
  });

  it("keeps a beam tail on long single paragraphs", () => {
    const body = "Word ".repeat(40).trim();
    const { stable, tail } = splitStreamingMarkdown(body);
    expect(stable.length).toBeGreaterThan(0);
    expect(tail.length).toBeGreaterThan(0);
    expect(stable + tail).toBe(body);
  });

  it("splits at paragraph boundaries when present", () => {
    const t = "First paragraph is done.\n\nSecond is still stream";
    const { stable, tail } = splitStreamingMarkdown(t);
    expect(stable).toContain("First paragraph");
    expect(tail).toContain("Second");
    expect(stable + tail).toBe(t);
  });
});
