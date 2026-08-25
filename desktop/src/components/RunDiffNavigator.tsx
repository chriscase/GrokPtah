import { useEffect, useMemo, useState } from "react";
import {
  changeSummary,
  diffTotals,
  firstChangedLine,
  isOpenable,
  parseUnifiedDiff,
  type DiffFile,
} from "../lib/sourceDiff";

export type RunDiffNavigatorProps = {
  /** Raw unified diff from the run review. */
  diff: string;
  /** True when the review capped the diff; shown so nobody assumes it is whole. */
  truncated?: boolean;
  /**
   * Open one changed file in the source viewer, in the run's own isolated
   * worktree, at its first change. Absent when no worktree is inspectable.
   */
  onOpenFile?: (path: string, line: number | null) => void;
  /** Run whose worktree the files live in, for the identity line. */
  runId: string;
};

/**
 * Per-file review before promotion.
 *
 * A single wall of diff text makes it easy to promote something unread. This
 * splits the review into files, keeps the exact line numbers of each hunk,
 * and lets the reviewer open any file directly in the run's own isolated
 * worktree — the tree the change would be promoted *from*.
 */
export function RunDiffNavigator({
  diff,
  truncated = false,
  onOpenFile,
  runId,
}: RunDiffNavigatorProps) {
  const files = useMemo<DiffFile[]>(() => parseUnifiedDiff(diff), [diff]);
  const [index, setIndex] = useState(0);

  // A fresh review resets the cursor rather than pointing past the end.
  useEffect(() => {
    setIndex(0);
  }, [diff]);

  const totals = useMemo(() => diffTotals(files), [files]);

  if (files.length === 0) {
    return (
      <div className="panel-block run-diff-empty" data-testid="run-diff-empty">
        {diff.trim() ? "No per-file changes could be read from this diff." : "No changes"}
      </div>
    );
  }

  const safeIndex = Math.min(index, files.length - 1);
  const current = files[safeIndex];
  const openLine = firstChangedLine(current);

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
        {onOpenFile && (
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

      <p className="run-diff-position" role="status" data-testid="run-diff-position">
        File {safeIndex + 1} of {files.length} · {current.path} · {current.status} ·{" "}
        {changeSummary(current)}
      </p>

      {current.binary ? (
        <div className="panel-block run-diff-binary" data-testid="run-diff-binary">
          Binary file. Its bytes are not shown.
        </div>
      ) : (
        <div className="run-diff-hunks" data-testid="run-diff-hunks">
          {current.hunks.map((hunk, hunkIndex) => (
            <div className="run-diff-hunk" key={`${hunk.oldStart}-${hunk.newStart}-${hunkIndex}`}>
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
      )}
    </section>
  );
}
