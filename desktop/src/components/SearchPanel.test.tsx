import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SearchPanel } from "./SearchPanel";

const mocks = vi.hoisted(() => ({ searchSessions: vi.fn() }));

vi.mock("../lib/api", () => ({
  api: { searchSessions: mocks.searchSessions },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("SearchPanel", () => {
  it("keeps keyboard focus inside the modal and closes on Escape", () => {
    const onClose = vi.fn();
    render(
      <SearchPanel
        open
        onClose={onClose}
        onOpenSession={vi.fn()}
      />,
    );

    const close = screen.getByRole("button", { name: "Close Esc" });
    fireEvent.change(screen.getByPlaceholderText("Search messages, titles, tags, folders…"), {
      target: { value: "demo" },
    });
    const search = screen.getByRole("button", { name: "Search" });

    close.focus();
    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(search);

    fireEvent.keyDown(search, { key: "Tab" });
    expect(document.activeElement).toBe(close);

    fireEvent.keyDown(close, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
