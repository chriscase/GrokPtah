import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HelpCenter } from "./HelpCenter";
import { createHelpViewFixtureAuthority } from "../lib/helpCenterView.fixtures";

afterEach(cleanup);

/** The synthetic corpus: every state below is reachable by construction. */
const fixtureAuthority = createHelpViewFixtureAuthority();

function searchBox() {
  return screen.getByRole("combobox", { name: "Search help" });
}

function type(value: string) {
  fireEvent.change(searchBox(), { target: { value } });
}

/**
 * Titles of the result listbox, in order.
 *
 * Scoped to the listbox on purpose: the topic `<select>` also owns `option`
 * elements, and a bare role query would silently mix the two.
 */
function optionTitles() {
  const listbox = screen.getByRole("listbox");
  return within(listbox)
    .queryAllByRole("option")
    .map((option) => option.querySelector("strong")?.textContent ?? "");
}

describe("HelpCenter", () => {
  it("renders the offline corpus with an accessible dialog and article", () => {
    render(<HelpCenter open onClose={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Help Center" })).toBeInTheDocument();
    expect(searchBox()).toBeInTheDocument();
    expect(screen.getByRole("listbox", { name: "Help articles" })).toBeInTheDocument();
    expect(screen.getByText(/Product corpus v1/)).toBeInTheDocument();
    expect(screen.getByText(/Offline hybrid retrieval/)).toHaveTextContent(/no network, no model/);
  });

  it("filters articles deterministically and exposes the selected article", () => {
    render(<HelpCenter open onClose={vi.fn()} includeRestricted />);

    type("provider route policy");

    expect(
      screen.getByRole("heading", { name: "Provider routes and gateway policy" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Sessions, builds, and chats" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/Source-backed offline guidance/)).toBeInTheDocument();
    expect(screen.getByText(/Rank signal:/)).toHaveTextContent(
      /ranking signal only, not certification/,
    );
  });

  it("closes on Escape without changing the source corpus", () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} />);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalledOnce();
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

  it("keeps keyboard focus inside the modal and restores the opener", () => {
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open Help";
    document.body.appendChild(opener);
    opener.focus();

    const { unmount } = render(<HelpCenter open onClose={vi.fn()} />);
    const dialog = screen.getByRole("dialog", { name: "Help Center" });
    const close = screen.getByRole("button", { name: "Close Help Center" });
    const focusables = dialog.querySelectorAll<HTMLElement>(
      "button:not([disabled]), input:not([disabled]), select:not([disabled])",
    );
    const last = focusables[focusables.length - 1];

    expect(document.activeElement).toBe(searchBox());
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
});

describe("HelpCenter retrieval states", () => {
  const renderFixture = (props: Record<string, unknown> = {}) =>
    render(
      <HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} {...props} />,
    );

  it("presents a confident result as an answer, with verified citation spans", () => {
    renderFixture();
    type("lantern workspace");

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("Answer from the shipped documentation");
    expect(outcome).toHaveTextContent("answer");

    const citations = screen.getByRole("region", { name: "Cited answer spans" });
    expect(citations).toHaveTextContent("Why this article is the answer");
    const quotes = within(citations).getAllByRole("blockquote");
    expect(quotes.length).toBeGreaterThan(0);
    // Each quote names the documents backing that exact text, and says it was
    // re-checked rather than asking the reader to take it on trust.
    expect(citations).toHaveTextContent(/verified/);
    expect(citations).toHaveTextContent(/docs\/synthetic\/lantern-guide\.md/);
  });

  it("does not present an ambiguous result as an answer", () => {
    renderFixture();
    type("beacon");

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("More than one article fits");
    expect(outcome).toHaveTextContent("ambiguous");
    expect(outcome).toHaveTextContent(/none is being presented as the response/);

    // Both tied articles are offered, and both are labelled suggestions.
    expect(optionTitles()).toEqual(["Northern relay rotation", "Southern relay rotation"]);
    expect(screen.getAllByText("Suggestion")).toHaveLength(2);
    expect(screen.getByRole("note")).toHaveTextContent(/did not present this article as the answer/);
    // Quotes still explain why a candidate surfaced, but they are never framed
    // as the citations of an answer that was not given.
    expect(screen.queryByRole("region", { name: "Cited answer spans" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Match evidence" }))
      .toHaveTextContent("Why this article matched");
  });

  it("does not present a weak match as an answer", () => {
    renderFixture();
    type("cartography atlas");

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("No confident answer");
    expect(outcome).toHaveTextContent("low-confidence");
    expect(screen.getByRole("note")).toHaveTextContent(/did not present this article as the answer/);
  });

  it("says nothing matched rather than guessing", () => {
    renderFixture();
    type("zzzz qqqq");

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("No documented answer");
    expect(outcome).toHaveTextContent(/so Help is not guessing at one/);
    expect(optionTitles()).toHaveLength(0);
    expect(screen.getByRole("heading", { name: "No matching guidance" })).toBeInTheDocument();
  });

  it("reports a rejected query as a rejection, not as an abstention", () => {
    renderFixture();
    type("x".repeat(600));

    const outcome = screen.getByRole("alert", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("Question not searched");
    expect(outcome).toHaveTextContent("query-too-long");
    expect(outcome).not.toHaveTextContent("abstain");
  });

  it("browses the corpus before a question is asked", () => {
    renderFixture();

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("Browse the Help corpus");
    expect(screen.getByRole("listbox", { name: "Help articles" })).toBeInTheDocument();
    // Topic order first, so a reader lands on an introduction, not on whatever
    // sorts first by ID.
    expect(optionTitles()[0]).toBe("Set up the Lantern workspace");
  });

  it("keeps a restricted article out of results until the embedder asks for it", () => {
    const { unmount } = renderFixture();
    type("sealed vault");
    expect(
      screen.getByRole("status", { name: "Help retrieval outcome" }),
    ).toHaveTextContent("No documented answer");
    unmount();

    renderFixture({ includeRestricted: true });
    type("sealed vault");
    expect(
      screen.getByRole("heading", { name: "Promote a sealed vault review" }),
    ).toBeInTheDocument();
  });
});

describe("HelpCenter capability and permission labels", () => {
  it("labels access, audience, and documented capabilities without claiming a grant", () => {
    render(
      <HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} includeRestricted />,
    );
    type("sealed vault");

    const labels = screen.getByRole("region", { name: "Article access and capabilities" });
    expect(labels).toHaveTextContent("Operator only");
    expect(labels).toHaveTextContent(/does not confer the role/);
    expect(labels).toHaveTextContent("Power user, Operator");

    const capabilities = within(labels).getByRole("list", { name: "Documented capabilities" });
    expect(capabilities).toHaveTextContent("Promote a run");
    expect(capabilities).toHaveTextContent("run.promote");
    // The decisive assertion: a documented capability is never presented as an
    // available one.
    expect(capabilities).toHaveTextContent(/live: unknown/);
    expect(labels).toHaveTextContent(/Documented capability, not a live grant/);
  });

  it("says so plainly when an article documents no capability", () => {
    render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} />);
    type("lantern workspace");

    const labels = screen.getByRole("region", { name: "Article access and capabilities" });
    expect(labels).toHaveTextContent("Open to everyone");
    expect(within(labels).getByRole("list", { name: "Documented capabilities" }))
      .toHaveTextContent("Observe sessions");
  });
});

describe("HelpCenter keyboard navigation", () => {
  const activeOptionTitle = () => {
    const activeId = searchBox().getAttribute("aria-activedescendant");
    if (!activeId) return null;
    return document.getElementById(activeId)?.querySelector("strong")?.textContent ?? null;
  };

  it("moves the active option with the arrow keys without leaving the search field", () => {
    render(
      <HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} includeRestricted />,
    );
    const input = searchBox();

    expect(input).toHaveAttribute("aria-expanded", "true");
    expect(input).toHaveAttribute("aria-controls");
    expect(activeOptionTitle()).toBe("Set up the Lantern workspace");

    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(activeOptionTitle()).toBe("Promote a sealed vault review");
    // Focus never leaves the combobox: the active option is named, not focused.
    expect(document.activeElement).toBe(input);

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(activeOptionTitle()).toBe("Set up the Lantern workspace");

    fireEvent.keyDown(input, { key: "End" });
    expect(activeOptionTitle()).toBe("Southern relay rotation");

    fireEvent.keyDown(input, { key: "Home" });
    expect(activeOptionTitle()).toBe("Set up the Lantern workspace");
  });

  it("wraps at both ends so a keyboard user is never stuck", () => {
    render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} />);
    const input = searchBox();

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(activeOptionTitle()).toBe("Southern relay rotation");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(activeOptionTitle()).toBe("Set up the Lantern workspace");
  });

  it("opens the active option on Enter and marks it selected", () => {
    render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} />);
    const input = searchBox();

    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(screen.getByRole("heading", { name: "Northern relay rotation" }))
      .toBeInTheDocument();
    const selected = within(screen.getByRole("listbox"))
      .getAllByRole("option")
      .filter((option) => option.getAttribute("aria-selected") === "true");
    expect(selected).toHaveLength(1);
    expect(selected[0]).toHaveTextContent("Northern relay rotation");
  });

  it("leaves Escape to the dialog rather than trapping it in the search field", () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} authority={fixtureAuthority} />);

    fireEvent.keyDown(searchBox(), { key: "Escape" });
    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalled();
  });
});

