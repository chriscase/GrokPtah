import { useEffect, useMemo, useRef, useState } from "react";
import {
  changeSummary,
  diffTotals,
  firstChangedLine,
  isOpenable,
  readDiffEvidence,
  type DiffFile,
} from "../lib/sourceDiff";

export type RunDiffNavigatorProps = {
  /** Raw unified diff from the run review. This is the authority. */
  diff: string;
  /** True when the review capped the diff; evidence is then incomplete. */
  truncated?: boolean;
  /**
   * Open one changed file read-only in the run's own isolated worktree, at
   * its first change. Absent when no worktree is inspectable.
   */
  onOpenFile?: (path: string, line: number | null) => void;
  /** Run whose worktree the files live in, for the identity line. */
  runId: string;
  /**
   * Called with this run's id when the reviewer's acknowledgement changes.
   * Promotion is gated on it upstream; this component never decides, it only
   * reports. Taking the run id means the caller can pass one stable callback
   * for every run rather than a fresh closure per render.
   */
  onEvidenceAcknowledged?: (runId: string, acknowledged: boolean) => void;
};

/**
 * Per-file review before promotion.
 *
 * A single wall of diff text makes it easy to promote something unread. This
 * splits the review into files with exact line numbers and lets the reviewer
 * open any file directly in the run's own isolated worktree — the tree the
 * change would be promoted *from*.
 *
 * When the parse cannot account for the whole diff, the raw diff is shown
 * instead of a summary of it, the reasons are named, and acknowledgement is
 * withheld so promotion stays blocked. Incomplete evidence is a stop, not a
 * warning.
 */
