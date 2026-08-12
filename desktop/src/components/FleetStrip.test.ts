import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { queueSummary } from "./FleetStrip";
import type { PromptQueueEntry } from "../lib/promptQueue";

const root = dirname(fileURLToPath(import.meta.url));

describe("FleetStrip cockpit exceed (#174 continuous)", () => {
  it("exposes subagent/token metrics and a11y labels on cards", () => {
    const src = readFileSync(join(root, "FleetStrip.tsx"), "utf8");
    expect(src).toMatch(/runningSubagents/);
    expect(src).toMatch(/totalTokens/);
    expect(src).toMatch(/aria-pressed/);
    expect(src).toMatch(/aria-label/);
    expect(src).toMatch(/data-testid="fleet-card"/);
    expect(src).toMatch(/data-running-subagents/);
    expect(src).toMatch(/data-queue-count/);
    expect(src).toMatch(/steering_deferred/);
    expect(src).toMatch(/runOrigin/);
    expect(src).toMatch(/MCP coordinator/);
  });

  it("summarizes queued and steering work for background sessions", () => {
    const entry = (source: PromptQueueEntry["source"]): PromptQueueEntry => ({
      id: source,
      version: 0,
      text: "demo",
      kind: "prompt",
      source,
      owner: "mcp",
      created_at: "2026-08-12T00:00:00Z",
      priority: false,
    });
    expect(queueSummary([entry("composer"), entry("steering_deferred")])).toEqual([
      "2 queued",
      "1 steer",
    ]);
  });
});
