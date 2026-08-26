import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useEffect, useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HelpRoute, useHelpPaletteShortcut } from "./react/HelpRoute";

afterEach(cleanup);

function Background() {
  return (
    <div id="root">
      <button type="button">behind the overlay</button>
    </div>
  );
}

describe("Help route", () => {
  it("renders as a labelled modal dialog", () => {
    render(<HelpRoute open onClose={vi.fn()} />);
    const dialog = screen.getByRole("dialog", { name: "Help" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(screen.getByLabelText("Search Help")).toBeInTheDocument();
  });

  it("renders nothing when closed", () => {
    render(<HelpRoute open={false} onClose={vi.fn()} />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("makes the background inert, not merely aria-hidden", () => {
    // aria-hidden alone leaves the background tabbable; `inert` is what
    // actually stops Tab and assistive-technology traversal.
    const { unmount } = render(
      <>
        <Background />
        <HelpRoute open onClose={vi.fn()} />
      </>,
    );
    const background = document.getElementById("root")!;
    expect(background).toHaveAttribute("aria-hidden", "true");
    expect(background.hasAttribute("inert")).toBe(true);
    unmount();
    expect(document.getElementById("root")).toBeNull();
  });

  it("releases the background when it closes", () => {
    const { rerender } = render(
      <>
        <Background />
        <HelpRoute open onClose={vi.fn()} />
      </>,
    );
    expect(document.getElementById("root")!.hasAttribute("inert")).toBe(true);
    rerender(
      <>
        <Background />
        <HelpRoute open={false} onClose={vi.fn()} />
      </>,
    );
    const background = document.getElementById("root")!;
    expect(background.hasAttribute("inert")).toBe(false);
    expect(background.hasAttribute("aria-hidden")).toBe(false);
  });

  it("does not inert itself when it renders inside the app root", () => {
    // The arrangement every React app actually has, and the one the previous
    // `document.getElementById("root")` default got wrong: the route renders
    // *inside* `#root`, so inerting `#root` inerted the palette. The surface
    // built for keyboard and screen-reader users became the one surface
    // neither could reach.
    const root = document.createElement("div");
    root.id = "root";
    document.body.append(root);

    render(
      <>
        <nav data-testid="chrome">app chrome</nav>
        <HelpRoute open onClose={vi.fn()} />
      </>,
      { container: root },
    );

    const dialog = screen.getByRole("dialog");
    expect(root.hasAttribute("inert")).toBe(false);
    expect(root.hasAttribute("aria-hidden")).toBe(false);
    // Nothing between the dialog and the body is inert.
    for (let node = dialog.parentElement; node; node = node.parentElement) {
      expect(node.hasAttribute("inert"), node.tagName).toBe(false);
    }
    // The dialog itself is reachable.
    expect(dialog.closest("[inert]")).toBeNull();
    // What is beside it is not.
    const chrome = screen.getByTestId("chrome");
    expect(chrome.hasAttribute("inert")).toBe(true);
    expect(chrome).toHaveAttribute("aria-hidden", "true");

    root.remove();
  });

  it("restores a sibling that was already hidden for its own reasons", () => {
    const root = document.createElement("div");
    document.body.append(root);

    const tree = (open: boolean) => (
      <>
        <div data-testid="already" aria-hidden="true" />
        <HelpRoute open={open} onClose={vi.fn()} />
      </>
    );
    const { rerender } = render(tree(true), { container: root });
    const already = screen.getByTestId("already");
    expect(already.hasAttribute("inert")).toBe(true);

    rerender(tree(false));
    // The palette added `inert`, so it removes `inert`. It did not add
    // `aria-hidden`, so it must not remove it.
    expect(already.hasAttribute("inert")).toBe(false);
    expect(already).toHaveAttribute("aria-hidden", "true");

    root.remove();
  });

  it("ignores a named background that would capture the dialog", () => {
    const root = document.createElement("div");
    document.body.append(root);

    // A caller naming an ancestor is asking for the palette to be inerted.
    // The route inerts the dialog's siblings instead of obeying.
    render(
      <>
        <div data-testid="beside" />
        <HelpRoute open onClose={vi.fn()} backgroundRef={{ current: root }} />
      </>,
      { container: root },
    );

    expect(root.hasAttribute("inert")).toBe(false);
    expect(screen.getByTestId("beside").hasAttribute("inert")).toBe(true);
    expect(screen.getByRole("dialog").closest("[inert]")).toBeNull();

    root.remove();
  });

  it("honors a named background that is genuinely beside the dialog", () => {
    // The escape hatch still works when it does not capture the palette.
    const named = { current: null as HTMLElement | null };
    const Named = () => {
      const ref = useRef<HTMLDivElement | null>(null);
      useEffect(() => {
        named.current = ref.current;
      }, []);
      return <div ref={ref} data-testid="named" />;
    };
    const { rerender } = render(
      <>
        <Named />
        <div data-testid="other" />
        <HelpRoute open={false} onClose={vi.fn()} backgroundRef={named} />
      </>,
    );
    rerender(
      <>
        <Named />
        <div data-testid="other" />
        <HelpRoute open onClose={vi.fn()} backgroundRef={named} />
      </>,
    );
    expect(screen.getByTestId("named").hasAttribute("inert")).toBe(true);
    // Only the named element; sibling inerting is the fallback, not an addition.
    expect(screen.getByTestId("other").hasAttribute("inert")).toBe(false);
  });

  it("restores focus to the opener on close", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    expect(document.activeElement).toBe(opener);

    const { rerender } = render(<HelpRoute open onClose={vi.fn()} />);
    rerender(<HelpRoute open={false} onClose={vi.fn()} />);
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it("closes on Escape and on a backdrop click", () => {
    const onClose = vi.fn();
    const { container } = render(<HelpRoute open onClose={onClose} />);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    const backdrop = container.querySelector('[data-help-surface="route"]')!;
    fireEvent.mouseDown(backdrop);
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("does not close when the click starts inside the dialog", () => {
    const onClose = vi.fn();
    render(<HelpRoute open onClose={onClose} />);
    fireEvent.mouseDown(screen.getByRole("dialog"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("contains Tab focus inside the dialog", () => {
    render(<HelpRoute open onClose={vi.fn()} />);
    const dialog = screen.getByRole("dialog");
    const focusable = dialog.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    expect(focusable.length).toBeGreaterThan(1);
    const first = focusable[0]!;
    const last = focusable[focusable.length - 1]!;

    last.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(first);

    first.focus();
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("states the offline case honestly rather than as an error", () => {
    render(<HelpRoute open onClose={vi.fn()} />);
    const note = screen.getByRole("note");
    expect(note.textContent).toMatch(/Answering is not available here/);
    expect(note.textContent).toMatch(/runs entirely offline/);
    expect(note.textContent).not.toMatch(/error|failed|unavailable/i);
  });

  it("names the provider when one is configured, and when it is not reachable", () => {
    const { rerender } = render(
      <HelpRoute open onClose={vi.fn()} answering={{ kind: "available", serviceLabel: "Company gateway" }} />,
    );
    expect(screen.getByRole("note").textContent).toMatch(/Company gateway/);
    expect(screen.getByRole("note").textContent).toMatch(/Search itself runs offline/);

    rerender(
      <HelpRoute
        open
        onClose={vi.fn()}
        answering={{ kind: "unavailable", serviceLabel: "Company gateway", detail: "quota exhausted" }}
      />,
    );
    // Honest about the provider without implying search is broken.
    expect(screen.getByRole("note").textContent).toMatch(/quota exhausted/);
    expect(screen.getByRole("note").textContent).toMatch(/Search still works offline/);
  });

  it("seeds a context-aware query and says where the search came from", () => {
    render(
      <HelpRoute open onClose={vi.fn()} context={{ label: "Provider settings", seedQuery: "gateway" }} />,
    );
    expect(screen.getByText(/Searching Help from Provider settings/)).toBeInTheDocument();
    const listbox = screen.getByRole("listbox", { name: "Help results" });
    expect(within(listbox).getAllByRole("option").length).toBeGreaterThan(0);
  });

  it("announces results through a single polite live region", () => {
    render(<HelpRoute open onClose={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Search Help"), { target: { value: "durable run recovery" } });
    const statuses = screen.getAllByRole("status");
    expect(statuses).toHaveLength(1);
    expect(statuses[0]).toHaveAttribute("aria-live", "polite");
    expect(statuses[0]!.textContent).toMatch(/Help results?\./);
  });

  it("shows which corpus produced the results", () => {
    const { container } = render(<HelpRoute open onClose={vi.fn()} />);
    const provenance = container.querySelector('[data-help-part="provenance"]')!;
    expect(provenance.textContent).toMatch(/Corpus sha256:/);
  });

  it("reports a redacted credential without echoing it", () => {
    const { container } = render(<HelpRoute open onClose={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Search Help"), {
      target: { value: "my key xai-AbCdEf0123456789AbCdEf on the gateway" },
    });
    expect(container.textContent).toMatch(/credential in your query was removed/);
    expect(container.textContent).not.toContain("AbCdEf");
  });

  it("carries no fixed pixel sizing, clipping, or nowrap that would break 400% zoom", () => {
    const { container } = render(
      <HelpRoute open onClose={vi.fn()} context={{ label: "Settings", seedQuery: "gateway" }} />,
    );
    for (const element of container.querySelectorAll<HTMLElement>("*")) {
      const style = element.getAttribute("style") ?? "";
      expect(style).not.toMatch(/(?:^|;)\s*(?:height|max-height|width|max-width)\s*:\s*\d+px/);
      expect(style).not.toMatch(/overflow\s*:\s*hidden/);
      expect(style).not.toMatch(/white-space\s*:\s*nowrap/);
      expect(style).not.toMatch(/position\s*:\s*fixed/);
    }
  });

  it("conveys state through semantics and text, never color alone", () => {
    const { container } = render(
      <HelpRoute open onClose={vi.fn()} context={{ label: "Settings", seedQuery: "computer use" }} />,
    );
    const option = within(screen.getByRole("listbox")).getAllByRole("option")[0]!;
    expect(option).toHaveAttribute("aria-selected", "true");
    expect(option).toHaveAttribute("data-topic");
    // No inline color or transition: forced-colors and reduced-motion users
    // lose nothing, because nothing was carried that way.
    for (const element of container.querySelectorAll<HTMLElement>("*")) {
      const style = element.getAttribute("style") ?? "";
      expect(style).not.toMatch(/(?:^|;)\s*(?:color|background|background-color)\s*:/);
      expect(style).not.toMatch(/transition|animation/);
    }
  });

  it("keeps the results list operable by keyboard alone", () => {
    const onActivate = vi.fn();
    render(
      <HelpRoute
        open
        onClose={vi.fn()}
        onActivate={onActivate}
        context={{ label: "Settings", seedQuery: "computer use" }}
      />,
    );
    const listbox = screen.getByRole("listbox", { name: "Help results" });
    fireEvent.keyDown(listbox, { key: "ArrowDown" });
    fireEvent.keyDown(listbox, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledTimes(1);
  });
});

describe("Help palette shortcut", () => {
  function Harness({ onOpen }: { onOpen: () => void }) {
    useHelpPaletteShortcut(onOpen);
    return <input aria-label="unrelated field" />;
  }

  it("opens on the modifier chord", () => {
    const onOpen = vi.fn();
    render(<Harness onOpen={onOpen} />);
    fireEvent.keyDown(document, { key: "/", ctrlKey: true });
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("stays out of the way while the user is typing", () => {
    const onOpen = vi.fn();
    render(<Harness onOpen={onOpen} />);
    const field = screen.getByLabelText("unrelated field");
    field.focus();
    fireEvent.keyDown(field, { key: "/", ctrlKey: true });
    expect(onOpen).not.toHaveBeenCalled();
  });
});
