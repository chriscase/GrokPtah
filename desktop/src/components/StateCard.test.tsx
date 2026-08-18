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
});
