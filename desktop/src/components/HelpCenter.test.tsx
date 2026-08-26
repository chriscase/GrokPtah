/**
 * Help Center surface gates.
 *
 * Two things are being held down here. First, accessibility: a dialog that
 * traps focus, returns it, announces status, and is reachable by keyboard —
 * because Help is the surface a reader reaches when something has already gone
 * wrong for them. Second, the boundary: this component must reach the host
 * only through the three Help commands, and must never mint a chat session.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

/** Every Tauri call this component makes, recorded. */
const invoked: Array<{ command: string; args: unknown }> = [];
let nextProjection: unknown = null;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args: unknown) => {
    invoked.push({ command, args });
    if (command === "help_ask" || command === "help_follow" || command === "help_cancel") {
      return Promise.resolve(
        nextProjection ?? {
          handle: "help-00000001",
          status: "abstained",
          claims: [],
          error: null,
          message: null,
        },
      );
    }
    return Promise.resolve(null);
  },
}));

const { HelpCenter } = await import("./HelpCenter");

function open() {
  return render(<HelpCenter open onClose={() => {}} sessionToken="tok" />);
}

beforeEach(() => {
  invoked.length = 0;
  nextProjection = null;
});

// This suite does not run with vitest globals, so React Testing Library never
// registers its own afterEach. Without this, every render stays in the
// document and the next query finds several of everything.
afterEach(cleanup);

describe("Help Center accessibility", () => {
  it("is a labelled modal dialog", () => {
    open();
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(screen.getByRole("heading", { name: "Help Center" })).toBeInTheDocument();
  });

  it("moves focus to the search field when it opens", async () => {
    open();
    await waitFor(() => expect(screen.getByLabelText("Search Help")).toHaveFocus());
  });

  it("returns focus to whatever opened it", async () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    const { unmount } = render(<HelpCenter open onClose={() => {}} sessionToken="tok" />);
    await waitFor(() => expect(screen.getByLabelText("Search Help")).toHaveFocus());
    unmount();
    await waitFor(() => expect(opener).toHaveFocus());
    opener.remove();
  });

  it("closes on Escape", async () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} sessionToken="tok" />);
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("keeps Tab inside the dialog", async () => {
    const user = userEvent.setup();
    open();
    await waitFor(() => expect(screen.getByLabelText("Search Help")).toHaveFocus());
    const dialog = screen.getByRole("dialog");
    for (let step = 0; step < 12; step += 1) {
      await user.tab();
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
  });

  it("announces result counts in a live region", async () => {
    const user = userEvent.setup();
    open();
    const status = screen.getAllByRole("status")[0];
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent(/Browsing \d+ Help articles?/);
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await waitFor(() => expect(status).toHaveTextContent(/\d+ Help articles? match/));
  });

  it("says so plainly when nothing matches", async () => {
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "capital of Portugal");
    await waitFor(() =>
      expect(screen.getAllByRole("status")[0]).toHaveTextContent(/No Help article matches/),
    );
  });

  it("names its landmarks", () => {
    open();
    expect(screen.getByRole("navigation", { name: "Help articles" })).toBeInTheDocument();
    expect(screen.getByLabelText("Help article")).toBeInTheDocument();
    expect(screen.getByLabelText("Written answer")).toBeInTheDocument();
  });
});

describe("Help Center boundary", () => {
  it("renders offline results without calling the host at all", async () => {
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await waitFor(() => expect(screen.getAllByRole("status")[0]).toHaveTextContent(/match/));
    expect(
      invoked.filter((call) => call.command !== "help_session"),
      "search reached the host; offline retrieval must be local",
    ).toEqual([]);
  });

  it("never creates a chat session", async () => {
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() => expect(invoked.length).toBeGreaterThan(0));
    for (const call of invoked) {
      expect(call.command).not.toBe("session_new_kind");
      expect(call.command).not.toBe("session_new");
      expect(call.command).not.toBe("session_prompt");
      expect(call.command).not.toBe("session_delete");
    }
  });

  it("sends only a question, a locale, and the opaque session handle", async () => {
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() => expect(invoked.some((c) => c.command === "help_ask")).toBe(true));
    const ask = invoked.find((call) => call.command === "help_ask");
    expect(ask).toBeDefined();
    const payload = (ask!.args as { ask: Record<string, unknown> }).ask;
    expect(Object.keys(payload).sort()).toEqual(["locale", "question", "session"]);
    expect(payload.session).toBe("tok");
    // Nothing that could name a route, a chunk, or an authority.
    const serialized = JSON.stringify(payload);
    for (const forbidden of ["route", "model", "chunk", "grant", "capability", "tenant"]) {
      expect(serialized).not.toContain(forbidden);
    }
  });

  it("shows an abstention as an abstention, keeping the search results", async () => {
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() =>
      expect(screen.getByText(/could not support an answer/)).toBeInTheDocument(),
    );
    // The offline results are still on screen: a model declining does not
    // leave the reader with nothing.
    expect(screen.getAllByRole("status")[0]).toHaveTextContent(/match/);
  });

  it("re-checks a returned answer and drops an unsupported claim", async () => {
    nextProjection = {
      handle: "help-00000001",
      status: "answered",
      claims: [
        {
          ordinal: 0,
          text: "GrokPtah approves every computer action for you.",
          citations: [
            {
              source_id: "product.readme.features",
              path: "README.md",
              heading: "Features (desktop)",
              quote: "this text is not in the corpus at all",
            },
          ],
        },
      ],
      error: null,
      message: null,
    };
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() =>
      expect(screen.getByText(/could not support an answer/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/approves every computer action/)).not.toBeInTheDocument();
  });

  it("renders a supported claim with its exact quote", async () => {
    const { HELP_CORPUS } = await import("../lib/help/canonical/corpus");
    const chunk = HELP_CORPUS.chunks.find(
      (candidate) => candidate.kind === "body" && candidate.text.length > 80,
    )!;
    const source = HELP_CORPUS.sources.find((s) => s.id === chunk.source_ids[0])!;
    // Trimmed: getByText normalises whitespace, so a quote ending in a space
    // can never match the rendered node exactly.
    const quote = chunk.text.slice(0, 60).trim();
    nextProjection = {
      handle: "help-00000001",
      status: "answered",
      claims: [
        {
          ordinal: 0,
          text: "A supported statement.",
          citations: [
            { source_id: source.id, path: source.path, heading: source.heading, quote },
          ],
        },
      ],
      error: null,
      message: null,
    };
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() =>
      expect(screen.getByText("A supported statement.")).toBeInTheDocument(),
    );
    expect(screen.getByText((content) => content.trim() === quote)).toBeInTheDocument();
    expect(screen.getByLabelText("Sources for this statement")).toBeInTheDocument();
  });

  it("reports an unavailable answer with the host's fixed message only", async () => {
    nextProjection = {
      handle: "help-00000001",
      status: "unavailable",
      claims: [],
      error: "not_available",
      message: "Help cannot answer that right now.",
    };
    const user = userEvent.setup();
    open();
    await user.type(screen.getByLabelText("Search Help"), "recover an interrupted run");
    await user.click(screen.getByRole("button", { name: /Ask for a written answer/ }));
    await waitFor(() =>
      expect(screen.getByText("Help cannot answer that right now.")).toBeInTheDocument(),
    );
    // No reason, no code, nothing that distinguishes one refusal from another.
    for (const leak of ["revoked", "expired", "capability", "tenant", "stale"]) {
      expect(screen.queryByText(new RegExp(leak, "i"))).not.toBeInTheDocument();
    }
  });
});
