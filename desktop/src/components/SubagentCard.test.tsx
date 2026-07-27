import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SubagentInfo } from "../lib/protocol";
import { SubagentCard } from "./SubagentCard";

afterEach(cleanup);

function agent(
  execution_mode: SubagentInfo["execution_mode"],
): SubagentInfo {
  return {
    id: "child-1",
    kind: "general-purpose",
    title: "Fix the failing tests",
    status: "running",
    cwd: "/workspace/.grokptah/worktrees/sub-child-1",
    execution_mode,
  };
}

describe("SubagentCard isolation visibility", () => {
  it("shows the isolated cwd without a shared-mutation warning", () => {
    render(<SubagentCard agent={agent("worktree")} onCancel={vi.fn()} />);

    expect(screen.getByText("Isolated worktree")).toBeTruthy();
    expect(
      screen.getByTitle("/workspace/.grokptah/worktrees/sub-child-1"),
    ).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("warns when a mutating child shares the parent cwd", () => {
    render(
      <SubagentCard agent={agent("shared_mutating")} onCancel={vi.fn()} />,
    );

    expect(screen.getByText("Shared / writes enabled")).toBeTruthy();
    expect(screen.getByRole("alert")).toHaveTextContent(
      /edit the parent session's working folder/i,
    );
  });

  it("keeps the existing cancel-one-child action", async () => {
    const onCancel = vi.fn().mockResolvedValue(undefined);
    render(<SubagentCard agent={agent("worktree")} onCancel={onCancel} />);

    fireEvent.click(screen.getByText("Cancel child"));
    await waitFor(() => expect(onCancel).toHaveBeenCalledWith("child-1"));
  });
});
