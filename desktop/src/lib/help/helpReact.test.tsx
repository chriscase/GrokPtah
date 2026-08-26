import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  HelpCitationList,
  HelpHighlightedText,
  HelpResults,
  HelpSearchInput,
  useHelpSearch,
} from "./react/primitives";
import { buildHelpExcerpt } from "./retrieval/highlight";
import { createHelpSearchController, describeHelpResultForAssistiveTech } from "./consumer";
import { searchHelpCorpus } from "./retrieval/hybrid";

afterEach(cleanup);

function Harness({ onActivate }: { onActivate?: (id: string) => void } = {}) {
  const { state, controller } = useHelpSearch();
  return (
    <div>
      <HelpSearchInput controller={controller} state={state} />
      <HelpResults
        state={state}
        controller={controller}
        onActivate={onActivate ? (result) => onActivate(result.articleId) : undefined}
      />
    </div>
  );
}

describe("Help React primitives", () => {
  it("renders highlights as elements, never as markup", () => {
    // The excerpt text is hostile on purpose: if any path built an HTML
    // string, this would become a real element.
    const excerpt = buildHelpExcerpt("<script>alert(1)</script> durable restart", ["durable"]);
    const { container } = render(<HelpHighlightedText excerpt={excerpt} />);
    expect(container.querySelector("script")).toBeNull();
    expect(container.innerHTML).not.toContain("<script>");
    expect(container.textContent).toContain("durable");
    expect(container.querySelectorAll("mark").length).toBeGreaterThan(0);
  });

  it("keeps highlight offsets inside the rendered text", () => {
    const excerpt = buildHelpExcerpt(
      "Durable runs expose a state, cursor, and evidence trail that survive a restart.",
      ["durable", "restart"],
    );
    const { container } = render(<HelpHighlightedText excerpt={excerpt} />);
    // Rendering must reproduce the excerpt exactly — no dropped or duplicated text.
    expect(container.textContent).toBe(excerpt.text);
  });

  it("exposes citations as a labelled list with the exact source anchor", () => {
    const result = searchHelpCorpus("durable run recovery").results[0]!;
    render(<HelpCitationList citations={result.citations} />);
    const list = screen.getByRole("list", { name: /Sources \(\d+\)/ });
    const items = within(list).getAllByRole("listitem");
    expect(items.length).toBe(result.citations.length);
    expect(items[0]!.textContent).toContain(result.citations[0]!.path);
    expect(items[0]!.textContent).toContain(result.citations[0]!.heading);
  });

  it("announces results, corrections, and redaction through a live region", () => {
    render(<Harness />);
    const input = screen.getByLabelText("Search Help");

    fireEvent.change(input, { target: { value: "chekpoint recovry" } });
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status.textContent).toMatch(/Help results?\./);
    expect(status.textContent).toMatch(/Showing results for/);

    fireEvent.change(input, { target: { value: "my key xai-AbCdEf0123456789AbCdEf on the gateway" } });
    expect(screen.getByRole("status").textContent).toMatch(/credential in your query was removed/);
    expect(screen.getByRole("status").textContent).not.toContain("AbCdEf");
  });

  it("states abstention in text rather than showing an empty list", () => {
    render(<Harness />);
    fireEvent.change(screen.getByLabelText("Search Help"), {
      target: { value: "how do I bake sourdough bread" },
    });
    expect(screen.getByRole("status").textContent).toMatch(/No confident match|No Help article/);
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("is operable by keyboard alone", () => {
    const onActivate = vi.fn();
    render(<Harness onActivate={onActivate} />);
    fireEvent.change(screen.getByLabelText("Search Help"), { target: { value: "computer use" } });

    const listbox = screen.getByRole("listbox", { name: "Help results" });
    expect(listbox).toHaveAttribute("tabindex", "0");
    const options = within(listbox).getAllByRole("option");
    expect(options.length).toBeGreaterThan(1);
    expect(options[0]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(listbox, { key: "ArrowDown" });
    expect(within(listbox).getAllByRole("option")[1]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(listbox, { key: "Home" });
    expect(within(listbox).getAllByRole("option")[0]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(listbox, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it("conveys state through semantics and text, not color", () => {
    render(<Harness />);
    fireEvent.change(screen.getByLabelText("Search Help"), { target: { value: "semantic search" } });
    const option = within(screen.getByRole("listbox")).getAllByRole("option")[0]!;

    // Selection is aria-selected plus a data attribute, and topic is spelled
    // out as text — all of which survive forced-colors mode, where a
    // background tint or a colored dot would carry no information.
    expect(option).toHaveAttribute("aria-selected", "true");
    expect(option).toHaveAttribute("data-active", "true");
    expect(option).toHaveAttribute("data-topic");
    expect(option.textContent).toContain("computer use");
    expect(option.getAttribute("style")).toBeNull();
  });

  it("carries no fixed pixel sizing that would clip at 200% text", () => {
    const { container } = render(<Harness />);
    fireEvent.change(screen.getByLabelText("Search Help"), { target: { value: "computer use" } });
    for (const element of container.querySelectorAll<HTMLElement>("*")) {
      const style = element.getAttribute("style") ?? "";
      expect(style).not.toMatch(/(?:^|;)\s*(?:height|max-height|width|max-width)\s*:\s*\d+px/);
      expect(style).not.toMatch(/overflow\s*:\s*hidden/);
      expect(style).not.toMatch(/white-space\s*:\s*nowrap/);
    }
  });

  it("labels the input with a real label element", () => {
    render(<Harness />);
    const input = screen.getByLabelText("Search Help");
    expect(input.tagName).toBe("INPUT");
    expect(input).toHaveAttribute("aria-describedby");
    // A placeholder is not a label: it disappears on input.
    expect(input.getAttribute("aria-label")).toBeNull();
  });

  it("describes a result for assistive technology without score noise", () => {
    const result = searchHelpCorpus("durable run recovery").results[0]!;
    const description = describeHelpResultForAssistiveTech(result, 5);
    expect(description).toMatch(/^Result 1 of 5\./);
    expect(description).toContain(result.title);
    expect(description).toMatch(/cited from/);
    expect(description).not.toMatch(/fused|BM25|0\.\d/);
  });
});

describe("Help search controller", () => {
  it("drives retrieval without React and cancels cleanly", () => {
    const controller = createHelpSearchController();
    const seen: number[] = [];
    const unsubscribe = controller.subscribe((state) => seen.push(state.results.length));

    controller.search("durable run recovery");
    expect(controller.getState().results.length).toBeGreaterThan(0);
    expect(controller.getState().corpusDigest).toMatch(/^sha256:/);

    controller.moveActive(1);
    expect(controller.getState().activeIndex).toBe(1);

    controller.clear();
    expect(controller.getState().results).toHaveLength(0);

    unsubscribe();
    controller.search("computer use");
    expect(seen.length).toBeGreaterThan(0);
    controller.dispose();
  });
});
