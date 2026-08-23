import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  normalizeToolStatus,
  ToolCallCard,
  ToolHistoryGroup,
} from "./ToolCallCard";
import type { TranscriptItem } from "../lib/protocol";

type ToolItem = Extract<TranscriptItem, { kind: "tool" }>;

const tool = (over: Partial<ToolItem> = {}): ToolItem => ({
  kind: "tool",
  callId: "c1",
  title: "read_file",
  status: "completed",
  ...over,
});

const makeTools = (n: number) =>
  Array.from({ length: n }, (_, i) => ({
    item: tool({ callId: `c${i + 1}`, title: `t${i + 1}` }),
    index: i,
  }));

/** The disclosure header for a card by tool title (accname = "title status"). */
const header = (title: string) =>
  screen.getByRole("button", { name: new RegExp(`^${title} `) });

const setClipboard = (writeText: (text: string) => Promise<void>) => {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
};

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  Reflect.deleteProperty(navigator as object, "clipboard");
});

describe("normalizeToolStatus", () => {
  it.each(["completed", "failed", "denied", "running", "pending"] as const)(
    "keeps canonical %s",
    (s) => {
      expect(normalizeToolStatus(s)).toEqual({ kind: s, raw: s });
    },
  );

  it("is case- and whitespace-insensitive for canonical values", () => {
    expect(normalizeToolStatus(" RUNNING ").kind).toBe("running");
    expect(normalizeToolStatus("Completed").kind).toBe("completed");
  });

  it("unwraps Rust-enum style object statuses", () => {
    expect(normalizeToolStatus({ failed: { code: 1 } }).kind).toBe("failed");
    expect(normalizeToolStatus({ Denied: {} }).kind).toBe("denied");
  });

  it("maps anything unrecognized to unknown while keeping the raw text", () => {
    expect(normalizeToolStatus("frobbed")).toEqual({
      kind: "unknown",
      raw: "frobbed",
    });
    expect(normalizeToolStatus("").kind).toBe("unknown");
    expect(normalizeToolStatus(null).kind).toBe("unknown");
    expect(normalizeToolStatus(undefined).kind).toBe("unknown");
    expect(normalizeToolStatus(42).kind).toBe("unknown");
    expect(normalizeToolStatus({}).kind).toBe("unknown");
  });
});

describe("status rendering", () => {
  it.each([
    ["completed", "✓"],
    ["failed", "✗"],
    ["denied", "⊘"],
    ["running", "●"],
    ["pending", "●"],
  ] as const)("labels %s with visible text, not color alone", (status, glyph) => {
    const { container } = render(<ToolCallCard item={tool({ status })} />);
    const card = container.querySelector(".tool-card");
    expect(card).toHaveAttribute("data-status", status);
    // The status word itself is always in the header text (non-color cue)…
    expect(header("read_file")).toHaveTextContent(status);
    // …and part of the accessible name.
    expect(header("read_file")).toHaveAccessibleName(
      new RegExp(`read_file ${status}`),
    );
    // The glyph is decoration only.
    const glyphEl = container.querySelector(".tool-card-glyph");
    expect(glyphEl).toHaveTextContent(glyph);
    expect(glyphEl).toHaveAttribute("aria-hidden", "true");
  });

  it("shows odd statuses as a safe unknown state", () => {
    const { container } = render(
      <ToolCallCard
        item={tool({ status: "Totally Weird!" as ToolItem["status"] })}
      />,
    );
    expect(container.querySelector(".tool-card")).toHaveAttribute(
      "data-status",
      "unknown",
    );
    expect(header("read_file")).toHaveTextContent("unknown");
    // Raw value never leaks into the class list.
    expect(container.querySelector(".tool-card")?.className).not.toContain(
      "Totally",
    );
  });

  it("surfaces the raw odd status as evidence when expanded", () => {
    render(
      <ToolCallCard
        item={tool({
          status: "frobbed" as ToolItem["status"],
          output: "some output",
        })}
      />,
    );
    fireEvent.click(header("read_file"));
    expect(screen.getByText(/reported status: frobbed/)).toBeInTheDocument();
  });

  it("surfaces the raw odd status even without output", () => {
    render(
      <ToolCallCard item={tool({ status: "frobbed" as ToolItem["status"] })} />,
    );
    fireEvent.click(header("read_file"));
    expect(
      screen.getByText("(no output — reported status: frobbed)"),
    ).toBeInTheDocument();
  });

  it("falls back to a generic title when the title is blank", () => {
    render(<ToolCallCard item={tool({ title: "  " })} />);
    expect(
      screen.getByRole("button", { name: /^tool / }),
    ).toBeInTheDocument();
  });
});

