/**
 * Unified-diff reader for per-file review before promotion.
 *
 * The run inspector used to show one wall of diff text. This parses that
 * text into files and hunks with real line numbers on both sides, so a
 * reviewer can step file by file and open each change at the exact line in
 * the isolated worktree it came from.
 *
 * Pure and browser-safe: it reads a string and returns data.
 */

import { stripDiffPathPrefix } from "./sourceLocator";

/** What happened to a file across the diff. */
export type DiffFileStatus = "added" | "removed" | "modified" | "renamed";

/** One line inside a hunk, carrying its number on whichever side it exists. */
export interface DiffLine {
  kind: "context" | "add" | "remove";
  text: string;
  oldNumber: number | null;
  newNumber: number | null;
}

/** One `@@` range. */
export interface DiffHunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  /** Trailing section heading Git puts after the closing `@@`, if any. */
  heading: string;
  lines: DiffLine[];
}

/** One file's worth of change. */
export interface DiffFile {
  /** Best path to open: the new side when it exists, else the old side. */
  path: string;
  oldPath: string | null;
  newPath: string | null;
  status: DiffFileStatus;
  binary: boolean;
  additions: number;
  deletions: number;
  hunks: DiffHunk[];
}

const HUNK = /^@@+ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@+(.*)$/;

function blankFile(): DiffFile {
  return {
    path: "",
    oldPath: null,
    newPath: null,
    status: "modified",
    binary: false,
    additions: 0,
    deletions: 0,
    hunks: [],
  };
}

function devNull(path: string | null): boolean {
  return path === null || path === "/dev/null";
}

/** Settle `path` and `status` once both sides of a file header are known. */
function finalise(file: DiffFile): DiffFile {
  const oldMissing = devNull(file.oldPath);
  const newMissing = devNull(file.newPath);
  if (file.status !== "renamed") {
    if (oldMissing && !newMissing) file.status = "added";
    else if (!oldMissing && newMissing) file.status = "removed";
    else file.status = "modified";
  }
  const preferred = newMissing ? file.oldPath : file.newPath;
  file.path = preferred && preferred !== "/dev/null" ? preferred : (file.path ?? "");
  if (oldMissing) file.oldPath = null;
  if (newMissing) file.newPath = null;
  return file;
}

/**
 * Parse a unified diff into files.
 *
 * Tolerant by design: a truncated diff (the run review caps its size) still
 * yields every complete file that preceded the cut.
 */
export function parseUnifiedDiff(diff: string): DiffFile[] {
  if (typeof diff !== "string" || !diff.trim()) return [];
  const files: DiffFile[] = [];
  let file: DiffFile | null = null;
  let hunk: DiffHunk | null = null;
  let oldCursor = 0;
  let newCursor = 0;

  const closeFile = () => {
    if (file) files.push(finalise(file));
    file = null;
    hunk = null;
  };

  const rawLines = diff.split("\n");
  // A diff that ends with a newline yields one trailing empty element. That
  // is the terminator, not an empty context line, so drop it.
  if (rawLines.length > 0 && rawLines[rawLines.length - 1] === "") rawLines.pop();

  for (const raw of rawLines) {
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;

    if (line.startsWith("diff --git ")) {
      closeFile();
      file = blankFile();
      // `diff --git a/x b/x` gives us a name even when no ---/+++ follows.
      const paths = /^diff --git (\S+) (\S+)$/.exec(line);
      if (paths) {
        file.oldPath = stripDiffPathPrefix(paths[1]);
        file.newPath = stripDiffPathPrefix(paths[2]);
        file.path = file.newPath;
      }
      continue;
    }

    if (!file) {
      // A bare diff with no `diff --git` preamble still starts at `---`.
      if (line.startsWith("--- ")) {
        file = blankFile();
      } else {
        continue;
      }
    }

    if (line.startsWith("--- ")) {
      hunk = null;
      const path = line.slice(4).trim().split("\t")[0];
      file.oldPath = path === "/dev/null" ? "/dev/null" : stripDiffPathPrefix(path);
      continue;
    }
    if (line.startsWith("+++ ")) {
      hunk = null;
      const path = line.slice(4).trim().split("\t")[0];
      file.newPath = path === "/dev/null" ? "/dev/null" : stripDiffPathPrefix(path);
      continue;
    }
    if (line.startsWith("rename from ")) {
      file.status = "renamed";
      file.oldPath = line.slice("rename from ".length).trim();
      continue;
    }
    if (line.startsWith("rename to ")) {
      file.status = "renamed";
      file.newPath = line.slice("rename to ".length).trim();
      file.path = file.newPath;
      continue;
    }
    if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) {
      file.binary = true;
      continue;
    }

    const hunkHeader = HUNK.exec(line);
    if (hunkHeader) {
      oldCursor = Number.parseInt(hunkHeader[1], 10);
      newCursor = Number.parseInt(hunkHeader[3], 10);
      hunk = {
        oldStart: oldCursor,
        oldLines: hunkHeader[2] === undefined ? 1 : Number.parseInt(hunkHeader[2], 10),
        newStart: newCursor,
        newLines: hunkHeader[4] === undefined ? 1 : Number.parseInt(hunkHeader[4], 10),
        heading: hunkHeader[5].trim(),
        lines: [],
      };
      file.hunks.push(hunk);
      continue;
    }

    if (!hunk) continue;
    // "\ No newline at end of file" annotates the previous line, not a change.
    if (line.startsWith("\\")) continue;

    const marker = line.charAt(0);
    const text = line.slice(1);
    if (marker === "+") {
      hunk.lines.push({ kind: "add", text, oldNumber: null, newNumber: newCursor });
      newCursor += 1;
      file.additions += 1;
    } else if (marker === "-") {
      hunk.lines.push({ kind: "remove", text, oldNumber: oldCursor, newNumber: null });
      oldCursor += 1;
      file.deletions += 1;
    } else if (marker === " " || line === "") {
      hunk.lines.push({
        kind: "context",
        text: marker === " " ? text : "",
        oldNumber: oldCursor,
        newNumber: newCursor,
      });
      oldCursor += 1;
      newCursor += 1;
    }
  }

  closeFile();
  return files.filter((entry) => entry.path !== "");
}

/**
 * The line to open a file at: the first added line, else the first removed
 * line's position on the new side, else the start of the first hunk.
 */
export function firstChangedLine(file: DiffFile): number | null {
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "add" && line.newNumber !== null) return line.newNumber;
    }
  }
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "remove") return Math.max(1, hunk.newStart);
    }
  }
  return file.hunks.length > 0 ? Math.max(1, file.hunks[0].newStart) : null;
}

/** Compact `+n −m` summary for a file row. */
export function changeSummary(file: DiffFile): string {
  if (file.binary) return "binary";
  return `+${file.additions} −${file.deletions}`;
}

/** Whether the file still exists on the new side and can be opened. */
export function isOpenable(file: DiffFile): boolean {
  return file.status !== "removed" && !file.binary;
}

/** Totals for the navigator header. */
export function diffTotals(files: DiffFile[]): {
  files: number;
  additions: number;
  deletions: number;
} {
  return files.reduce(
    (total, file) => ({
      files: total.files + 1,
      additions: total.additions + file.additions,
      deletions: total.deletions + file.deletions,
    }),
    { files: 0, additions: 0, deletions: 0 },
  );
}
