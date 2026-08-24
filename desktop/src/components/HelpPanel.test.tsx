import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { HelpPanel } from "./HelpPanel";

describe("HelpPanel", () => {
  afterEach(cleanup);

  it("searches locally and keeps the authority boundary visible", () => {
    const onClose = () => {};
    render(<HelpPanel open onClose={onClose} audience="operator" includeRestricted />);
    const input = screen.getByRole("textbox", { name: "Search Help Center" });
    fireEvent.change(input, { target: { value: "stale frame" } });
    fireEvent.click(screen.getByRole("button", { name: /Use Computer Use without losing control/i }));
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
});
