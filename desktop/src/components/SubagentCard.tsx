import { useState } from "react";
import type {
  SubagentExecutionMode,
  SubagentInfo,
} from "../lib/protocol";
import { safeErrorMessage } from "../lib/errorMessage";

const MODE_LABELS: Record<SubagentExecutionMode, string> = {
  unknown: "Legacy mode",
  worktree: "Isolated worktree",
  project_copy: "Isolated project copy",
  shared_read_only: "Shared / read-only",
  shared_mutating: "Shared / writes enabled",
  isolation_failed: "Isolation failed",
};

export function SubagentCard({
  agent,
  onCancel,
}: {
  agent: SubagentInfo;
  onCancel: (id: string) => Promise<void>;
}) {
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mode = agent.execution_mode ?? "unknown";
  const sharedMutation = mode === "shared_mutating";

  async function cancel() {
    if (cancelling) return;
    setCancelling(true);
    setError(null);
    try {
      await onCancel(agent.id);
    } catch (cancelError) {
      setError(safeErrorMessage(cancelError));
    } finally {
      setCancelling(false);
    }
  }

  return (
    <article
      className={[
        "subagent-card",
        `is-${String(agent.status).toLowerCase().replace(/\s+/g, "-")}`,
        sharedMutation ? "is-shared-mutating" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      data-testid="subagent-card"
    >
      <div className="subagent-card-topline">
        <span className="subagent-card-kind">{agent.kind || "agent"}</span>
        <span
          className={`subagent-card-mode is-${mode.replaceAll("_", "-")}`}
        >
          {MODE_LABELS[mode]}
        </span>
      </div>
      <div className="subagent-card-title">
        {agent.title || agent.id.slice(0, 8)}
      </div>
      <div className="subagent-card-status">{agent.status}</div>
      {agent.cwd && (
        <div className="subagent-card-cwd" title={agent.cwd}>
          <span>cwd</span>
          <code>{agent.cwd}</code>
        </div>
      )}
      {sharedMutation && (
        <div className="subagent-card-warning" role="alert">
          This child can edit the parent session's working folder.
        </div>
      )}
      {agent.summary && (
        <div className="subagent-card-summary" title={agent.summary}>
          {String(agent.summary).slice(0, 160)}
          {String(agent.summary).length > 160 ? "..." : ""}
        </div>
      )}
      {agent.last_tool && (
        <div className="subagent-card-tool">tool: {agent.last_tool}</div>
      )}
      {error && <div className="subagent-card-error">{error}</div>}
      {String(agent.status) === "running" && (
        <button
          type="button"
          className="danger"
          data-testid="subagent-cancel"
          title="Cancel this child only"
          disabled={cancelling}
          onClick={() => void cancel()}
        >
          {cancelling ? "Cancelling..." : "Cancel child"}
        </button>
      )}
    </article>
  );
}
