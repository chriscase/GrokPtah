import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { afterEach, describe, expect, it } from "vitest";
import {
  FOCUSABLE_SELECTOR,
  focusableIn,
  inertProps,
  inertSiblings,
  isChromeLocked,
  isShellInert,
  trapTabKey,
} from "./overlayA11y";
import { resolveWorkspaceShortcut } from "./workspaceShortcuts";

const here = dirname(fileURLToPath(import.meta.url));

afterEach(() => {
  document.body.innerHTML = "";
});

function trapEvent(shiftKey: boolean) {
  let prevented = false;
  return {
    event: {
      key: "Tab",
      shiftKey,
      preventDefault() {
        prevented = true;
      },
    },
    get prevented() {
      return prevented;
    },
  };
}

describe("overlay a11y primitives", () => {
  it("locks chrome for every overlay authority boundary", () => {
    expect(isChromeLocked({})).toBe(false);
    for (const flag of [
      "settingsOpen",
      "sessionBrowserOpen",
      "permissionOpen",
      "searchOpen",
      "aboutOpen",
      "mcpTrustOpen",
      "helpOpen",
      "debugOpen",
    ] as const) {
      expect(isChromeLocked({ [flag]: true }), flag).toBe(true);
    }
  });

  it("emits an inert attribute only when locked", () => {
    expect(inertProps(false)).toEqual({});
    expect(inertProps(true)).toEqual({ inert: "" });
  });

  it("lists only enabled, unhidden, reachable focusables", () => {
    const root = document.createElement("div");
    root.innerHTML = `
      <button>ok</button>
      <button disabled>disabled</button>
      <a href="#x">link</a>
      <span tabindex="-1">programmatic only</span>
      <button aria-hidden="true">hidden</button>
      <div inert><button>behind inert</button></div>
    `;
    document.body.append(root);
    expect(focusableIn(root).map((el) => el.textContent?.trim())).toEqual([
      "ok",
      "link",
    ]);
    expect(focusableIn(null)).toEqual([]);
    expect(FOCUSABLE_SELECTOR).toContain("button:not([disabled])");
  });

  it("wraps Tab at both ends of the trap", () => {
    const root = document.createElement("div");
    const first = document.createElement("button");
    const middle = document.createElement("button");
    const last = document.createElement("button");
    root.append(first, middle, last);
    document.body.append(root);

    first.focus();
    const back = trapEvent(true);
    trapTabKey(back.event, root);
    expect(document.activeElement).toBe(last);
    expect(back.prevented).toBe(true);

    last.focus();
    const forward = trapEvent(false);
    trapTabKey(forward.event, root);
    expect(document.activeElement).toBe(first);
    expect(forward.prevented).toBe(true);

    // Interior stops are left to the browser.
    middle.focus();
    const interior = trapEvent(false);
    trapTabKey(interior.event, root);
    expect(document.activeElement).toBe(middle);
    expect(interior.prevented).toBe(false);
  });

  it("pulls focus back when it has escaped the trap entirely", () => {
    const outside = document.createElement("button");
    const root = document.createElement("div");
    const inside = document.createElement("button");
    root.append(inside);
    document.body.append(outside, root);

    outside.focus();
    trapTabKey(trapEvent(false).event, root);
    expect(document.activeElement).toBe(inside);
  });

  it("makes siblings inert and restores their exact prior state", () => {
    const shell = document.createElement("div");
    const landmark = document.createElement("main");
    const alreadyInert = document.createElement("aside");
    alreadyInert.setAttribute("inert", "");
    alreadyInert.setAttribute("aria-hidden", "true");
    const exempt = document.createElement("div");
    exempt.dataset.modalLayer = "consent";
    const layer = document.createElement("div");
    shell.append(landmark, alreadyInert, exempt, layer);
    document.body.append(shell);

    const restore = inertSiblings(layer, ["consent"]);
    expect(landmark.hasAttribute("inert")).toBe(true);
    expect(landmark.getAttribute("aria-hidden")).toBe("true");
    expect(exempt.hasAttribute("inert")).toBe(false);
    expect(layer.hasAttribute("inert")).toBe(false);

    restore();
    expect(landmark.hasAttribute("inert")).toBe(false);
    expect(landmark.hasAttribute("aria-hidden")).toBe(false);
    // A sibling that was already inert stays inert.
    expect(alreadyInert.hasAttribute("inert")).toBe(true);
    expect(alreadyInert.getAttribute("aria-hidden")).toBe("true");
  });

  it("gives every landmark exactly one inert owner", () => {
    // Consent and Help inert their own background from inside the component.
    // The shell must not also write `inert` on the same landmarks.
    expect(isChromeLocked({ permissionOpen: true })).toBe(true);
    expect(isShellInert({ permissionOpen: true })).toBe(false);
    expect(isChromeLocked({ helpOpen: true })).toBe(true);
    expect(isShellInert({ helpOpen: true })).toBe(false);
    // Overlays with no self-treatment still get it from the shell.
    for (const flag of [
      "settingsOpen",
      "sessionBrowserOpen",
      "searchOpen",
      "aboutOpen",
      "mcpTrustOpen",
    ] as const) {
      expect(isShellInert({ [flag]: true }), flag).toBe(true);
    }
    expect(isShellInert({})).toBe(false);
  });

  it("would strand the shell inert if two owners wrote the same attribute", () => {
    // The regression `isShellInert` exists to prevent, reproduced directly: a
    // prop-driven owner sets `inert`, the helper records that as prior state,
    // the prop-driven owner removes it, and the helper puts it back forever.
    const shell = document.createElement("div");
    const landmark = document.createElement("main");
    const layer = document.createElement("div");
    shell.append(landmark, layer);
    document.body.append(shell);

    landmark.setAttribute("inert", ""); // owner A (the React prop)
    const restore = inertSiblings(layer); // owner B records inert: true
    landmark.removeAttribute("inert"); // owner A closes and cleans up
    restore(); // owner B restores what it recorded

    expect(landmark.hasAttribute("inert")).toBe(true);
    // Which is exactly why App feeds `isShellInert`, not `isChromeLocked`,
    // into the landmark props.
  });

  it("is a no-op for a detached layer", () => {
    expect(() => inertSiblings(null)()).not.toThrow();
    expect(() => inertSiblings(document.createElement("div"))()).not.toThrow();
  });
});

