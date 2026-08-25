import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { HelpPanel } from "./HelpPanel";

describe("HelpPanel", () => {
  afterEach(cleanup);

  it("searches locally and keeps the authority boundary visible", () => {
    const onClose = () => {};
    render(<HelpPanel open onClose={onClose} audience="operator" includeRestricted />);
    const input = screen.getByRole("textbox", { name: "Search Help Center" });
    fireEvent.change(input, { target: { value: "stale frame" } });
    // Title comes from the consolidated article; the two former corpora
    // titled this entry differently and the merge keeps one title.
    fireEvent.click(screen.getByRole("button", { name: /Computer Use: consent and boundaries/i }));
    expect(screen.getByText(/Help explains behavior only/i)).toBeTruthy();
    expect(screen.getByText("Gated guidance")).toBeTruthy();
  });

  it("keeps restricted guidance out of the default audience", () => {
    render(<HelpPanel open onClose={() => {}} />);
    const inputs = screen.getAllByLabelText("Search Help Center");
    fireEvent.change(inputs[inputs.length - 1], {
      target: { value: "enterprise gateway" },
    });
    expect(screen.queryByRole("button", { name: /restricted company gateway/i })).toBeNull();
  });

  it("enters the search field, wraps keyboard focus, and closes on Escape", () => {
    const onClose = vi.fn();
    render(<HelpPanel open onClose={onClose} />);
    const input = screen.getByRole("textbox", { name: "Search Help Center" });
    const close = screen.getByRole("button", { name: /Close Help Center/i });
    expect(document.activeElement).toBe(input);

    close.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(input);
    input.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(close);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("restores focus to the invoking control when closed", () => {
    const trigger = document.createElement("button");
    trigger.textContent = "Open Help";
    document.body.appendChild(trigger);
    trigger.focus();
    const { rerender } = render(<HelpPanel open onClose={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Search Help Center" }));
    rerender(<HelpPanel open={false} onClose={() => {}} />);
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });
});
