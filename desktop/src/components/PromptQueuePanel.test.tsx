import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PromptQueuePanel } from "./PromptQueuePanel";
import { createPromptQueueEntry } from "../lib/promptQueue";

afterEach(cleanup);

function entry(id: string, text: string) {
  return createPromptQueueEntry(text, {
    id,
    created_at: "2026-07-27T00:00:00Z",
  });
}

describe("PromptQueuePanel", () => {
  it("edits, removes, clears, reorders, steers, and promotes entries", async () => {
    const entries = [entry("a", "first"), entry("b", "second")];
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const onRemove = vi.fn().mockResolvedValue(undefined);
    const onClear = vi.fn().mockResolvedValue(undefined);
    const onMove = vi.fn().mockResolvedValue(undefined);
    const onSteer = vi.fn().mockResolvedValue(undefined);
    const onRunNext = vi.fn().mockResolvedValue(undefined);

    render(
      <PromptQueuePanel
        entries={entries}
        busy
        onEdit={onEdit}
        onRemove={onRemove}
        onClear={onClear}
        onMove={onMove}
        onSteer={onSteer}
        onRunNext={onRunNext}
      />,
    );

    const first = screen.getByLabelText("Queued prompt 1");
    fireEvent.change(first, { target: { value: "edited first" } });
    fireEvent.blur(first);
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith(entries[0], "edited first"),
    );

    fireEvent.click(screen.getByLabelText("Move queued prompt 2 up"));
    await waitFor(() => expect(onMove).toHaveBeenCalledWith(entries[1], 0));

    fireEvent.click(screen.getAllByText("Steer now")[0]);
    await waitFor(() => expect(onSteer).toHaveBeenCalledWith(entries[0]));

    fireEvent.click(screen.getAllByText("Run next")[1]);
    await waitFor(() => expect(onRunNext).toHaveBeenCalledWith(entries[1]));

    fireEvent.click(screen.getByLabelText("Remove queued prompt 1"));
    await waitFor(() => expect(onRemove).toHaveBeenCalledWith(entries[0]));

    fireEvent.click(screen.getByText("Clear"));
    await waitFor(() => expect(onClear).toHaveBeenCalledTimes(1));
  });

  it("explains that steering does not stop the current turn", () => {
    render(
      <PromptQueuePanel
        entries={[entry("a", "guide it")]}
        busy
        onEdit={vi.fn()}
        onRemove={vi.fn()}
        onClear={vi.fn()}
        onMove={vi.fn()}
        onSteer={vi.fn()}
        onRunNext={vi.fn()}
      />,
    );

    expect(screen.getByText(/guides the current turn without stopping/i)).toBeTruthy();
    expect(screen.getByText("Steer now")).toHaveAttribute(
      "title",
      expect.stringMatching(/without stopping/i),
    );
  });

  it("labels steering states and contains failed mutations", async () => {
    const onSteer = vi.fn().mockRejectedValue(new Error("stale queue"));
    const onError = vi.fn();
    render(
      <PromptQueuePanel
        entries={[
          entry("a", "guide it"),
          { ...entry("b", "defer it"), source: "steering_deferred" },
          { ...entry("c", "next"), priority: true },
        ]}
        busy
        onEdit={vi.fn()}
        onRemove={vi.fn()}
        onClear={vi.fn()}
        onMove={vi.fn()}
        onSteer={onSteer}
        onRunNext={vi.fn()}
        onError={onError}
      />,
    );

    expect(screen.getByText("Queued")).toBeTruthy();
    expect(screen.getByText("Steering deferred")).toBeTruthy();
    expect(document.querySelector(".prompt-queue-state.run-next")).toBeTruthy();

    fireEvent.click(screen.getAllByText("Steer now")[0]);
    await waitFor(() => expect(onError).toHaveBeenCalledWith("stale queue"));
    expect(screen.getByText("Update failed")).toBeTruthy();
  });
});
