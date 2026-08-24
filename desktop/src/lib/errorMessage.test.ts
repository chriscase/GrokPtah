import { describe, expect, it } from "vitest";
import { safeErrorMessage, sanitizeSensitiveText } from "./errorMessage";

describe("safe backend error messages", () => {
  it("redacts credentials, local paths, and UI placeholders", () => {
    const text = sanitizeSensitiveText(
      "401 api_key=sk-live-value at /Users/chriscase/project; native temp /tmp/grokptah-run; Saved (leave blank to keep)",
    );
    expect(text).toBe(
      "401 [redacted] at [local path redacted]; native temp [local path redacted]; [redacted]",
    );
    expect(sanitizeSensitiveText("helper failed at /private/var/folders/abc/state")).toBe(
      "helper failed at [local path redacted]",
    );
  });

  it("bounds long failures and handles empty unknown values", () => {
    expect(safeErrorMessage(new Error("x".repeat(400)))).toHaveLength(318);
    expect(safeErrorMessage(undefined, "Try again later.")).toBe("Try again later.");
  });
});
