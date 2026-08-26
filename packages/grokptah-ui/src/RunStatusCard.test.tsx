import { readFileSync } from "node:fs";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RunStatusCard, type RunStatusSnapshot } from "./RunStatusCard";

const themeSource = readFileSync("src/theme.css", "utf8");
const componentSource = readFileSync("src/RunStatusCard.tsx", "utf8");

describe("RunStatusCard", () => {
  it("exposes an accessible name, description, and native round-meter semantics", () => {
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

    expect(screen.getByText("Round 3 of 8 maximum")).toBeInTheDocument();
    const meter = screen.getByRole("meter", { name: "Round budget used" });
    expect(meter).toHaveAttribute("value", "3");
    expect(meter).toHaveAttribute("min", "0");
    expect(meter).toHaveAttribute("max", "8");
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

  it("keeps state semantics independent from early completion", () => {
    const { rerender } = render(
      <RunStatusCard
        snapshot={{
          state: "completed",
          progress: { round: 3, maxRounds: 12 },
        }}
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent("Run completed.");
    expect(screen.getByRole("article")).toHaveAttribute("data-state", "completed");
    expect(screen.getByText("Round 3 of 12 maximum")).toBeInTheDocument();

    rerender(<RunStatusCard snapshot={{ state: "failed" }} />);
    expect(screen.getByRole("status")).toHaveTextContent("Run failed.");
    expect(screen.queryByRole("meter")).not.toBeInTheDocument();
    expect(screen.queryByText(/Round \d+ of \d+ maximum/)).not.toBeInTheDocument();
  });

  it("omits absent, malformed, oversized, and inconsistent round budgets", () => {
    const { rerender } = render(<RunStatusCard snapshot={{ state: "running" }} />);
    expect(screen.queryByRole("meter")).not.toBeInTheDocument();
    expect(screen.queryByText(/Round \d+ of \d+ maximum/)).not.toBeInTheDocument();

    const invalidBudgets = [
      { round: -1, maxRounds: 4 },
      { round: 1, maxRounds: 101 },
      { round: 5, maxRounds: 4 },
      { round: Number.NaN, maxRounds: 4 },
      { round: 1, maxRounds: Number.POSITIVE_INFINITY },
      { round: 0, maxRounds: 0 },
    ];
    for (const progress of invalidBudgets) {
      rerender(
        <RunStatusCard
          snapshot={{ state: "running", progress }}
        />,
      );
      expect(screen.queryByRole("meter")).not.toBeInTheDocument();
      expect(screen.queryByText(/Round \d+ of \d+ maximum/)).not.toBeInTheDocument();
    }
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
    expect(componentSource).not.toContain('aria-label="Run progress"');
    expect(themeSource).toContain("@media (forced-colors: active)");
    expect(themeSource).toContain("@media (prefers-reduced-motion: reduce)");
    expect(themeSource).toContain("@container gpt-ui-run-status");
    expect(themeSource).toContain("@media (max-width: 20rem)");
    expect(themeSource).toContain("overflow-wrap: anywhere");
  });
});