describe("global workspace shortcuts under an overlay", () => {
  const open = { chromeLocked: true };
  const closed = { chromeLocked: false };
  const cases: Array<[string, Record<string, unknown>, string]> = [
    ["⌘1 switch session", { key: "1", metaKey: true }, "focus-dock"],
    ["⌘6 switch session", { key: "6", metaKey: true }, "focus-dock"],
    ["⌘\\ open a second dock", { key: "\\", metaKey: true }, "open-beside"],
    ["⌘B sidebar", { key: "b", metaKey: true }, "toggle-sidebar"],
    ["⌘⌥B rightbar", { key: "b", metaKey: true, altKey: true }, "toggle-rightbar"],
    ["⌘⇧L live", { key: "l", metaKey: true, shiftKey: true }, "toggle-live"],
    [
      "⌘⌥← cycle docks",
      { key: "ArrowLeft", metaKey: true, altKey: true },
      "cycle-dock",
    ],
    [
      "⌘⌥→ cycle docks",
      { key: "ArrowRight", metaKey: true, altKey: true },
      "cycle-dock",
    ],
    ["⌃1 on Windows/Linux", { key: "1", ctrlKey: true }, "focus-dock"],
  ];

  it("resolves the full shortcut set while no overlay is open", () => {
    for (const [label, event, kind] of cases) {
      expect(
        resolveWorkspaceShortcut(event as never, closed)?.kind,
        label,
      ).toBe(kind);
    }
  });

  it("resolves nothing at all while a permission prompt is pending", () => {
    for (const [label, event] of cases) {
      expect(resolveWorkspaceShortcut(event as never, open), label).toBeNull();
    }
  });

  it("ignores unmodified and unmapped keys", () => {
    expect(resolveWorkspaceShortcut({ key: "1" }, closed)).toBeNull();
    expect(resolveWorkspaceShortcut({ key: "7", metaKey: true }, closed)).toBeNull();
    expect(
      resolveWorkspaceShortcut({ key: "ArrowLeft", metaKey: true }, closed),
    ).toBeNull();
  });

  it("keeps the dock index and cycle direction it resolved", () => {
    expect(
      resolveWorkspaceShortcut({ key: "3", metaKey: true }, closed),
    ).toEqual({ kind: "focus-dock", index: 2 });
    expect(
      resolveWorkspaceShortcut(
        { key: "ArrowRight", metaKey: true, altKey: true },
        closed,
      ),
    ).toEqual({ kind: "cycle-dock", delta: 1 });
  });
});

describe("App chrome wiring", () => {
  const app = readFileSync(join(here, "..", "App.tsx"), "utf8");

  it("feeds every overlay into one lock and gates the key handler on it", () => {
    expect(app).toContain("const chromeLocked = isChromeLocked(overlayFlags);");
    for (const flag of [
      "settingsOpen,",
      "sessionBrowserOpen,",
      "permissionOpen: Boolean(permission),",
      "searchOpen,",
      "aboutOpen,",
      "mcpTrustOpen: Boolean(mcpTrustPrompt),",
      "helpOpen,",
    ]) {
      expect(app, flag).toContain(flag);
    }
    expect(app).toContain("resolveWorkspaceShortcut(e, { chromeLocked })");
    // The handler must own no shortcut decision of its own any more.
    expect(app).not.toMatch(/const meta = e\.metaKey \|\| e\.ctrlKey;/);
  });

  it("marks all five operator landmarks inert while an overlay is open", () => {
    expect(app).toMatch(/<header className="titlebar" \{\.\.\.lockChrome\}/);
    expect(app).toMatch(/<footer className="status-bar" \{\.\.\.lockChrome\}/);
    expect(app.match(/\{\.\.\.lockChrome\}/g)?.length).toBe(5);
    // The shell inerts only the overlays that do not inert themselves.
    expect(app).toContain("const lockChrome = inertProps(isShellInert(overlayFlags));");
    expect(app).not.toContain("inertProps(chromeLocked)");
  });
});
