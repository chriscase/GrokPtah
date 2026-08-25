import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HelpCenter } from "./HelpCenter";
import { PermissionModal } from "./PermissionModal";
import { focusableIn } from "../lib/modalFocus";

afterEach(cleanup);

describe("HelpCenter", () => {
  it("renders the offline corpus with an accessible dialog and article", () => {
    render(<HelpCenter open onClose={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Help Center" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Search help" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions, builds, and chats" })).toBeInTheDocument();
    expect(screen.getByText(/Product corpus v1/)).toBeInTheDocument();
    expect(screen.getByText(/^\d+ articles$/)).not.toHaveAttribute("aria-live");
  });

  it("filters articles deterministically and exposes the selected article", () => {
    render(<HelpCenter open onClose={vi.fn()} />);
    const input = screen.getByRole("textbox", { name: "Search help" });

    fireEvent.change(input, { target: { value: "provider route" } });

    expect(screen.getByRole("heading", { name: "Provider routes and gateway policy" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Sessions, builds, and chats" })).not.toBeInTheDocument();
    expect(screen.getByText(/Source-backed offline guidance/)).toBeInTheDocument();
    expect(screen.getByText(/Heuristic match confidence:/)).toHaveTextContent(/ranking signal only, not certification/);
  });

  it("supports roving arrow-key navigation through article results", () => {
    render(<HelpCenter open onClose={vi.fn()} />);
    const options = within(screen.getByRole("listbox", { name: "Help article results" })).getAllByRole("option");

    expect(options[0]).toHaveAttribute("tabindex", "0");
    expect(options[1]).toHaveAttribute("tabindex", "-1");
    options[0].focus();
    fireEvent.keyDown(options[0], { key: "ArrowDown" });

    expect(document.activeElement).toBe(options[1]);
    expect(options[1]).toHaveAttribute("aria-selected", "true");
    expect(options[0]).toHaveAttribute("tabindex", "-1");

    fireEvent.keyDown(options[1], { key: "End" });
    expect(document.activeElement).toBe(options[options.length - 1]);
    fireEvent.keyDown(options[options.length - 1], { key: "Home" });
    expect(document.activeElement).toBe(options[0]);
    expect(options[0]).toHaveAttribute("aria-selected", "true");
    expect(options[0]).toHaveAttribute("tabindex", "0");
  });

  it("keeps the Tab trap on the selected option and does not re-enter tabindex=-1 results", () => {
    render(<HelpCenter open onClose={vi.fn()} />);
    const dialog = screen.getByRole("dialog", { name: "Help Center" });
    const listbox = screen.getByRole("listbox", { name: "Help article results" });
    const options = within(listbox).getAllByRole("option");
    const selected = options.find((option) => option.getAttribute("aria-selected") === "true");
    const hidden = options.filter((option) => option.getAttribute("tabindex") === "-1");
    const tabStops = focusableIn(dialog);

    expect(selected).toBeTruthy();
    expect(hidden.length).toBeGreaterThan(0);
    expect(tabStops).toContain(selected);
    expect(tabStops.filter((stop) => options.includes(stop))).toEqual([selected]);
    for (const option of hidden) {
      expect(tabStops).not.toContain(option);
    }

    const close = screen.getByRole("button", { name: "Close Help Center" });
    const last = tabStops[tabStops.length - 1];
    last.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(close);
    expect(hidden).not.toContain(document.activeElement);

    close.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
    expect(hidden).not.toContain(document.activeElement);
    if (options.includes(document.activeElement as HTMLElement)) {
      expect(document.activeElement).toHaveAttribute("aria-selected", "true");
      expect(document.activeElement).toHaveAttribute("tabindex", "0");
    }
  });

  it("gives listbox options presentation wrappers without changing search selection", () => {
    render(<HelpCenter open onClose={vi.fn()} />);
    const listbox = screen.getByRole("listbox", { name: "Help article results" });
    const wrappers = Array.from(listbox.children);

    expect(wrappers.length).toBeGreaterThan(1);
    for (const wrapper of wrappers) {
      expect(wrapper).toHaveAttribute("role", "presentation");
    }

    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "provider route" },
    });
    const filtered = screen.getByRole("listbox", { name: "Help article results" });
    expect(within(filtered).getByRole("option", { name: /Provider routes and gateway policy/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("heading", { name: "Provider routes and gateway policy" })).toBeInTheDocument();
    expect(Array.from(filtered.children).every((child) => child.getAttribute("role") === "presentation")).toBe(true);
  });

  it("closes on Escape without changing the source corpus", () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} />);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalledOnce();
  });

  it("cancels a nested confirmation before closing the Help Center", () => {
    const onClose = vi.fn();
    const onAskAssistant = vi.fn().mockResolvedValue({
      text: "A bounded answer.",
      citations: ["product.readme"],
      uncertainty: "The selected article is the authority.",
    });
    render(<HelpCenter open onClose={onClose} onAskAssistant={onAskAssistant} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    expect(screen.getByRole("alertdialog", { name: "Confirm assistant request" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog", { name: "Confirm assistant request" })).not.toBeInTheDocument();
  });

  it("keeps keyboard focus inside the modal and restores the opener", () => {
    const onClose = vi.fn();
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open Help";
    document.body.appendChild(opener);
    opener.focus();

    const { unmount } = render(<HelpCenter open onClose={onClose} />);
    const dialog = screen.getByRole("dialog", { name: "Help Center" });
    const close = screen.getByRole("button", { name: "Close Help Center" });
    const last = focusableIn(dialog)[focusableIn(dialog).length - 1];

    expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Search help" }));
    last.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(close);

    close.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);

    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it("makes the application background inert while Help is open", () => {
    const onClose = vi.fn();
    const { rerender } = render(
      <div className="app-shell">
        <main data-testid="app-background">Active coding lane</main>
        <HelpCenter open onClose={onClose} />
      </div>,
    );

    const background = screen.getByTestId("app-background");
    expect(background).toHaveAttribute("inert");
    expect(background).toHaveAttribute("aria-hidden", "true");

    rerender(
      <div className="app-shell">
        <main data-testid="app-background">Active coding lane</main>
        <HelpCenter open={false} onClose={onClose} />
      </div>,
    );
    expect(background).not.toHaveAttribute("inert");
    expect(background).not.toHaveAttribute("aria-hidden");
  });

  it("renders an honest empty state for an unknown query", () => {
    render(<HelpCenter open onClose={vi.fn()} />);
    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "teleport my repository" },
    });

    expect(screen.getByRole("heading", { name: "No matching guidance" })).toBeInTheDocument();
  });

  it("requires confirmation before calling the optional assistant and validates citations", async () => {
    const onAskAssistant = vi.fn().mockResolvedValue({
      text: "Builds and chats are separate surfaces.",
      citations: ["product.readme"],
      uncertainty: "This answer is limited to the selected article.",
    });
    render(<HelpCenter open onClose={vi.fn()} onAskAssistant={onAskAssistant} assistantProviderLabel="Company gateway · review-model" />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    expect(onAskAssistant).not.toHaveBeenCalled();
    const assistantConfirm = screen.getByRole("alertdialog", { name: "Confirm assistant request" });
    expect(assistantConfirm).toBeInTheDocument();
    expect(screen.getByText(/Company gateway · review-model/)).toBeInTheDocument();
    expect(within(assistantConfirm).getByText(/product\.readme/)).toBeInTheDocument();
    expect(within(assistantConfirm).getByText(/README\.md/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));
    await waitFor(() => expect(onAskAssistant).toHaveBeenCalledOnce());
    expect(screen.getByText(/Draft answer — not product truth/)).toBeInTheDocument();
  });

  it("falls back to cited guidance when the assistant answer is ungrounded", async () => {
    const onAskAssistant = vi.fn().mockResolvedValue({
      text: "It is fully certified.",
      citations: ["unknown-source"],
      uncertainty: "",
    });
    render(<HelpCenter open onClose={vi.fn()} onAskAssistant={onAskAssistant} />);
    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/answer rejected/));
    expect(screen.getByText(/Source-backed offline guidance/)).toBeInTheDocument();
  });

  it("requires confirmation before provider semantic ranking and preserves corpus bounds", async () => {
    const onSearchSemantic = vi.fn().mockResolvedValue({
      results: [{ articleId: "providers.gateway", score: 0.88, rationale: "Gateway policy match." }],
      uncertainty: "Provider ranking is not product certification.",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        onSearchSemantic={onSearchSemantic}
        assistantProviderLabel="Company gateway · review-model"
      />,
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "why is the company gateway model weak?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    expect(onSearchSemantic).not.toHaveBeenCalled();
    const semanticConfirm = screen.getByRole("alertdialog", { name: "Confirm meaning search" });
    expect(semanticConfirm).toBeInTheDocument();
    expect(within(semanticConfirm).getByText(/providers\.gateway/)).toBeInTheDocument();
    expect(within(semanticConfirm).getAllByText(/docs\/PROVIDER_PROFILES\.md/).length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole("button", { name: "Search by meaning" }));
    await waitFor(() => expect(onSearchSemantic).toHaveBeenCalledOnce());
    expect(screen.getByRole("heading", { name: "Provider routes and gateway policy" })).toBeInTheDocument();
    expect(screen.getByText(/Provider semantic ranking/)).toBeInTheDocument();
    expect(screen.getByText(/Provider ranking score: 88%/)).toBeInTheDocument();
  });

  it("keeps lexical guidance when semantic ranking is rejected", async () => {
    const onSearchSemantic = vi.fn().mockResolvedValue({
      results: [{ articleId: "providers.gateway", score: 2, rationale: "out of bounds" }],
      uncertainty: "bounded",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        onSearchSemantic={onSearchSemantic}
        assistantProviderLabel="Company gateway · review-model"
      />,
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "why is the company gateway model weak?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    fireEvent.click(screen.getByRole("button", { name: "Search by meaning" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/ranking rejected/));
    expect(screen.getByRole("heading", { name: "Review code through a restricted company gateway" })).toBeInTheDocument();
  });

  it("traps Tab inside the top confirmation and restores the layer opener, not the Help opener", () => {
    const onClose = vi.fn();
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open Help";
    document.body.appendChild(opener);
    opener.focus();
    const focusSpy = vi.spyOn(opener, "focus");

    const { unmount } = render(
      <HelpCenter open onClose={onClose} onAskAssistant={vi.fn()} />,
    );
    const prepare = screen.getByRole("button", { name: "Prepare cited question" });
    focusSpy.mockClear();
    fireEvent.click(prepare);

    const alert = screen.getByRole("alertdialog", { name: "Confirm assistant request" });
    const primary = screen.getByRole("button", { name: "Send cited context" });
    const cancel = screen.getByRole("button", { name: "Cancel" });
    const summary = within(alert).getByText("Review exact cited sources");
    const confirmStops = focusableIn(alert);
    expect(document.activeElement).toBe(primary);
    expect(focusSpy).not.toHaveBeenCalled();
    expect(confirmStops[0]).toBe(summary);
    expect(confirmStops[confirmStops.length - 1]).toBe(cancel);
    expect(confirmStops).toContain(primary);

    cancel.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(summary);
    expect(document.activeElement).not.toBe(
      screen.getByRole("button", { name: "Close Help Center", hidden: true }),
    );

    summary.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);
    expect(alert.contains(document.activeElement)).toBe(true);

    const help = document.querySelector<HTMLElement>('[data-modal-layer="help"]');
    expect(help).toHaveAttribute("aria-modal", "false");
    expect(help).not.toHaveAttribute("aria-hidden", "true");
    expect(alert).toHaveAttribute("aria-modal", "true");
    expect(alert.getAttribute("aria-describedby")?.split(/\s+/)).toEqual([
      "help-assistant-confirm-copy",
      "help-assistant-confirm-disclosure",
    ]);
    expect(document.getElementById("help-assistant-confirm-disclosure")).toHaveTextContent("product.readme");
    expect(document.getElementById("help-assistant-confirm-disclosure")).toHaveTextContent("README.md");

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(document.activeElement).toBe(prepare);
    expect(focusSpy).not.toHaveBeenCalled();

    unmount();
    expect(focusSpy).toHaveBeenCalled();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it("closes stacked confirmations one layer at a time", () => {
    const onClose = vi.fn();
    render(
      <HelpCenter
        open
        onClose={onClose}
        onAskAssistant={vi.fn()}
        onSearchSemantic={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "gateway" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    expect(screen.getByRole("alertdialog", { name: "Confirm meaning search" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question", hidden: true }));
    expect(screen.getByRole("alertdialog", { name: "Confirm assistant request" })).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog", { name: "Confirm meaning search" })).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog", { name: "Confirm assistant request" })).not.toBeInTheDocument();
    expect(screen.getByRole("alertdialog", { name: "Confirm meaning search" })).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("does not make consent-layer siblings inert while Help is open", () => {
    render(
      <div className="app-shell">
        <main data-testid="app-background">Active coding lane</main>
        <div data-modal-layer="consent" data-testid="consent-layer">
          Allow this tool?
        </div>
        <HelpCenter open onClose={vi.fn()} />
      </div>,
    );

    expect(screen.getByTestId("app-background")).toHaveAttribute("inert");
    expect(screen.getByTestId("consent-layer")).not.toHaveAttribute("inert");
    expect(screen.getByTestId("consent-layer")).not.toHaveAttribute("aria-hidden", "true");
  });

  it("yields Tab, Escape, and AT to a mounted permission consent layer", async () => {
    const onClose = vi.fn();
    const request = {
      id: "req-consent",
      session_id: "session-background-aaaa",
      tool_name: "run_terminal_cmd",
      summary: "Allow this tool?",
      detail: { session_id: "session-background-aaaa" },
    };
    const { rerender } = render(
      <div className="app-shell">
        <main data-testid="app-background">Active coding lane</main>
        <PermissionModal request={request} onRespond={vi.fn()} />
        <HelpCenter open onClose={onClose} onAskAssistant={vi.fn()} />
      </div>,
    );

    const help = await waitFor(() => {
      const dialog = document.querySelector<HTMLElement>('[data-modal-layer="help"]');
      expect(dialog).toHaveAttribute("inert");
      return dialog!;
    });
    expect(help).toHaveAttribute("aria-hidden", "true");
    expect(help).toHaveAttribute("aria-modal", "false");
    expect(screen.getByTestId("app-background")).toHaveAttribute("inert");

    const allow = screen.getByTestId("permission-allow");
    expect(document.activeElement).toBe(allow);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByTestId("permission-modal")).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Tab" });
    const technicalDetails = screen.getByText("Technical details");
    expect(document.activeElement).toBe(technicalDetails);
    expect(help.contains(document.activeElement)).toBe(false);

    technicalDetails.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(allow);
    expect(screen.getByTestId("permission-modal").contains(document.activeElement)).toBe(true);

    rerender(
      <div className="app-shell">
        <main data-testid="app-background">Active coding lane</main>
        <HelpCenter open onClose={onClose} onAskAssistant={vi.fn()} />
      </div>,
    );
    await waitFor(() => {
      expect(document.querySelector('[data-modal-layer="help"]')).not.toHaveAttribute("inert");
    });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("includes source-preview summaries in confirm Tab cycles and describes disclosed ids", () => {
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        onAskAssistant={vi.fn()}
        onSearchSemantic={vi.fn()}
        assistantProviderLabel="Company gateway · review-model"
      />,
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Search help" }), {
      target: { value: "why is the company gateway model weak?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));

    const semantic = screen.getByRole("alertdialog", { name: "Confirm meaning search" });
    const semanticSummary = within(semantic).getByText("Review exact metadata");
    const searchByMeaning = screen.getByRole("button", { name: "Search by meaning" });
    const cancel = within(semantic).getByRole("button", { name: "Cancel" });
    const semanticStops = focusableIn(semantic);
    expect(semanticStops[0]).toBe(semanticSummary);
    expect(semanticStops).toEqual([semanticSummary, searchByMeaning, cancel]);
    expect(document.querySelector('[data-modal-layer="help"]')).toHaveAttribute("aria-modal", "false");
    expect(semantic).toHaveAttribute("aria-modal", "true");
    expect(semantic.getAttribute("aria-describedby")?.split(/\s+/)).toEqual([
      "help-semantic-confirm-copy",
      "help-semantic-confirm-disclosure",
    ]);
    expect(document.getElementById("help-semantic-confirm-disclosure")).toHaveTextContent("providers.gateway");
    expect(document.getElementById("help-semantic-confirm-ids")).toHaveTextContent("docs/PROVIDER_PROFILES.md");

    cancel.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(semanticSummary);
    semanticSummary.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);
  });
});
