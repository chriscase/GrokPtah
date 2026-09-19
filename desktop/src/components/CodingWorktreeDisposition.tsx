import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { codingWorktreeAffordances } from "../lib/codingWorktreeDisposition";
import type { CodingWorktreeHostView } from "../lib/protocol";
import { StateCard } from "./StateCard";

type CodingWorktreeDispositionProps = {
  sessionId: string | null;
};

export function CodingWorktreeDisposition({ sessionId }: CodingWorktreeDispositionProps) {
  const [view, setView] = useState<CodingWorktreeHostView | null>(null);
  const [applyTarget, setApplyTarget] = useState("");
  const [digest, setDigest] = useState("");
  const [digestTouched, setDigestTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const requestEpoch = useRef(0);

  useEffect(() => {
    const epoch = ++requestEpoch.current;
    setView(null);
    setApplyTarget("");
    setDigest("");
    setDigestTouched(false);
    setError(null);
    setBusy(false);
    if (!sessionId) {
      return;
    }
    void api
      .codingWorktreeForSession(sessionId)
      .then((next) => {
        if (requestEpoch.current !== epoch) return;
        setView(next);
        if (next?.patchDigest) setDigest(next.patchDigest);
      })
      .catch((reason) => {
        if (requestEpoch.current !== epoch) return;
        setError(String(reason));
        setView(null);
      });
  }, [sessionId]);

  const affordances = codingWorktreeAffordances(view, applyTarget, digest);

  const run = async (mutation: () => Promise<CodingWorktreeHostView>) => {
    if (!view) return;
    const epoch = requestEpoch.current;
    setBusy(true);
    setError(null);
    try {
      const next = await mutation();
      if (requestEpoch.current !== epoch) return;
      setView(next);
      if (!digestTouched && next.patchDigest) setDigest(next.patchDigest);
    } catch (reason) {
      if (requestEpoch.current !== epoch) return;
      setError(String(reason));
    } finally {
      if (requestEpoch.current === epoch) setBusy(false);
    }
  };

  return (
    <section
      className="coding-worktree-disposition panel-block"
      aria-label="Coding worktree disposition"
      data-status={affordances.status}
    >
      <strong>Coding worktree disposition</strong>
      <p className="coding-worktree-disposition-copy">
        Thin AgentHost chrome for Pause, fence-first Stop, Accept, Discard, and
        Keep for review. No auto-Accept. Computer Mode stays off.
      </p>

      {!sessionId || affordances.status === "none" ? (
        <StateCard
          variant="empty"
          title="No coding worktree session"
          description={affordances.reason}
        />
      ) : (
        <>
          <dl className="coding-worktree-disposition-meta">
            <div>
              <dt>Handle</dt>
              <dd>{view?.handle}</dd>
            </div>
            <div>
              <dt>Phase</dt>
              <dd>{view?.phase}</dd>
            </div>
            <div>
              <dt>Disposition</dt>
              <dd>{view?.disposition ?? "none"}</dd>
            </div>
            <div>
              <dt>Patch digest</dt>
              <dd>{view?.patchDigest ?? "none"}</dd>
            </div>
          </dl>
          <p className="coding-worktree-disposition-reason">{affordances.reason}</p>
          <label className="coding-worktree-disposition-field">
            Apply target
            <input
              value={applyTarget}
              onChange={(event) => setApplyTarget(event.target.value)}
              placeholder="Explicit path — never main checkout"
              disabled={busy || affordances.status !== "active"}
              autoComplete="off"
            />
          </label>
          <label className="coding-worktree-disposition-field">
            Patch digest
            <input
              value={digest}
              onChange={(event) => {
                setDigestTouched(true);
                setDigest(event.target.value);
              }}
              placeholder="sha256:…"
              disabled={busy || affordances.status !== "active"}
              autoComplete="off"
            />
          </label>
          <div className="coding-worktree-disposition-actions">
            <button
              type="button"
              disabled={busy || !affordances.canPause}
              onClick={() => void run(() => api.codingWorktreePause(view!.handle))}
            >
              Pause
            </button>
            <button
              type="button"
              disabled={busy || !affordances.canStop}
              onClick={() => void run(() => api.codingWorktreeStop(view!.handle))}
            >
              Stop
            </button>
            <button
              type="button"
              disabled={busy || !affordances.canAccept}
              onClick={() =>
                void run(() =>
                  api.codingWorktreeAccept(view!.handle, applyTarget.trim(), digest.trim()),
                )
              }
            >
              Accept
            </button>
            <button
              type="button"
              className="danger"
              disabled={busy || !affordances.canDiscard}
              onClick={() => void run(() => api.codingWorktreeDiscard(view!.handle))}
            >
              Discard
            </button>
            <button
              type="button"
              disabled={busy || !affordances.canKeepForReview}
              onClick={() => void run(() => api.codingWorktreeKeepForReview(view!.handle))}
            >
              Keep for review
            </button>
          </div>
        </>
      )}

      {error && (
        <StateCard
          variant="error"
          title="Coding worktree command failed"
          description="The host refused the command. No auto-retry was attempted."
          technicalDetail={error}
        />
      )}
    </section>
  );
}