describe("disclosure behavior", () => {
  it("auto-opens while running and exposes accurate expanded state", () => {
    render(<ToolCallCard item={tool({ status: "running" })} />);
    const btn = header("read_file");
    expect(btn).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Running…")).toBeInTheDocument();
    // aria-controls points at the real body element.
    const bodyId = btn.getAttribute("aria-controls");
    expect(bodyId).toBeTruthy();
    expect(document.getElementById(bodyId!)).not.toBeNull();
  });

  it("collapses again once the call settles, without user input", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ status: "running" })} />,
    );
    rerender(<ToolCallCard item={tool({ status: "completed", output: "ok" })} />);
    expect(header("read_file")).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Running…")).toBeNull();
  });

  it("keeps a manually-opened settled card open across updates", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ output: "v1" })} />,
    );
    fireEvent.click(header("read_file"));
    expect(header("read_file")).toHaveAttribute("aria-expanded", "true");
    rerender(<ToolCallCard item={tool({ output: "v2" })} />);
    expect(header("read_file")).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("v2")).toBeInTheDocument();
  });

  it("keeps a manually-closed live card closed while output streams", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ status: "running", output: "chunk one" })} />,
    );
    fireEvent.click(header("read_file")); // close the auto-opened live card
    expect(header("read_file")).toHaveAttribute("aria-expanded", "false");
    rerender(
      <ToolCallCard
        item={tool({ status: "running", output: "chunk one chunk two" })}
      />,
    );
    expect(header("read_file")).toHaveAttribute("aria-expanded", "false");
  });

  it("resets manual state when the callId changes", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ callId: "a", status: "running" })} />,
    );
    fireEvent.click(header("read_file")); // manual close
    expect(header("read_file")).toHaveAttribute("aria-expanded", "false");
    rerender(<ToolCallCard item={tool({ callId: "b", status: "running" })} />);
    // New identity → automatic rules again → live auto-open.
    expect(header("read_file")).toHaveAttribute("aria-expanded", "true");
  });
});

describe("preview and output", () => {
  it("collapses whitespace and newlines into a single-line preview", () => {
    const { container } = render(
      <ToolCallCard item={tool({ output: "  a\n\n b\tc  " })} />,
    );
    const preview = container.querySelector(".tool-card-preview");
    expect(preview).toHaveTextContent("a b c");
    // Preview is visual-only; it never churns the accessible name.
    expect(preview).toHaveAttribute("aria-hidden", "true");
  });

  it("bounds long unbroken output in the collapsed preview", () => {
    const { container } = render(
      <ToolCallCard item={tool({ output: "x".repeat(300) })} />,
    );
    const text = container.querySelector(".tool-card-preview")?.textContent ?? "";
    expect(text.length).toBe(89);
    expect(text.endsWith("…")).toBe(true);
  });

  it("shows the exact multiline output when expanded", () => {
    const output = "line1\nline2\n  indented";
    const { container } = render(<ToolCallCard item={tool({ output })} />);
    fireEvent.click(header("read_file"));
    expect(container.querySelector(".tool-card-output")?.textContent).toBe(
      output,
    );
  });

  it("does not render the heavy output body while collapsed", () => {
    const { container } = render(
      <ToolCallCard item={tool({ output: "big ".repeat(50) })} />,
    );
    expect(container.querySelector(".tool-card-output")).toBeNull();
  });

  it("shows an honest live placeholder per state when there is no output yet", () => {
    // Pending has not started; saying "Running…" would overstate it.
    const { container, rerender } = render(
      <ToolCallCard item={tool({ status: "pending" })} />,
    );
    expect(screen.getByText("Waiting to start…")).toBeInTheDocument();
    expect(screen.queryByText("Running…")).toBeNull();
    // Collapsed pending card without output previews "waiting…".
    fireEvent.click(header("read_file"));
    expect(container.querySelector(".tool-card-preview")).toHaveTextContent(
      "waiting…",
    );

    // Running says so, in body and collapsed preview alike.
    rerender(<ToolCallCard item={tool({ callId: "c2", status: "running" })} />);
    expect(screen.getByText("Running…")).toBeInTheDocument();
    expect(screen.queryByText("Waiting to start…")).toBeNull();
    fireEvent.click(header("read_file"));
    expect(container.querySelector(".tool-card-preview")).toHaveTextContent(
      "running…",
    );

    // Settled with no output → explicit empty marker (expanded view).
    rerender(<ToolCallCard item={tool({ callId: "c3", status: "completed" })} />);
    fireEvent.click(header("read_file"));
    expect(screen.getByText("(no output)")).toBeInTheDocument();
  });

  it("keeps very long titles bounded but fully available", () => {
    const long = `read_${"x".repeat(300)}`;
    render(<ToolCallCard item={tool({ title: long })} />);
    // Full title reachable via tooltip and the accessible name.
    expect(screen.getByTitle(long)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: new RegExp(long.slice(0, 40)) }),
    ).toBeInTheDocument();
  });
});

