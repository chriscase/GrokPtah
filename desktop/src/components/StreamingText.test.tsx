import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { StreamingText } from "./StreamingText";
import { StreamingMarkdown } from "./StreamingMarkdown";

afterEach(() => cleanup());

describe("StreamingText", () => {
  it("keeps thought text as one live text flow", () => {
    const { container } = render(
      <StreamingText
        text="The task is complete. I need to provide a concise handoff."
        streaming
        animated={false}
      />,
    );

    const stream = container.querySelector(".stream-text");
    expect(stream?.textContent).toContain("The task is complete.");
    expect(stream?.querySelectorAll(".stream-token")).toHaveLength(0);
    expect(stream?.querySelector(".stream-caret")).not.toBeNull();
  });

  it("keeps live assistant text out of animated word spans", () => {
    const { container } = render(
      <StreamingMarkdown
        text="The user wants me to inspect the current diff and report only the files changed."
        streaming
      />,
    );

    expect(container.querySelectorAll(".stream-token")).toHaveLength(0);
  });

  it("keeps live markdown in one stable text flow until it settles", async () => {
    const { container } = render(
      <StreamingMarkdown
        text="The task is complete. I need to provide a concise handoff covering the changed files and verification."
        streaming
      />,
    );

    expect(container.querySelector(".stream-md-live")).not.toBeNull();
    expect(container.querySelector(".stream-md-stable")).toBeNull();
    expect(container.querySelector(".stream-md-tail")).toBeNull();
    await waitFor(() =>
      expect(container.querySelector(".stream-md-live")?.textContent).toContain(
        "The task is complete.",
      ),
    );
  });
});
