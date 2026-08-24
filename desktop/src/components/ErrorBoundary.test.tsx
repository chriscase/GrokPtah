import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function BrokenSurface(): never {
  throw new Error("render failed at /Users/alice/project (api_key=sk-live-secret)");
}

describe("ErrorBoundary", () => {
  it("redacts sensitive render failures before displaying diagnostics", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);

    render(
      <ErrorBoundary label="test surface">
        <BrokenSurface />
      </ErrorBoundary>,
    );

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("The test surface hit a render error.");
    expect(alert).toHaveTextContent("[local path redacted]");
    expect(alert).toHaveTextContent("[redacted]");
    expect(alert).not.toHaveTextContent("/Users/alice/project");
    expect(alert).not.toHaveTextContent("sk-live-secret");
  });
});
