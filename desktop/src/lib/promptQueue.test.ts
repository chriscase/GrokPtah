import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const root = dirname(fileURLToPath(import.meta.url));

describe("prompt queue / interject (#147)", () => {
  it("App queues when busy and supports interject + drain", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/promptQueues/);
    expect(app).toMatch(/interject/);
    expect(app).toMatch(/fromQueue/);
    expect(app).toMatch(/Queued while turn runs/);
    // Send stays enabled while busy so users can queue
    expect(app).toMatch(/disabled=\{!composer\.trim\(\)\}/);
    expect(app).not.toMatch(/disabled=\{busy \|\| !composer\.trim\(\)\}/);
    // Enter must queue while busy (not no-op)
    expect(app).toMatch(/Enter queues/);
    expect(app).not.toMatch(/if \(!busy\) void sendPrompt\(\)/);
  });
});

describe("background tasks panel (#52)", () => {
  it("exposes schedule shell and cancel/adopt for long-running work", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/Schedule scan/);
    expect(app).toMatch(/Schedule shell/);
    expect(app).toMatch(/Open session/);
    expect(app).toMatch(/Background \/ scheduled/);
    expect(app).toMatch(/background_task/);
  });
});

describe("terminal design system (#129)", () => {
  it("uses design tokens and Tab N labels, not raw green PTY banners", () => {
    const term = readFileSync(
      join(root, "..", "components", "TerminalPane.tsx"),
      "utf8",
    );
    expect(term).toMatch(/--surface-deep/);
    expect(term).toMatch(/--accent/);
    expect(term).toMatch(/Tab \{i \+ 1\}/);
    expect(term).not.toMatch(/\\x1b\[32mGrokPtah terminal/);
  });
});
