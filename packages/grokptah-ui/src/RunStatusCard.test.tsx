import { readFileSync } from "node:fs";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RunStatusCard, type RunStatusSnapshot } from "./RunStatusCard";

const themeSource = readFileSync("src/theme.css", "utf8");

describe("RunStatusCard", () => {
  it("exposes an accessible name, description, and native progress semantics", () => {
    render(
      <RunStatusCard
        snapshot={{
          state: "running",
          progress: { round: 3, maxRounds: 8 },
        }}
      />,
    );

    const article = screen.getByRole("article", { name: "Run status" });
    expect(article).toHaveAccessibleDescription("The run is in progress.");

    const progress = screen.getByRole("progressbar", { name: "Run progress" });
    expect(progress).toHaveAttribute("value", "3");
    expect(progress).toHaveAttribute("max", "8");
  });

  it("uses fixed polite atomic live text for every accepted state", () => {
    const cases = [
      ["queued", "Run is queued."],
      ["running", "Run is running."],
      ["completed", "Run completed."],
      ["failed", "Run failed."],
      ["cancelled", "Run was cancelled."],
      ["interrupted", "Run was interrupted."],
      ["limit_reached", "Run reached its configured limit."],
    ] as const;

    for (const [state, liveText] of cases) {
      const { unmount } = render(
        <RunStatusCard snapshot={{ state }} />,
      );
      const live = screen.getByRole("status");
      expect(live).toHaveTextContent(liveText);
      expect(live).toHaveAttribute("aria-live", "polite");
      expect(live).toHaveAttribute("aria-atomic", "true");
      expect(screen.getByRole("article")).toHaveAttribute("data-state", state);
      unmount();
    }
  });

  it("keeps state semantics independent from progress and never synthesizes success", () => {
    render(
      <RunStatusCard
        snapshot={{
          state: "running",
          progress: { round: 12, maxRounds: 12 },
        }}
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent("Run is running.");
    expect(screen.getByRole("article")).toHaveAttribute("data-state", "running");
    expect(screen.getByRole("progressbar")).toHaveAttribute("value", "12");
  });

  it("bounds valid progress and refuses malformed progress", () => {
    const { rerender } = render(
      <RunStatusCard
        snapshot={{
          state: "running",
          progress: { round: -4, maxRounds: 1_000 },
        }}
      />,
    );

    let progress = screen.getByRole("progressbar");
    expect(progress).toHaveAttribute("value", "0");
    expect(progress).toHaveAttribute("max", "100");

    rerender(
      <RunStatusCard
        snapshot={{
          state: "running",
          progress: { round: 10, maxRounds: 4 },
        }}
      />,
    );
    progress = screen.getByRole("progressbar");
    expect(progress).toHaveAttribute("value", "4");
    expect(progress).toHaveAttribute("max", "4");

    rerender(
      <RunStatusCard
        snapshot={
          {
            state: "running",
            progress: { round: Number.NaN, maxRounds: 4 },
          } as unknown as RunStatusSnapshot
        }
      />,
    );
    progress = screen.getByRole("progressbar");
    expect(progress).not.toHaveAttribute("value");
    expect(progress).not.toHaveAttribute("max");
  });

  it("renders no raw identity or detail fields from a wider projection", () => {
    const { container } = render(
      <RunStatusCard
        snapshot={
          {
            state: "running",
            progress: { round: 1, maxRounds: 4 },
            brokerRunId: "raw-run-id",
            bindingId: "raw-binding-id",
            prompt: "private prompt",
            detail: "hidden detail",
          } as unknown as RunStatusSnapshot
        }
      />,
    );

    expect(container).not.toHaveTextContent("raw-run-id");
    expect(container).not.toHaveTextContent("raw-binding-id");
    expect(container).not.toHaveTextContent("private prompt");
    expect(container).not.toHaveTextContent("hidden detail");
  });

  it("contains responsive accessibility safeguards in the component stylesheet", () => {
    expect(themeSource).toContain("@media (forced-colors: active)");
    expect(themeSource).toContain("@media (prefers-reduced-motion: reduce)");
    expect(themeSource).toContain("@container gpt-ui-run-status");
    expect(themeSource).toContain("@media (max-width: 20rem)");
    expect(themeSource).toContain("overflow-wrap: anywhere");
  });
});
