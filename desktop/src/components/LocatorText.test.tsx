import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LocatorText } from "./LocatorText";

afterEach(cleanup);

describe("LocatorText", () => {
  it("renders plain text when nothing can be opened", () => {
    const { container } = render(<LocatorText text="all tests passed" onOpenSource={vi.fn()} />);
    expect(container).toHaveTextContent("all tests passed");
    expect(screen.queryByTestId("locator-link")).not.toBeInTheDocument();
  });

  it("renders plain text when no open handler is supplied", () => {
    render(<LocatorText text="see src/lib/api.ts:12" />);
    expect(screen.queryByTestId("locator-link")).not.toBeInTheDocument();
  });

  it("turns a tool path into a direct-open control", () => {
    const onOpenSource = vi.fn();
    render(<LocatorText text="edited src/lib/api.ts:12:4 ok" onOpenSource={onOpenSource} />);

    const link = screen.getByTestId("locator-link");
    expect(link).toHaveTextContent("src/lib/api.ts:12:4");
    fireEvent.click(link);
    expect(onOpenSource).toHaveBeenCalledWith("src/lib/api.ts", 12);
  });

  it("labels a link for a screen reader with its line", () => {
    render(<LocatorText text="at src/a.rs:9" onOpenSource={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Open src/a.rs at line 9" })).toBeInTheDocument();
  });

  it("labels a link without a line", () => {
    render(<LocatorText text="see src/a.rs" onOpenSource={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Open src/a.rs" })).toBeInTheDocument();
  });

  it("is lossless: every character of the original text survives", () => {
    const text = "FAIL src/x.test.ts > case\n  at src/x.ts:88:11 (retry 2)";
    const { container } = render(<LocatorText text={text} onOpenSource={vi.fn()} />);
    expect(container.textContent).toBe(text);
  });

  it("links several paths in one block of output", () => {
    render(
      <LocatorText
        text="crates/a/src/lib.rs:164:59 and src/app.tsx(12,4)"
        onOpenSource={vi.fn()}
      />,
    );
    expect(screen.getAllByTestId("locator-link")).toHaveLength(2);
  });

  it("keeps a wrapping bracket outside the link", () => {
    render(<LocatorText text="(src/a.rs:3)" onOpenSource={vi.fn()} />);
    expect(screen.getByTestId("locator-link")).toHaveTextContent("src/a.rs:3");
  });

  it("honours the link limit without dropping text", () => {
    const text = Array.from({ length: 10 }, (_, i) => `src/f${i}.ts:1`).join(" ");
    const { container } = render(
      <LocatorText text={text} onOpenSource={vi.fn()} limit={3} />,
    );
    expect(screen.getAllByTestId("locator-link")).toHaveLength(3);
    expect(container.textContent).toBe(text);
  });

  it("passes an escaping path through so the boundary can refuse it", () => {
    const onOpenSource = vi.fn();
    render(<LocatorText text="tried ../../etc/passwd:1" onOpenSource={onOpenSource} />);
    fireEvent.click(screen.getByTestId("locator-link"));
    expect(onOpenSource).toHaveBeenCalledWith("../../etc/passwd", 1);
  });
});
