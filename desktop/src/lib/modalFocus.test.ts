import { describe, expect, it } from "vitest";
import { focusableIn } from "./modalFocus";

describe("modal focus trap membership", () => {
  it("omits tabindex=-1 options and inert nodes, and includes native summary", () => {
    const root = document.createElement("div");
    root.innerHTML = `
      <button type="button">Close</button>
      <button type="button" tabindex="0">Selected</button>
      <button type="button" tabindex="-1">Hidden option</button>
      <details open>
        <summary>Review exact metadata</summary>
      </details>
      <div inert>
        <button type="button">Inert</button>
        <summary>Inert summary</summary>
      </div>
    `;
    document.body.appendChild(root);

    const stops = focusableIn(root);
    expect(stops.map((el) => el.textContent?.trim())).toEqual([
      "Close",
      "Selected",
      "Review exact metadata",
    ]);

    root.remove();
  });
});