describe("HelpCenter cited answer seam", () => {
  const renderWithAnswer = (
    onAnswer: (request: unknown, signal: AbortSignal) => Promise<string>,
    props: Record<string, unknown> = {},
  ) =>
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        authority={fixtureAuthority}
        assistantProviderLabel="Company gateway"
        onAnswer={onAnswer as never}
        {...props}
      />,
    );

  it("sends nothing before a confirmation, and shows what would be sent", () => {
    const onAnswer = vi.fn().mockResolvedValue("{}");
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));

    expect(onAnswer).not.toHaveBeenCalled();
    const confirm = screen.getByRole("alertdialog", { name: "Confirm cited answer request" });
    expect(confirm).toHaveTextContent("Company gateway");
    expect(confirm).toHaveTextContent(/grants no tools, stores nothing/);
    expect(within(confirm).getByRole("list", { name: "Articles in this request" }))
      .toHaveTextContent("synthetic.lantern-workspace");
    // Provider, model, and cost are declared unknown at the point of consent.
    expect(confirm).toHaveTextContent(/model unknown/);
    expect(confirm).toHaveTextContent(/cost unknown/);
  });

  it("shows the seam's own refusal instead of a button that does nothing", () => {
    const onAnswer = vi.fn();
    // Below the contract's minimum timeout, so the seam refuses to build a
    // request at all.
    renderWithAnswer(onAnswer, { answerTimeoutMs: 100 });
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));

    expect(onAnswer).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    const refusal = screen.getByRole("alert");
    expect(refusal).toHaveTextContent("invalid-timeout");
    expect(refusal).toHaveTextContent("Nothing was sent.");
  });

  it("does not offer the seam when Help abstained", () => {
    renderWithAnswer(vi.fn());
    type("beacon");

    expect(
      screen.queryByRole("button", { name: "Prepare cited answer request" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/not asked to cover for a retriever/)).toBeInTheDocument();
  });

  it("shows a cited reply as a draft and keeps the corpus authoritative", async () => {
    const onAnswer = vi.fn(async (request: { allowedSourceIds: readonly string[] }) =>
      JSON.stringify({
        outcome: "answered",
        text: "Panes do not share state.",
        citations: [request.allowedSourceIds[0]],
        uncertainty: "Drafted from the cited article only.",
      }));
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));

    await waitFor(() => expect(onAnswer).toHaveBeenCalledOnce());
    const result = await screen.findByText("Cited draft answer");
    expect(result.parentElement).toHaveTextContent("Panes do not share state.");
    expect(result.parentElement).toHaveTextContent(/cited documentation remains the authority/);
    expect(result.parentElement).toHaveTextContent(/model: unknown/);
    expect(result.parentElement).toHaveTextContent(/cost: unknown/);
  });

  it("never renders an uncited assertion as an answer", async () => {
    const onAnswer = vi.fn().mockResolvedValue(
      JSON.stringify({
        outcome: "answered",
        text: "You now have operator capability.",
        citations: [],
        uncertainty: "None.",
      }),
    );
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));

    await waitFor(() => expect(onAnswer).toHaveBeenCalledOnce());
    expect(await screen.findByText("Reply not shown")).toBeInTheDocument();
    expect(screen.queryByText("You now have operator capability.")).not.toBeInTheDocument();
  });

  it("turns prose that is not the envelope into a visible abstention", async () => {
    const onAnswer = vi.fn().mockResolvedValue("Sure! You are approved to promote.");
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));

    await waitFor(() => expect(onAnswer).toHaveBeenCalledOnce());
    expect(await screen.findByText("The model abstained")).toBeInTheDocument();
    expect(screen.queryByText(/You are approved to promote/)).not.toBeInTheDocument();
  });

  it("reports honest progress while a reply is outstanding", async () => {
    let release: (value: string) => void = () => {};
    const onAnswer = vi.fn(() => new Promise<string>((resolve) => { release = resolve; }));
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));

    await waitFor(() => expect(onAnswer).toHaveBeenCalledOnce());
    const pending = await screen.findByText(/nothing has been received yet/);
    expect(pending).toBeInTheDocument();
    // No fabricated progress: latency is unknown while the request is open.
    expect(screen.getByText(/latency: unknown/)).toBeInTheDocument();

    await act(async () => {
      release(JSON.stringify({
        outcome: "not_found", text: "", citations: [], uncertainty: "Not covered.",
      }));
    });
    expect(await screen.findByText(/found no answer in the cited articles/)).toBeInTheDocument();
  });

  it("abandons a request that outruns its declared budget and aborts the adapter", async () => {
    vi.useFakeTimers();
    try {
      let seenSignal: AbortSignal | null = null;
      const onAnswer = vi.fn((_request: unknown, signal: AbortSignal) => {
        seenSignal = signal;
        return new Promise<string>(() => {});
      });
      renderWithAnswer(onAnswer, { answerTimeoutMs: 5_000 });
      type("lantern workspace");

      fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
      fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));
      expect(onAnswer).toHaveBeenCalledOnce();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5_000);
      });

      expect(screen.getByText("No reply within the declared budget")).toBeInTheDocument();
      // The budget is stated because it was declared; no elapsed time is claimed.
      expect(screen.getByText(/declared a 5s budget/)).toBeInTheDocument();
      expect(screen.getByText(/Whether it was ever served is unknown/)).toBeInTheDocument();
      expect(seenSignal).not.toBeNull();
      expect((seenSignal as unknown as AbortSignal).aborted).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("lets the reader cancel an outstanding request", async () => {
    let seenSignal: AbortSignal | null = null;
    const onAnswer = vi.fn((_request: unknown, signal: AbortSignal) => {
      seenSignal = signal;
      return new Promise<string>(() => {});
    });
    renderWithAnswer(onAnswer);
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited answer request" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));
    await screen.findByText(/nothing has been received yet/);

    fireEvent.click(screen.getByRole("button", { name: "Cancel request" }));

    expect((seenSignal as unknown as AbortSignal).aborted).toBe(true);
    expect(
      screen.getByRole("button", { name: "Prepare cited answer request" }),
    ).toBeInTheDocument();
  });
});

