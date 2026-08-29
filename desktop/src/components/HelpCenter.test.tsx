import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HelpCenter } from "./HelpCenter";
import {
  HELP_VIEW_FIXTURE_CORPUS,
  HELP_VIEW_FIXTURE_QUERIES,
} from "../lib/help/view.fixtures";

afterEach(cleanup);

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
  return within(screen.getByRole("listbox"))
    .queryAllByRole("option")
    .map((option) => option.querySelector("strong")?.textContent ?? "");
}

const renderHelp = (props: Record<string, unknown> = {}) =>
  render(
    <HelpCenter open onClose={vi.fn()} corpus={HELP_VIEW_FIXTURE_CORPUS} {...props} />,
  );

describe("Help surface", () => {
  it("renders an accessible dialog over the offline corpus", () => {
    renderHelp();

    expect(screen.getByRole("dialog", { name: "Help" })).toBeInTheDocument();
    expect(searchBox()).toBeInTheDocument();
    expect(screen.getByRole("listbox", { name: "Help articles" })).toBeInTheDocument();
    expect(screen.getByText(/Offline hybrid retrieval/)).toHaveTextContent(
      /no network, no model/,
    );
  });

  it("closes on Escape", () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} corpus={HELP_VIEW_FIXTURE_CORPUS} />);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalledOnce();
  });

  it("can open already searching, for a caller that knows the question", () => {
    render(
      <HelpCenter
        open
        onClose={vi.fn()}
        corpus={HELP_VIEW_FIXTURE_CORPUS}
        initialQuery={HELP_VIEW_FIXTURE_QUERIES.answer}
      />,
    );

    expect(searchBox()).toHaveValue(HELP_VIEW_FIXTURE_QUERIES.answer);
    expect(
      screen.getByRole("status", { name: "Help retrieval outcome" }),
    ).toHaveTextContent("Answer from the shipped documentation");

    // Seeded, not controlled: the reader owns the field from the first keystroke.
    type("qqqq zzzz");
    expect(
      screen.getByRole("status", { name: "Help retrieval outcome" }),
    ).toHaveTextContent("No documented answer");
  });

  it("renders nothing when closed", () => {
    render(<HelpCenter open={false} onClose={vi.fn()} corpus={HELP_VIEW_FIXTURE_CORPUS} />);

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});

describe("Help retrieval states", () => {
  it("presents a decisive result as the answer, with a verified quote", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.answer);

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("Answer from the shipped documentation");

    const citations = screen.getByRole("region", { name: "Cited answer" });
    expect(citations).toHaveTextContent("Why this article is the answer");
    expect(within(citations).getByRole("blockquote")).toHaveTextContent(
      "Panes do not share state",
    );
    expect(citations).toHaveTextContent(/verified/);
    expect(citations).toHaveTextContent(/docs\/synthetic\/lantern-guide\.md/);
  });

  it("does not present an ambiguous result as an answer", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.ambiguous);

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("More than one article fits");
    expect(outcome).toHaveTextContent(/none is being presented as the response/);

    expect(optionTitles().slice(0, 2)).toEqual([
      "Northern relay rotation",
      "Southern relay rotation",
    ]);
    // Nothing carries the answer badge, and the detail pane says the article
    // on screen is a suggestion.
    expect(screen.queryByText("Answer")).not.toBeInTheDocument();
    expect(
      screen.getByText(/did not present this article as the answer/),
    ).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Cited answer" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Match evidence" })).toBeInTheDocument();
  });

  it("says a weak match is weak rather than answering with it", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.lowConfidence);

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("No confident answer");
    expect(outcome).toHaveTextContent("below-threshold");
  });

  it("says nothing matched rather than guessing", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.noMatch);

    const outcome = screen.getByRole("status", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("No documented answer");
    expect(outcome).toHaveTextContent(/not guessing at one/);
    expect(optionTitles()).toHaveLength(0);
    expect(screen.getByRole("heading", { name: "No matching guidance" })).toBeInTheDocument();
  });

  it("reports a rejected query as a rejection, not an abstention", () => {
    renderHelp();
    type("x".repeat(600));

    const outcome = screen.getByRole("alert", { name: "Help retrieval outcome" });
    expect(outcome).toHaveTextContent("Question not searched");
    expect(outcome).toHaveTextContent("query-too-long");
  });

  it("browses the corpus before a question is asked", () => {
    renderHelp();

    expect(
      screen.getByRole("status", { name: "Help retrieval outcome" }),
    ).toHaveTextContent("Browse the Help corpus");
    expect(screen.getByRole("listbox", { name: "Help articles" })).toBeInTheDocument();
  });
});

describe("Help keyboard navigation", () => {
  const activeOptionTitle = () => {
    const activeId = searchBox().getAttribute("aria-activedescendant");
    if (!activeId) return null;
    return document.getElementById(activeId)?.querySelector("strong")?.textContent ?? null;
  };

  it("moves the active option with the arrows without leaving the search field", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.ambiguous);
    const input = searchBox();

    expect(input).toHaveAttribute("aria-expanded", "true");
    expect(input).toHaveAttribute("aria-controls");
    expect(activeOptionTitle()).toBe("Northern relay rotation");

    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(activeOptionTitle()).toBe("Southern relay rotation");
    // Focus never leaves the combobox: the active option is named, not focused.
    expect(document.activeElement).toBe(input);

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(activeOptionTitle()).toBe("Northern relay rotation");

    fireEvent.keyDown(input, { key: "End" });
    expect(activeOptionTitle()).toBe(optionTitles()[optionTitles().length - 1]);

    fireEvent.keyDown(input, { key: "Home" });
    expect(activeOptionTitle()).toBe("Northern relay rotation");
  });

  it("wraps at both ends so a keyboard user is never stuck", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.ambiguous);
    const input = searchBox();
    const titles = optionTitles();

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(activeOptionTitle()).toBe(titles[titles.length - 1]);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(activeOptionTitle()).toBe(titles[0]);
  });

  it("opens the active option on Enter and marks exactly one selected", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.ambiguous);
    const input = searchBox();

    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(
      screen.getByRole("heading", { name: "Southern relay rotation" }),
    ).toBeInTheDocument();
    const selected = within(screen.getByRole("listbox"))
      .getAllByRole("option")
      .filter((option) => option.getAttribute("aria-selected") === "true");
    expect(selected).toHaveLength(1);
    expect(selected[0]).toHaveTextContent("Southern relay rotation");
  });

  it("leaves Escape to the dialog rather than trapping it in the search field", () => {
    const onClose = vi.fn();
    render(<HelpCenter open onClose={onClose} corpus={HELP_VIEW_FIXTURE_CORPUS} />);

    fireEvent.keyDown(searchBox(), { key: "Escape" });
    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalled();
  });
});

