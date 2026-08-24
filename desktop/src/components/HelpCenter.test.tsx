import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HelpCenter } from "./HelpCenter";

afterEach(cleanup);

describe("HelpCenter", () => {
  it("renders the offline corpus with an accessible dialog and article", () => {
    render(<HelpCenter open onClose={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Help Center" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Search help" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions, builds, and chats" })).toBeInTheDocument();
    expect(screen.getByText(/Product corpus v1/)).toBeInTheDocument();
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
    const focusables = dialog.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled])',
    );
    const last = focusables[focusables.length - 1];

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
    expect(screen.getByRole("alertdialog", { name: "Confirm assistant request" })).toBeInTheDocument();
    expect(screen.getByText(/Company gateway · review-model/)).toBeInTheDocument();

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
    expect(screen.getByRole("alertdialog", { name: "Confirm meaning search" })).toBeInTheDocument();
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
});
