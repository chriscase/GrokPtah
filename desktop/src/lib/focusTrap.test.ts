import { describe, expect, it } from "vitest";
import { trapTabKey } from "./focusTrap";

describe("modal focus trap", () => {
  it("wraps forward and reverse Tab at the modal edges", () => {
    const container = document.createElement("div");
    const first = document.createElement("button");
    const last = document.createElement("button");
    container.append(first, last);
    document.body.append(container);

    first.focus();
    const forward = new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    });
    trapTabKey(forward, container);
    expect(forward.defaultPrevented).toBe(false);

    last.focus();
    const wrapForward = new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    });
    trapTabKey(wrapForward, container);
    expect(wrapForward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);

    first.focus();
    const wrapBackward = new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
      shiftKey: true,
    });
    trapTabKey(wrapBackward, container);
    expect(wrapBackward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);

    container.remove();
  });
});