describe("failure and denial evidence", () => {
  it("announces a failure once, without the output payload", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ status: "failed", output: "boom trace" })} />,
    );
    const announce = screen.getByTestId("tool-card-announce");
    expect(announce).toHaveAttribute("role", "status");
    expect(announce).toHaveTextContent("read_file failed");
    expect(announce.textContent).not.toContain("boom");
    // Output growth must not change the announcement (no re-announce churn).
    rerender(
      <ToolCallCard
        item={tool({ status: "failed", output: "boom trace, more boom" })}
      />,
    );
    expect(screen.getByTestId("tool-card-announce").textContent).toBe(
      "read_file failed",
    );
  });

  it("announces denials", () => {
    render(<ToolCallCard item={tool({ status: "denied" })} />);
    expect(screen.getByTestId("tool-card-announce")).toHaveTextContent(
      "read_file denied",
    );
  });

  it("stays silent for routine states and streaming updates", () => {
    const { rerender } = render(
      <ToolCallCard item={tool({ status: "running", output: "a" })} />,
    );
    expect(screen.getByTestId("tool-card-announce").textContent).toBe("");
    rerender(<ToolCallCard item={tool({ status: "running", output: "ab" })} />);
    expect(screen.getByTestId("tool-card-announce").textContent).toBe("");
    rerender(<ToolCallCard item={tool({ status: "completed", output: "ab" })} />);
    expect(screen.getByTestId("tool-card-announce").textContent).toBe("");
  });

  it("keeps failed evidence one click away", () => {
    render(
      <ToolCallCard item={tool({ status: "failed", output: "stack trace" })} />,
    );
    fireEvent.click(header("read_file"));
    expect(screen.getByText("stack trace")).toBeInTheDocument();
  });
});

describe("copy output", () => {
  it("copies the full output and confirms accessibly", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn(() => Promise.resolve());
    setClipboard(writeText);
    const output = "line1\nline2";
    render(<ToolCallCard item={tool({ output })} />);
    fireEvent.click(header("read_file"));

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy output" }));
    });
    expect(writeText).toHaveBeenCalledExactlyOnceWith(output);
    expect(
      screen.getByRole("button", { name: "Copied" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("tool-card-copy-status")).toHaveTextContent(
      "Output copied",
    );

    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(
      screen.getByRole("button", { name: "Copy output" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("tool-card-copy-status").textContent).toBe("");
  });

  it("shows and announces the failure when the clipboard refuses", async () => {
    vi.useFakeTimers();
    setClipboard(vi.fn(() => Promise.reject(new Error("nope"))));
    render(<ToolCallCard item={tool({ output: "data" })} />);
    fireEvent.click(header("read_file"));

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy output" }));
    });
    // A silent no-op would read as success and ship stale evidence: the
    // refusal must be visible on the control and announced to AT.
    const failed = screen.getByRole("button", { name: "Copy failed" });
    expect(failed).toHaveClass("is-failed");
    expect(screen.getByTestId("tool-card-copy-status")).toHaveTextContent(
      "Copy failed",
    );

    // Transient: the affordance returns so the user can retry.
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    const retry = screen.getByRole("button", { name: "Copy output" });
    expect(retry).not.toHaveClass("is-failed");
    expect(screen.getByTestId("tool-card-copy-status").textContent).toBe("");

    // A later successful copy is unaffected by the earlier failure.
    setClipboard(vi.fn(() => Promise.resolve()));
    await act(async () => {
      fireEvent.click(retry);
    });
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(screen.getByTestId("tool-card-copy-status")).toHaveTextContent(
      "Output copied",
    );
  });

  it("offers no copy affordance without output", () => {
    setClipboard(vi.fn(() => Promise.resolve()));
    render(<ToolCallCard item={tool({ status: "completed" })} />);
    fireEvent.click(header("read_file"));
    expect(screen.queryByRole("button", { name: "Copy output" })).toBeNull();
  });

  it("offers no copy affordance when the clipboard API is unavailable", () => {
    render(<ToolCallCard item={tool({ output: "data" })} />);
    fireEvent.click(header("read_file"));
    expect(screen.queryByRole("button", { name: "Copy output" })).toBeNull();
    // The evidence itself is still there.
    expect(screen.getByText("data")).toBeInTheDocument();
  });
});