export function RunDiffNavigator({
  diff,
  truncated = false,
  onOpenFile,
  runId,
  onEvidenceAcknowledged,
}: RunDiffNavigatorProps) {
  const evidence = useMemo(() => readDiffEvidence(diff, truncated), [diff, truncated]);
  const [index, setIndex] = useState(0);
  const [acknowledged, setAcknowledged] = useState(false);
  /**
   * Held in a ref so the reset effect does not depend on the caller's callback
   * identity. Depending on it would re-run the reset on every render, and the
   * reset writes state the caller owns — an unbounded loop rather than a reset.
   */
  const notifyRef = useRef(onEvidenceAcknowledged);
  notifyRef.current = onEvidenceAcknowledged;

  // A fresh review resets both the cursor and the acknowledgement: a reviewer
  // who acknowledged one diff has not acknowledged the next one.
  useEffect(() => {
    setIndex(0);
    setAcknowledged(false);
    notifyRef.current?.(runId, false);
  }, [diff, truncated, runId]);

  const files: DiffFile[] = evidence.files;
  const totals = useMemo(() => diffTotals(files), [files]);

  if (files.length === 0 && !evidence.raw.trim()) {
    return (
      <div className="panel-block run-diff-empty" data-testid="run-diff-empty">
        No changes
      </div>
    );
  }

  const safeIndex = Math.min(index, Math.max(0, files.length - 1));
  const current = files[safeIndex];
  const openLine = current ? firstChangedLine(current) : null;

  return (
    <section
      className="run-diff-navigator"
      aria-label={`Changed files in run ${runId}`}
      data-testid="run-diff-navigator"
    >
      <div className="run-diff-summary" data-testid="run-diff-summary">
        {totals.files} file{totals.files === 1 ? "" : "s"} · +{totals.additions} −
        {totals.deletions}
        {truncated ? " · diff truncated" : ""}
      </div>

      {!evidence.complete && (
        <div className="run-error run-diff-incomplete" role="alert" data-testid="run-diff-incomplete">
          <strong>Evidence is incomplete.</strong> Promotion is blocked because{" "}
          {evidence.reasons.join("; ")}. The authoritative diff is shown below exactly as
          received.
        </div>
      )}

      {!evidence.complete && (
        <details className="run-diff-raw" open data-testid="run-diff-raw" >
          <summary>Authoritative diff · {evidence.raw.length} characters</summary>
          <pre className="run-diff-raw-text">{evidence.raw}</pre>
        </details>
      )}

      {files.length > 0 && (
        <>
          <div className="run-diff-controls">
            <label className="run-diff-picker">
              <span className="sr-only">Changed file</span>
              <select
                aria-label="Changed file"
                value={safeIndex}
                data-testid="run-diff-file-select"
                onChange={(event) => setIndex(Number.parseInt(event.target.value, 10))}
              >
                {files.map((file, position) => (
                  <option key={`${file.path}-${position}`} value={position}>
                    {file.path} · {changeSummary(file)}
                  </option>
                ))}
              </select>
            </label>
            <button
              type="button"
              className="composer-chip quiet"
              onClick={() => setIndex((value) => Math.max(0, value - 1))}
              disabled={safeIndex === 0}
              data-testid="run-diff-prev"
            >
              Previous file
            </button>
            <button
              type="button"
              className="composer-chip quiet"
              onClick={() => setIndex((value) => Math.min(files.length - 1, value + 1))}
              disabled={safeIndex >= files.length - 1}
              data-testid="run-diff-next"
            >
              Next file
            </button>
            {onOpenFile && current && (
              <button
                type="button"
                className="composer-chip"
                disabled={!isOpenable(current)}
                data-testid="run-diff-open"
                onClick={() => onOpenFile(current.path, openLine)}
                title={
                  isOpenable(current)
                    ? `Open ${current.path} in the isolated worktree for run ${runId}`
                    : "This file cannot be opened from the isolated worktree"
                }
              >
                Open in isolated worktree
              </button>
            )}
          </div>

          {current && (
            <p className="run-diff-position" role="status" data-testid="run-diff-position">
              File {safeIndex + 1} of {files.length} · {current.path} · {current.status} ·{" "}
              {changeSummary(current)}
            </p>
          )}

          {current?.binary ? (
            <div className="panel-block run-diff-binary" data-testid="run-diff-binary">
              Binary file. Its bytes are not shown.
            </div>
          ) : (
            current && (
              <div className="run-diff-hunks" data-testid="run-diff-hunks">
                {current.hunks.map((hunk, hunkIndex) => (
                  <div
                    className="run-diff-hunk"
                    key={`${hunk.oldStart}-${hunk.newStart}-${hunkIndex}`}
                  >
                    <div className="run-diff-hunk-heading">
                      Lines {hunk.newStart}–{hunk.newStart + Math.max(0, hunk.newLines - 1)}
                      {hunk.heading ? ` · ${hunk.heading}` : ""}
                    </div>
                    <ol className="run-diff-lines" role="list">
                      {hunk.lines.map((line, lineIndex) => (
                        <li
                          key={`${hunkIndex}-${lineIndex}`}
                          className={`run-diff-line kind-${line.kind}`}
                          data-testid={`run-diff-line-${line.kind}`}
                        >
                          <span className="run-diff-line-number" aria-hidden="true">
                            {line.newNumber ?? line.oldNumber ?? ""}
                          </span>
                          <span className="sr-only">
                            {line.kind === "add"
                              ? `Added line ${line.newNumber}: `
                              : line.kind === "remove"
                                ? `Removed line ${line.oldNumber}: `
                                : `Line ${line.newNumber}: `}
                          </span>
                          <code className="run-diff-line-text">{line.text}</code>
                        </li>
                      ))}
                    </ol>
                  </div>
                ))}
              </div>
            )
          )}
        </>
      )}

      <label className="run-diff-acknowledge">
        <input
          type="checkbox"
          checked={acknowledged}
          disabled={!evidence.complete}
          data-testid="run-diff-acknowledge"
          onChange={(event) => {
            // Guarded as well as disabled: acknowledgement of evidence that is
            // not complete must be impossible, not merely discouraged.
            const next = evidence.complete && event.target.checked;
            setAcknowledged(next);
            notifyRef.current?.(runId, next);
          }}
        />
        <span>
          {evidence.complete
            ? "I have reviewed the complete diff for this run"
            : "Acknowledgement is unavailable while the evidence is incomplete"}
        </span>
      </label>

    </section>
  );
}
