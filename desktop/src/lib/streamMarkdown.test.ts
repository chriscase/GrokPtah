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

  it("keeps a substantial beam tail on long paragraphs", () => {
    const body = "Word ".repeat(80).trim();
    const { stable, tail } = splitStreamingMarkdown(body);
    expect(stable.length).toBeGreaterThan(0);
    expect(tail.length).toBeGreaterThanOrEqual(80);
    expect(stable + tail).toBe(body);
  });

  it("puts recent content in the beam tail", () => {
    const t =
      "First paragraph is done and fairly long so we have room.\n\nSecond is still streaming words here";
    const { stable, tail } = splitStreamingMarkdown(t);
    expect(stable + tail).toBe(t);
    expect(tail.length).toBeGreaterThan(0);
    // Tail should include the newest words
    expect(tail).toMatch(/streaming|words|here/);
  });
});
