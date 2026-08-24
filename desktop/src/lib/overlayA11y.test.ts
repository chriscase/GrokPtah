import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import {
  FOCUSABLE_SELECTOR,
  focusableIn,
  inertProps,
  isChromeLocked,
  trapTabKey,
} from "./overlayA11y";

const root = dirname(fileURLToPath(import.meta.url));

describe("overlay a11y helpers", () => {
  it("locks chrome for any blocking overlay", () => {
    expect(isChromeLocked({})).toBe(false);
    expect(isChromeLocked({ settingsOpen: true })).toBe(true);
    expect(isChromeLocked({ sessionBrowserOpen: true })).toBe(true);
    expect(isChromeLocked({ permissionOpen: true })).toBe(true);
    expect(isChromeLocked({ searchOpen: true })).toBe(true);
    expect(isChromeLocked({ aboutOpen: true })).toBe(true);
    expect(isChromeLocked({ mcpTrustOpen: true })).toBe(true);
  });

  it("emits an inert attribute only when locked", () => {
    expect(inertProps(false)).toEqual({});
    expect(inertProps(true)).toEqual({ inert: "" });
  });

  it("traps Tab inside a root and wraps both ends", () => {
    const root = document.createElement("div");
    const a = document.createElement("button");
    const b = document.createElement("button");
    a.textContent = "one";
    b.textContent = "two";
    root.append(a, b);
    document.body.append(root);
    a.focus();
    trapTabKey({ key: "Tab", shiftKey: true, preventDefault() {} }, root);
    expect(document.activeElement).toBe(b);
    b.focus();
    trapTabKey({ key: "Tab", shiftKey: false, preventDefault() {} }, root);
    expect(document.activeElement).toBe(a);
    root.remove();
  });

  it("lists only enabled, unhidden focusables", () => {
    const root = document.createElement("div");
    root.innerHTML = `
      <button>ok</button>
      <button disabled>no</button>
      <a href="#x">link</a>
      <span tabindex="-1">skip</span>
    `;
    expect(focusableIn(root).map((el) => el.tagName)).toEqual(["BUTTON", "A"]);
    expect(FOCUSABLE_SELECTOR).toContain("button");
  });
});

describe("App chrome wiring", () => {
  it("spreads inert onto the five operator landmarks while overlays stay siblings", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toContain("isChromeLocked");
    expect(app).toContain("inertProps");
    expect(app).toMatch(/<header className="titlebar" \{\.\.\.lockChrome\}/);
    expect(app).toMatch(/<footer className="status-bar" \{\.\.\.lockChrome\}/);
    expect(app).toContain("permissionOpen: Boolean(permission)");
    expect(app).toContain("sessionBrowserOpen");
  });
});
