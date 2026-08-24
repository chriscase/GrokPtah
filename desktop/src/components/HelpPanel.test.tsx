import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { HelpPanel } from "./HelpPanel";

describe("HelpPanel", () => {
  it("searches locally and keeps the authority boundary visible", () => {
    const onClose = () => {};
    render(<HelpPanel open onClose={onClose} />);
    const input = screen.getByRole("textbox", { name: "Search Help Center" });
    fireEvent.change(input, { target: { value: "stale frame" } });
    fireEvent.click(screen.getByRole("button", { name: /Use Computer Use without losing control/i }));
    expect(screen.getByText(/Help explains behavior only/i)).toBeTruthy();
    expect(screen.getByText("Gated guidance")).toBeTruthy();
  });
});
