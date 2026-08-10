import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { StyledSelect } from "./StyledSelect";

const root = dirname(fileURLToPath(import.meta.url));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("StyledSelect (#126)", () => {
  it("Settings and composer no longer use native <select>", () => {
    const settings = readFileSync(join(root, "SettingsPanel.tsx"), "utf8");
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(settings).toMatch(/StyledSelect/);
    expect(settings).not.toMatch(/<select[\s>]/);
    // Composer model/effort pickers
    expect(app).toMatch(/className="composer-select"/);
    expect(app).not.toMatch(/composer-pill[\s\S]{0,200}<select/);
  });

  it("exports a listbox-based dropdown (not a native select element)", () => {
    const src = readFileSync(join(root, "StyledSelect.tsx"), "utf8");
    expect(src).toMatch(/role="listbox"/);
    expect(src).toMatch(/role="option"/);
    expect(src).not.toMatch(/<select[\s>]/);
  });

  it("positions menus against the viewport so bottom-toolbar pickers can flip up", () => {
    const src = readFileSync(join(root, "StyledSelect.tsx"), "utf8");
    expect(src).toMatch(/position: "fixed"/);
    expect(src).toMatch(/placement === "above"/);
    expect(src).toMatch(/addEventListener\("resize"/);
    expect(src).toMatch(/addEventListener\("scroll", onViewportChange, true\)/);
  });

  it("does not require bridge status before dismissing the launch splash", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/const splashReady = workspaceRestored;/);
  });

  it("keeps live thought tokens in a stable readable line box", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    expect(css).toMatch(/\.bubble\.thought \{[\s\S]*line-height: 1\.6;/);
    expect(css).toMatch(
      /\.bubble\.thought \.stream-text\.is-streaming \{[\s\S]*text-shadow: none;/,
    );
    const pane = readFileSync(join(root, "SessionPane.tsx"), "utf8");
    expect(pane).toMatch(/item\.kind === "thought"[\s\S]*animated=\{false\}/);
  });

  it("flips a bottom-anchored menu above its trigger", () => {
    vi.spyOn(HTMLButtonElement.prototype, "getBoundingClientRect").mockReturnValue({
      top: 700,
      bottom: 720,
      left: 900,
      right: 960,
      width: 60,
      height: 20,
      x: 900,
      y: 700,
      toJSON: () => ({}),
    } as DOMRect);

    render(
      createElement(StyledSelect, {
        "aria-label": "Effort",
        value: "medium",
        options: ["none", "low", "medium", "high", "max"].map((value) => ({
          value,
          label: value,
        })),
        onChange: vi.fn(),
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Effort" }));
    const menu = screen.getByRole("listbox");
    expect(menu).toHaveAttribute("data-placement", "above");
    expect(menu).toHaveStyle({ position: "fixed", bottom: "72px" });
  });

  it("supports keyboard navigation and returns focus after selection", () => {
    const onChange = vi.fn();
    render(
      createElement(StyledSelect, {
        "aria-label": "Effort",
        value: "medium",
        options: ["low", "medium", "high"].map((value) => ({
          value,
          label: value,
        })),
        onChange,
      }),
    );

    const trigger = screen.getByRole("button", { name: "Effort" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const current = screen.getByRole("option", { name: "medium" });
    fireEvent.keyDown(current, { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByRole("option", { name: "high" }), {
      key: "Enter",
    });

    expect(onChange).toHaveBeenCalledWith("high");
    expect(trigger).toHaveFocus();
  });

  it("does not open while disabled", () => {
    render(
      createElement(StyledSelect, {
        "aria-label": "Model",
        value: "grok-build",
        options: [{ value: "grok-build", label: "Grok Build" }],
        disabled: true,
        onChange: vi.fn(),
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Model" }));
    expect(screen.queryByRole("listbox")).toBeNull();
  });
});
