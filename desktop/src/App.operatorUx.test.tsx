import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  acknowledgeOperatorPermission,
  permissionQueueAfterAcknowledgement,
  shouldHandleWorkspaceShortcut,
} from "./App";
import { PermissionModal } from "./components/PermissionModal";
import { enqueuePermission, headPermission } from "./lib/permissionQueue";
import type { PermissionRequest } from "./lib/protocol";

afterEach(cleanup);

const here = dirname(fileURLToPath(import.meta.url));
const appSrc = readFileSync(join(here, "App.tsx"), "utf8");
const modalSrc = readFileSync(join(here, "components", "PermissionModal.tsx"), "utf8");
const cssSrc = readFileSync(join(here, "styles", "app.css"), "utf8");

function req(
  id: string,
  sessionId: string,
  tool = "run_terminal_cmd",
): PermissionRequest {
  return {
    id,
    session_id: sessionId,
    tool_name: tool,
    summary: `Allow ${tool} on ${sessionId}?`,
    detail: { risk_tier: "ask" },
  };
}

function ConsentShortcutProbe({
  consentOpen,
  onShortcut,
}: {
  consentOpen: boolean;
  onShortcut: (name: string) => void;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!shouldHandleWorkspaceShortcut(consentOpen)) return;
      const meta = event.metaKey || event.ctrlKey;
      if (!meta) return;
      if (event.key === "b" || event.key === "B") onShortcut("toggle-sidebar");
      if (event.key === "1") onShortcut("focus-dock");
      if (event.key === "\\") onShortcut("open-beside");
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [consentOpen, onShortcut]);
  return consentOpen ? (
    <PermissionModal
      request={req("probe-1", "session-probe")}
      onRespond={async () => "acknowledged"}
    />
  ) : (
    <button type="button">idle</button>
  );
}