describe("ToolHistoryGroup", () => {
  const titles = (container: HTMLElement) =>
    Array.from(container.querySelectorAll(".tool-card-title")).map(
      (el) => el.textContent,
    );

  it("renders nothing for an empty history", () => {
    const { container } = render(<ToolHistoryGroup tools={[]} />);
    expect(container.firstChild).toBeNull();
  });

  it("shows everything without a toggle at or under the keepRecent bound", () => {
    const { container, rerender } = render(
      <ToolHistoryGroup tools={makeTools(3)} keepRecent={8} />,
    );
    expect(titles(container)).toEqual(["t1", "t2", "t3"]);
    expect(screen.queryByRole("button", { name: /earlier tool/ })).toBeNull();

    rerender(<ToolHistoryGroup tools={makeTools(8)} keepRecent={8} />);
    expect(titles(container)).toHaveLength(8);
    expect(screen.queryByRole("button", { name: /earlier tool/ })).toBeNull();
  });

  it("labels the group with an accurate count", () => {
    render(<ToolHistoryGroup tools={makeTools(9)} keepRecent={8} />);
    expect(
      screen.getByRole("group", { name: "Tool activity: 9 calls" }),
    ).toBeInTheDocument();
  });

  it("singularizes a single older tool and reveals it in stable order", () => {
    const { container } = render(
      <ToolHistoryGroup tools={makeTools(9)} keepRecent={8} />,
    );
    const toggle = screen.getByRole("button", { name: "Show 1 earlier tool" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    // aria-controls target exists even while collapsed.
    expect(
      document.getElementById(toggle.getAttribute("aria-controls")!),
    ).not.toBeNull();
    expect(titles(container)).toEqual([
      "t2",
      "t3",
      "t4",
      "t5",
      "t6",
      "t7",
      "t8",
      "t9",
    ]);

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByRole("button", { name: "Hide 1 earlier tool" }),
    ).toBeInTheDocument();
    expect(titles(container)).toEqual([
      "t1",
      "t2",
      "t3",
      "t4",
      "t5",
      "t6",
      "t7",
      "t8",
      "t9",
    ]);
  });

  it("pluralizes multiple older tools", () => {
    render(<ToolHistoryGroup tools={makeTools(11)} keepRecent={8} />);
    expect(
      screen.getByRole("button", { name: "Show 3 earlier tools" }),
    ).toBeInTheDocument();
  });

  it("hides older tools again without losing focus, order intact", () => {
    const { container } = render(
      <ToolHistoryGroup tools={makeTools(10)} keepRecent={8} />,
    );
    const toggle = screen.getByRole("button", { name: "Show 2 earlier tools" });
    toggle.focus();
    fireEvent.click(toggle); // show
    fireEvent.click(toggle); // hide
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(document.activeElement).toBe(toggle);
    expect(titles(container)).toEqual([
      "t3",
      "t4",
      "t5",
      "t6",
      "t7",
      "t8",
      "t9",
      "t10",
    ]);
    // Round-trips do not reorder anything.
    fireEvent.click(toggle);
    expect(titles(container)[0]).toBe("t1");
  });

  it("keeps keyboard semantics on real buttons", () => {
    render(<ToolHistoryGroup tools={makeTools(9)} keepRecent={8} />);
    const toggle = screen.getByRole("button", { name: "Show 1 earlier tool" });
    expect(toggle.tagName).toBe("BUTTON");
    expect(toggle).toHaveAttribute("type", "button");
  });
});

describe("ToolCallCard CSS contrast contract (PR #360 audit)", () => {
  it("colors live status text with the state token, not the raw accent", () => {
    const root = dirname(fileURLToPath(import.meta.url));
    const css = readFileSync(join(root, "ToolCallCard.css"), "utf8");
    // --state-attention is identical to --accent in dark but holds 4.5:1+
    // in light, where the raw accent only reaches ~2.9:1 as text.
    expect(css).toMatch(
      /status-pending \.tool-card-status\s*{[^}]*var\(--state-attention\)/s,
    );
    expect(css).not.toMatch(
      /status-pending \.tool-card-status\s*{[^}]*var\(--accent\)/s,
    );
    // The clipboard-failure state must have a visible treatment.
    expect(css).toMatch(/\.tool-card-copy\.is-failed\s*{[^}]*var\(--danger\)/s);
  });
});