describe("Help focus management", () => {
  it("focuses the search field, traps Tab, and restores the opener", () => {
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open Help";
    document.body.appendChild(opener);
    opener.focus();

    const { unmount } = renderHelp();
    const dialog = screen.getByRole("dialog", { name: "Help" });
    const close = screen.getByRole("button", { name: "Close Help" });
    // Derive the cycle the same way the trap does, rather than assuming which
    // elements are focusable: the article pane is a tab stop too, so a
    // narrower selector would test a cycle the component does not have.
    const focusables = dialog.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
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

  it("keeps the scrolling article pane reachable by keyboard", () => {
    // Regression: at narrow widths the article pane scrolls and holds no
    // focusable child, so without a tab stop a keyboard-only reader could not
    // scroll it at all. axe-core caught this as `scrollable-region-focusable`.
    renderHelp();
    const article = screen.getByRole("article", { name: "Help article" });

    expect(article).toHaveAttribute("tabindex", "0");
    article.focus();
    expect(document.activeElement).toBe(article);
  });

  it("makes the application background inert while Help is open", () => {
    const { rerender } = render(
      <div className="app-shell">
        <main data-testid="app-background">Active lane</main>
        <HelpCenter open onClose={vi.fn()} corpus={HELP_VIEW_FIXTURE_CORPUS} />
      </div>,
    );

    const background = screen.getByTestId("app-background");
    expect(background).toHaveAttribute("inert");
    expect(background).toHaveAttribute("aria-hidden", "true");

    rerender(
      <div className="app-shell">
        <main data-testid="app-background">Active lane</main>
        <HelpCenter open={false} onClose={vi.fn()} corpus={HELP_VIEW_FIXTURE_CORPUS} />
      </div>,
    );
    expect(background).not.toHaveAttribute("inert");
    expect(background).not.toHaveAttribute("aria-hidden");
  });

  it("leaves consent layers reachable above Help", () => {
    render(
      <div className="app-shell">
        <main data-testid="app-background">Active lane</main>
        <div data-modal-layer="consent" data-testid="consent-layer">
          Allow this tool?
        </div>
        <HelpCenter open onClose={vi.fn()} corpus={HELP_VIEW_FIXTURE_CORPUS} />
      </div>,
    );

    expect(screen.getByTestId("app-background")).toHaveAttribute("inert");
    expect(screen.getByTestId("consent-layer")).not.toHaveAttribute("inert");
    expect(screen.getByTestId("consent-layer")).not.toHaveAttribute("aria-hidden", "true");
  });
});

describe("Help honesty", () => {
  it("states that provider, model, cost, and latency are unknown", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.answer);

    const card = screen.getByRole("region", { name: "Written answer" });
    expect(card).toHaveTextContent(/provider: unknown/);
    expect(card).toHaveTextContent(/model: unknown/);
    expect(card).toHaveTextContent(/cost: unknown/);
    expect(card).toHaveTextContent(/latency: unknown/);
  });

  it("says written answers are off in this build, as a property of the build", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.answer);

    const card = screen.getByRole("region", { name: "Written answer" });
    expect(card).toHaveTextContent(/Not available in this build/);
    expect(card).toHaveTextContent(/no request can leave this machine/);
    // The cited documentation is the product, not a degraded fallback.
    expect(card).toHaveTextContent(/not a fallback/);
  });

  it("describes an enabled answer seam without claiming one answered", () => {
    renderHelp({ answersEnabled: true });
    type(HELP_VIEW_FIXTURE_QUERIES.answer);

    const card = screen.getByRole("region", { name: "Written answer" });
    expect(card).not.toHaveTextContent(/Not available in this build/);
    expect(card).toHaveTextContent(/drafted from the cited articles above/);
    // Unknowns stay unknown whether or not the seam is enabled.
    expect(card).toHaveTextContent(/model: unknown/);
  });

  it("searches without touching the network", () => {
    const fetchSpy = vi.fn();
    const original = globalThis.fetch;
    globalThis.fetch = fetchSpy as unknown as typeof fetch;
    try {
      renderHelp();
      type(HELP_VIEW_FIXTURE_QUERIES.answer);
      type(HELP_VIEW_FIXTURE_QUERIES.ambiguous);
      type(HELP_VIEW_FIXTURE_QUERIES.noMatch);

      expect(fetchSpy).not.toHaveBeenCalled();
    } finally {
      globalThis.fetch = original;
    }
  });

  it("labels a rank signal as a signal, never as a certification", () => {
    renderHelp();
    type(HELP_VIEW_FIXTURE_QUERIES.answer);

    expect(screen.getByText(/Rank signal:/)).toHaveTextContent(
      /ranking signal only, not certification/,
    );
  });
});