describe("App operator UX wiring", () => {
  it("suppresses workspace shortcuts only while consent exists", () => {
    expect(shouldHandleWorkspaceShortcut(true)).toBe(false);
    expect(shouldHandleWorkspaceShortcut(false)).toBe(true);
    const hits: string[] = [];
    const { rerender } = render(
      <ConsentShortcutProbe
        consentOpen
        onShortcut={(name) => hits.push(name)}
      />,
    );
    fireEvent.keyDown(window, { key: "b", metaKey: true });
    fireEvent.keyDown(window, { key: "1", ctrlKey: true });
    fireEvent.keyDown(window, { key: "\\", metaKey: true });
    expect(hits).toEqual([]);
    rerender(
      <ConsentShortcutProbe
        consentOpen={false}
        onShortcut={(name) => hits.push(name)}
      />,
    );
    fireEvent.keyDown(window, { key: "b", metaKey: true });
    fireEvent.keyDown(window, { key: "1", ctrlKey: true });
    expect(hits).toEqual(["toggle-sidebar", "focus-dock"]);
  });

  it("inerts every non-consent sibling in the operator shell", () => {
    render(
      <div data-testid="app-shell-probe">
        <header data-testid="shell-titlebar">title</header>
        <main data-testid="shell-main">
          <textarea data-testid="shell-composer" defaultValue="draft" />
        </main>
        <footer data-testid="shell-status">status</footer>
        <PermissionModal
          request={req("inert-1", "session-inert")}
          onRespond={async () => "acknowledged"}
        />
      </div>,
    );
    expect(screen.getByTestId("shell-titlebar")).toHaveAttribute("inert");
    expect(screen.getByTestId("shell-main")).toHaveAttribute("aria-hidden", "true");
    expect(screen.getByTestId("shell-status")).toHaveAttribute("inert");
    expect(screen.getByTestId("permission-modal-backdrop")).not.toHaveAttribute("inert");
  });

  it("maps permissionRespond Result to one closed acknowledgement and never flips a late resolve", async () => {
    const ok = vi.fn().mockResolvedValue(undefined);
    await expect(acknowledgeOperatorPermission(ok, 30)).resolves.toBe("acknowledged");
    expect(ok).toHaveBeenCalledOnce();

    const rejected = vi.fn().mockRejectedValue(new Error("bridge closed"));
    await expect(acknowledgeOperatorPermission(rejected, 30)).resolves.toBe("rejected");
    expect(rejected).toHaveBeenCalledOnce();

    const hang = vi.fn().mockReturnValue(new Promise(() => {}));
    await expect(acknowledgeOperatorPermission(hang, 20)).resolves.toBe("lost");
    expect(hang).toHaveBeenCalledOnce();

    let finish: ((value?: unknown) => void) | undefined;
    const late = vi.fn(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await expect(acknowledgeOperatorPermission(late, 15)).resolves.toBe("lost");
    finish?.("ok");
    await new Promise((resolve) => setTimeout(resolve, 15));
    expect(late).toHaveBeenCalledOnce();
  });

  it("does not dequeue, retry, or claim a backend outcome after rejection or lost ack", async () => {
    let q = enqueuePermission([], req("a1", "s1", "write_file"));
    q = enqueuePermission(q, req("a2", "s2"));
    expect(permissionQueueAfterAcknowledgement(q, "a1", "rejected")).toEqual(q);
    expect(permissionQueueAfterAcknowledgement(q, "a1", "lost")).toEqual(q);
    expect(headPermission(permissionQueueAfterAcknowledgement(q, "a1", "rejected"))?.tool_name).toBe(
      "write_file",
    );
    expect(permissionQueueAfterAcknowledgement(q, "a1", "acknowledged")[0]?.id).toBe("a2");
  });

  it("keeps the head request on screen when acknowledgement is rejected", async () => {
    const send = vi.fn().mockRejectedValue(new Error("no ack"));
    function Host() {
      const [queue, setQueue] = useState([
        req("host-1", "s1"),
        req("host-2", "s2", "write_file"),
      ]);
      const head = headPermission(queue);
      if (!head) return <div data-testid="no-permission">none</div>;
      return (
        <PermissionModal
          request={head}
          queuedBehind={queue.length - 1}
          onRespond={async (requestId) => {
            const ack = await acknowledgeOperatorPermission(send, 25);
            setQueue((current) =>
              permissionQueueAfterAcknowledgement(current, requestId, ack),
            );
            return ack;
          }}
        />
      );
    }
    render(<Host />);
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    fireEvent.click(screen.getByTestId("permission-allow"));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(send).toHaveBeenCalledOnce();
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    expect(screen.getByTestId("permission-recovery").textContent).toMatch(
      /Response unconfirmed/,
    );
  });

  it("keeps the queue head after timeout and after arbitrary onRespond resolution", async () => {
    const hang = vi.fn().mockReturnValue(new Promise(() => {}));
    function TimeoutHost() {
      const [queue, setQueue] = useState([
        req("to-1", "s1"),
        req("to-2", "s2", "write_file"),
      ]);
      const head = headPermission(queue);
      if (!head) return <div data-testid="no-permission">none</div>;
      return (
        <PermissionModal
          request={head}
          queuedBehind={queue.length - 1}
          onRespond={async (requestId) => {
            const ack = await acknowledgeOperatorPermission(hang, 20);
            setQueue((current) =>
              permissionQueueAfterAcknowledgement(current, requestId, ack),
            );
            return ack;
          }}
        />
      );
    }
    render(<TimeoutHost />);
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    expect(hang).toHaveBeenCalledOnce();
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    cleanup();

    function ArbitraryHost() {
      const [queue, setQueue] = useState([
        req("arb-1", "s1"),
        req("arb-2", "s2", "write_file"),
      ]);
      const head = headPermission(queue);
      if (!head) return <div data-testid="no-permission">none</div>;
      return (
        <PermissionModal
          request={head}
          queuedBehind={queue.length - 1}
          onRespond={async (requestId) => {
            const raw: unknown = "ok";
            if (raw === "acknowledged") {
              setQueue((current) =>
                permissionQueueAfterAcknowledgement(current, requestId, raw),
              );
            }
            return raw;
          }}
        />
      );
    }
    render(<ArbitraryHost />);
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    expect(screen.queryByTestId("no-permission")).toBeNull();
  });

  it("wires App.tsx as the sole acknowledgement owner without claiming success on failure", () => {
    expect(appSrc).toMatch(/shouldHandleWorkspaceShortcut\(\s*Boolean\(\s*permission\s*\)\s*\)/);
    expect(appSrc).toMatch(/acknowledgeOperatorPermission\(\(\)\s*=>\s*api\.permissionRespond/);
    expect(appSrc).toMatch(/if\s*\(\s*ack\s*!==\s*["']acknowledged["']\s*\)/);
    expect(appSrc).toMatch(/permissionQueueAfterAcknowledgement\(\s*q,\s*requestId,\s*ack\s*\)/);
    expect(appSrc).toMatch(/presentDeniedPermissionRecord\(\s*permission,\s*sessionId\s*\)/);
    expect(appSrc).toMatch(/if\s*\(\s*record\s*\)/);
    expect(appSrc).toMatch(/key=\{permission\.id\}/);
    expect(appSrc).toMatch(/Host acknowledged Deny/);
    expect(appSrc).not.toMatch(/Permission denied/);
    expect(appSrc).not.toMatch(/Continuing…/);
    expect(appSrc).not.toMatch(/settleOperatorConsentAcknowledgement/);
    expect(appSrc).not.toMatch(/["']owning-session["']/);
    expect(modalSrc).toMatch(/observeNonConsentInert/);
    expect(modalSrc).toMatch(/readConsentAcknowledgement/);
    expect(modalSrc).toMatch(/reduceConsentLock/);
    expect(modalSrc).toMatch(/owningSessionId/);
    expect(modalSrc).not.toMatch(/sessionIdForPermission/);
    expect(modalSrc).not.toMatch(/acknowledgementTimeoutMs/);
    expect(modalSrc).not.toMatch(/settleOperatorConsentAcknowledgement/);
    expect(modalSrc).not.toMatch(/setTimeout\s*\(/);
    expect(modalSrc).not.toMatch(
      /useEffect\(\s*\(\)\s*=>\s*\{[\s\S]*submitGate\.current\s*=\s*null/,
    );
    expect(cssSrc).toMatch(/\.modal\.permission-modal :focus-visible/);
    expect(cssSrc).toMatch(/forced-colors:\s*active/);
  });
});