describe("HelpCenter legacy seams", () => {
  it("requires confirmation before calling the optional assistant and validates citations", async () => {
    const onAskAssistant = vi.fn().mockResolvedValue({
      text: "Builds and chats are separate surfaces.",
      citations: ["product.readme"],
      uncertainty: "This answer is limited to the selected article.",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        onAskAssistant={onAskAssistant}
        assistantProviderLabel="Company gateway · review-model"
      />,
    );
    type("sessions builds chats");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    expect(onAskAssistant).not.toHaveBeenCalled();
    expect(
      screen.getByRole("alertdialog", { name: "Confirm assistant request" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/Company gateway · review-model/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));
    await waitFor(() => expect(onAskAssistant).toHaveBeenCalledOnce());
    // The legacy request shape is unchanged for an existing embedder.
    expect(onAskAssistant.mock.calls[0][0]).toMatchObject({
      schema: "grokptah.help-assistant-request.v1",
      articleId: "getting-started.sessions",
      requiresConfirmation: true,
    });
    expect(screen.getByText(/Draft answer — not product truth/)).toBeInTheDocument();
  });

  it("falls back to cited guidance when the assistant answer is ungrounded", async () => {
    const onAskAssistant = vi.fn().mockResolvedValue({
      text: "It is fully certified.",
      citations: ["unknown-source"],
      uncertainty: "",
    });
    render(<HelpCenter open onClose={vi.fn()} onAskAssistant={onAskAssistant} />);
    type("sessions builds chats");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    fireEvent.click(screen.getByRole("button", { name: "Send cited context" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(/answer rejected/));
    expect(screen.getByText(/Source-backed offline guidance/)).toBeInTheDocument();
    expect(screen.queryByText("It is fully certified.")).not.toBeInTheDocument();
  });

  it("requires confirmation before provider ranking and preserves corpus bounds", async () => {
    const onSearchSemantic = vi.fn().mockResolvedValue({
      results: [{ articleId: "synthetic.southern-relay", score: 0.88, rationale: "match" }],
      uncertainty: "Provider ranking is not product certification.",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        authority={fixtureAuthority}
        onSearchSemantic={onSearchSemantic}
        assistantProviderLabel="Company gateway · review-model"
      />,
    );
    type("beacon");

    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    expect(onSearchSemantic).not.toHaveBeenCalled();
    const confirm = screen.getByRole("alertdialog", { name: "Confirm meaning search" });
    expect(confirm).toHaveTextContent(/No article body or workspace data will be sent/);
    expect(confirm).toHaveTextContent(/cannot change whether Help answered or abstained/);

    fireEvent.click(screen.getByRole("button", { name: "Search by meaning" }));
    await waitFor(() => expect(onSearchSemantic).toHaveBeenCalledOnce());

    // The provider reordered the candidates…
    await waitFor(() =>
      expect(optionTitles()).toEqual(["Southern relay rotation", "Northern relay rotation"]));
    // …and could not turn the abstention into an answer.
    expect(
      screen.getByRole("status", { name: "Help retrieval outcome" }),
    ).toHaveTextContent("More than one article fits");
    expect(screen.getAllByText("Suggestion")).toHaveLength(2);
  });

  it("keeps the answer badge on the article the authority named, after a re-rank", async () => {
    const onSearchSemantic = vi.fn().mockResolvedValue({
      results: [
        { articleId: "synthetic.southern-relay", score: 0.9, rationale: "provider preference" },
        { articleId: "synthetic.northern-relay", score: 0.4, rationale: "second" },
      ],
      uncertainty: "Provider ranking is not product certification.",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        authority={fixtureAuthority}
        onSearchSemantic={onSearchSemantic}
      />,
    );
    type("relay rotation");
    expect(optionTitles()).toEqual(["Northern relay rotation", "Southern relay rotation"]);

    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    fireEvent.click(screen.getByRole("button", { name: "Search by meaning" }));

    await waitFor(() =>
      expect(optionTitles()).toEqual(["Southern relay rotation", "Northern relay rotation"]));

    // The provider moved the southern article to the top; the answer is still
    // the one retrieval chose, and the badge follows the article, not the row.
    const options = within(screen.getByRole("listbox")).getAllByRole("option");
    expect(options[0]).toHaveTextContent("Suggestion");
    expect(options[0]).not.toHaveTextContent("Answer");
    expect(options[1]).toHaveTextContent("Answer");
  });

  it("keeps offline retrieval when provider ranking is rejected", async () => {
    const onSearchSemantic = vi.fn().mockResolvedValue({
      results: [{ articleId: "synthetic.southern-relay", score: 2, rationale: "out of bounds" }],
      uncertainty: "bounded",
    });
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        authority={fixtureAuthority}
        onSearchSemantic={onSearchSemantic}
      />,
    );
    type("beacon");

    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    fireEvent.click(screen.getByRole("button", { name: "Search by meaning" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(/ranking rejected/));
    expect(optionTitles()).toEqual(["Northern relay rotation", "Southern relay rotation"]);
  });

  it("says plainly when no assistant is connected", () => {
    render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} />);
    type("lantern workspace");

    expect(screen.getByText(/not connected in this build/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Prepare cited question" }),
    ).not.toBeInTheDocument();
  });
});

describe("HelpCenter confirmation layers", () => {
  it("cancels a nested confirmation before closing the Help Center", () => {
    const onClose = vi.fn();
    render(
      <HelpCenter
        open
        onClose={onClose}
        authority={fixtureAuthority}
        onAskAssistant={vi.fn()}
      />,
    );
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question" }));
    expect(
      screen.getByRole("alertdialog", { name: "Confirm assistant request" }),
    ).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).not.toHaveBeenCalled();
    expect(
      screen.queryByRole("alertdialog", { name: "Confirm assistant request" }),
    ).not.toBeInTheDocument();
  });

  it("traps Tab inside the top confirmation and restores the layer opener", () => {
    const onClose = vi.fn();
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open Help";
    document.body.appendChild(opener);
    opener.focus();
    const focusSpy = vi.spyOn(opener, "focus");

    const { unmount } = render(
      <HelpCenter
        open
        onClose={onClose}
        authority={fixtureAuthority}
        onAskAssistant={vi.fn()}
      />,
    );
    type("lantern workspace");
    const prepare = screen.getByRole("button", { name: "Prepare cited question" });
    focusSpy.mockClear();
    fireEvent.click(prepare);

    const alert = screen.getByRole("alertdialog", { name: "Confirm assistant request" });
    const primary = screen.getByRole("button", { name: "Send cited context" });
    const cancel = screen.getByRole("button", { name: "Cancel" });
    expect(document.activeElement).toBe(primary);
    expect(focusSpy).not.toHaveBeenCalled();

    cancel.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(primary);

    primary.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);
    expect(alert.contains(document.activeElement)).toBe(true);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(document.activeElement).toBe(prepare);
    expect(focusSpy).not.toHaveBeenCalled();

    unmount();
    expect(focusSpy).toHaveBeenCalled();
    opener.remove();
  });

  it("closes stacked confirmations one layer at a time", () => {
    const onClose = vi.fn();
    render(
      <HelpCenter
        open
        onClose={onClose}
        authority={fixtureAuthority}
        onAskAssistant={vi.fn()}
        onSearchSemantic={vi.fn()}
      />,
    );
    type("lantern workspace");

    fireEvent.click(screen.getByRole("button", { name: "Prepare meaning search" }));
    expect(
      screen.getByRole("alertdialog", { name: "Confirm meaning search" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Prepare cited question", hidden: true }));
    expect(
      screen.getByRole("alertdialog", { name: "Confirm assistant request" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("alertdialog", { name: "Confirm meaning search" }),
    ).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(
      screen.getByRole("alertdialog", { name: "Confirm meaning search" }),
    ).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });
});

describe("HelpCenter boundaries", () => {
  it("searches without touching the network", () => {
    const fetchSpy = vi.fn();
    const original = globalThis.fetch;
    globalThis.fetch = fetchSpy as unknown as typeof fetch;
    try {
      render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} />);
      type("lantern workspace");
      type("beacon");
      type("zzzz qqqq");

      expect(fetchSpy).not.toHaveBeenCalled();
    } finally {
      globalThis.fetch = original;
    }
  });

  it("treats article prose as data, not as instructions", () => {
    render(<HelpCenter open onClose={vi.fn()} authority={fixtureAuthority} includeRestricted />);
    type("sealed vault");

    const labels = screen.getByRole("region", { name: "Article access and capabilities" });
    // The article is about promotion; reading it grants nothing and the UI
    // says so rather than offering the operation.
    expect(labels).toHaveTextContent("Operator only");
    expect(labels).toHaveTextContent(/live: unknown/);
    expect(screen.queryByRole("button", { name: /Promote/ })).not.toBeInTheDocument();
  });
});
