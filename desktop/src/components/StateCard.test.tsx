import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StateCard } from "./StateCard";

afterEach(cleanup);

describe("StateCard", () => {
  it("keeps technical diagnostics behind disclosure and exposes recovery", () => {
    const onAction = vi.fn();
    render(
      <StateCard
        variant="error"
        title="Could not refresh"
        description="Saved data is unchanged."
        actionLabel="Try again"
        onAction={onAction}
        technicalDetail="store is already open (os error 35)"
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("Saved data is unchanged.");
    expect(screen.getByRole("alert")).not.toHaveTextContent("os error 35");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onAction).toHaveBeenCalledOnce();
  });

  it("renders an empty state without presenting it as a failure", () => {
    render(
      <StateCard
        variant="empty"
        title="No durable Agents yet"
        description="Complete a Build turn to create one."
      />,
    );

    expect(screen.getByText("No durable Agents yet")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it.each([
    ["loading", "status"],
    ["stale", "status"],
    ["disconnected", "status"],
    ["queued", "status"],
    ["approval", "status"],
    ["blocked", "alert"],
    ["error", "alert"],
  ] as const)("announces %s with the expected live semantic", (variant, role) => {
    render(
      <StateCard
        variant={variant}
        title={`${variant} title`}
        description={`${variant} description`}
      />,
    );

    expect(screen.getByRole(role)).toHaveTextContent(`${variant} description`);
    expect(screen.getByText(`${variant} title`).closest(".state-card")).toHaveAttribute(
      "data-state",
      variant,
    );
  });

  it.each(["archived", "retired", "proposed"] as const)(
    "keeps the %s state informative without an urgent announcement",
    (variant) => {
      render(
        <StateCard
          variant={variant}
          title={`${variant} title`}
          description={`${variant} description`}
        />,
      );

      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.queryByRole("status")).toBeNull();
    },
  );
});
