/**
 * Read-only source document contract.
 *
 * This module is the browser-safe half of the source viewer: it holds the
 * wire types the Rust `xai-source-view` crate emits and the strict parsers
 * that validate them. It imports no Tauri API, touches no credential, and
 * performs no I/O, so it is safe to publish in the public UI core.
 */

/** Which boundary a document was read from. */
export type SourceRootKind = "workspace" | "isolated_worktree";

/** An approved inspection boundary with its exact canonical path. */
export interface SourceRootInfo {
  id: string;
  kind: SourceRootKind;
  label: string;
  /** Exact canonical path. Shown verbatim; never abbreviated in the DOM title. */
  path: string;
  runId: string | null;
}

/** How the bytes decoded. */
export type SourceEncoding = "utf8" | "utf8_lossy" | "binary";

/** Line-ending shape of the window that was read. */
export type SourceEol = "none" | "lf" | "crlf" | "mixed";

/** One rendered line with its real 1-based file line number. */
export interface SourceLine {
  number: number;
  text: string;
  truncated: boolean;
}

/** A bounded, read-only projection of one file. */
export interface SourceDocument {
  rootId: string;
  rootKind: SourceRootKind;
  rootPath: string;
  rootLabel: string;
  runId: string | null;
  relativePath: string;
  absolutePath: string;
  language: string;
  encoding: SourceEncoding;
  byteLen: number;
  bytesRead: number;
  lines: SourceLine[];
  lineCount: number;
  truncatedBytes: boolean;
  truncatedLines: boolean;
  lossyReplacements: number;
  eol: SourceEol;
  contentFingerprint: string;
}

/** Stable refusal codes emitted by the Rust boundary. */
export const SOURCE_VIEW_ERROR_CODES = [
  "no_approved_root",
  "unknown_root",
  "empty_path",
  "nul_byte",
  "absolute_path_outside_root",
  "parent_escape",
  "invalid_component",
  "symlink_rejected",
  "not_found",
  "not_a_file",
  "outside_root",
  "too_large",
  "root_unavailable",
  "io",
] as const;

export type SourceViewErrorCode = (typeof SOURCE_VIEW_ERROR_CODES)[number];

const ROOT_KINDS: readonly SourceRootKind[] = ["workspace", "isolated_worktree"];
const ENCODINGS: readonly SourceEncoding[] = ["utf8", "utf8_lossy", "binary"];
const EOLS: readonly SourceEol[] = ["none", "lf", "crlf", "mixed"];

function record(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

function text(source: Record<string, unknown>, key: string, what: string): string {
  const value = source[key];
  if (typeof value !== "string") throw new TypeError(`${what}.${key} must be a string`);
  return value;
}

function optionalText(source: Record<string, unknown>, key: string, what: string): string | null {
  const value = source[key];
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") throw new TypeError(`${what}.${key} must be a string or null`);
  return value;
}

function count(source: Record<string, unknown>, key: string, what: string): number {
  const value = source[key];
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new TypeError(`${what}.${key} must be a non-negative number`);
  }
  return value;
}

function flag(source: Record<string, unknown>, key: string, what: string): boolean {
  const value = source[key];
  if (typeof value !== "boolean") throw new TypeError(`${what}.${key} must be a boolean`);
  return value;
}

function oneOf<T extends string>(
  source: Record<string, unknown>,
  key: string,
  allowed: readonly T[],
  what: string,
): T {
  const value = source[key];
  if (typeof value !== "string" || !allowed.includes(value as T)) {
    throw new TypeError(`${what}.${key} must be one of ${allowed.join(", ")}`);
  }
  return value as T;
}

/** Validate one approved root as it crosses the boundary. */
export function parseSourceRoot(value: unknown): SourceRootInfo {
  const source = record(value, "source root");
  return {
    id: text(source, "id", "source root"),
    kind: oneOf(source, "kind", ROOT_KINDS, "source root"),
    label: text(source, "label", "source root"),
    path: text(source, "path", "source root"),
    runId: optionalText(source, "runId", "source root"),
  };
}

/** Validate a list of approved roots, refusing the whole list on any bad entry. */
export function parseSourceRoots(value: unknown): SourceRootInfo[] {
  if (!Array.isArray(value)) throw new TypeError("source roots must be an array");
  return value.map(parseSourceRoot);
}

function parseSourceLine(value: unknown, index: number): SourceLine {
  const source = record(value, `line ${index}`);
  const number = count(source, "number", `line ${index}`);
  if (!Number.isInteger(number) || number < 1) {
    throw new TypeError(`line ${index}.number must be a 1-based integer`);
  }
  return {
    number,
    text: text(source, "text", `line ${index}`),
    truncated: flag(source, "truncated", `line ${index}`),
  };
}

/** Validate a document as it crosses the boundary. */
export function parseSourceDocument(value: unknown): SourceDocument {
  const source = record(value, "source document");
  const rawLines = source.lines;
  if (!Array.isArray(rawLines)) throw new TypeError("source document.lines must be an array");
  const encoding = oneOf(source, "encoding", ENCODINGS, "source document");
  const lines = rawLines.map(parseSourceLine);
  if (encoding === "binary" && lines.length > 0) {
    throw new TypeError("binary source documents must not carry rendered lines");
  }
  return {
    rootId: text(source, "rootId", "source document"),
    rootKind: oneOf(source, "rootKind", ROOT_KINDS, "source document"),
    rootPath: text(source, "rootPath", "source document"),
    rootLabel: text(source, "rootLabel", "source document"),
    runId: optionalText(source, "runId", "source document"),
    relativePath: text(source, "relativePath", "source document"),
    absolutePath: text(source, "absolutePath", "source document"),
    language: text(source, "language", "source document"),
    encoding,
    byteLen: count(source, "byteLen", "source document"),
    bytesRead: count(source, "bytesRead", "source document"),
    lines,
    lineCount: count(source, "lineCount", "source document"),
    truncatedBytes: flag(source, "truncatedBytes", "source document"),
    truncatedLines: flag(source, "truncatedLines", "source document"),
    lossyReplacements: count(source, "lossyReplacements", "source document"),
    eol: oneOf(source, "eol", EOLS, "source document"),
    contentFingerprint: text(source, "contentFingerprint", "source document"),
  };
}

/**
 * Pull the stable refusal code out of a boundary error.
 *
 * The Rust adapter formats refusals as `code: human sentence`. Callers use
 * the code to pick an explanation and the sentence as the detail line.
 */
export function parseSourceViewErrorCode(error: unknown): SourceViewErrorCode | null {
  const message = error instanceof Error ? error.message : String(error ?? "");
  const separator = message.indexOf(":");
  if (separator <= 0) return null;
  const candidate = message.slice(0, separator).trim();
  return (SOURCE_VIEW_ERROR_CODES as readonly string[]).includes(candidate)
    ? (candidate as SourceViewErrorCode)
    : null;
}

/** Short, non-accusatory explanation for each refusal. */
export function sourceViewErrorSummary(error: unknown): string {
  switch (parseSourceViewErrorCode(error)) {
    case "no_approved_root":
      return "Open a project folder before inspecting files.";
    case "unknown_root":
      return "That workspace is no longer approved for inspection.";
    case "empty_path":
      return "No file path was supplied.";
    case "nul_byte":
    case "invalid_component":
      return "That path is not a readable file name.";
    case "absolute_path_outside_root":
    case "parent_escape":
    case "outside_root":
      return "That path is outside the approved workspace, so it was not read.";
    case "symlink_rejected":
      return "That path crosses a symbolic link. Links are never followed.";
    case "not_found":
      return "That file is not in the approved workspace.";
    case "not_a_file":
      return "That path is not a regular file.";
    case "too_large":
      return "That file is larger than the viewer will read.";
    case "root_unavailable":
      return "The approved workspace is no longer readable.";
    case "io":
      return "The file could not be read.";
    default:
      return "The file could not be opened.";
  }
}

/** Human summary of what was withheld, or null when the document is complete. */
export function truncationNotice(document: SourceDocument): string | null {
  const notes: string[] = [];
  if (document.truncatedBytes) {
    notes.push(`showing the first ${document.bytesRead} of ${document.byteLen} bytes`);
  }
  if (document.truncatedLines) {
    notes.push(`showing the first ${document.lines.length} of ${document.lineCount} lines`);
  }
  if (document.lossyReplacements > 0) {
    notes.push(
      `${document.lossyReplacements} byte${document.lossyReplacements === 1 ? "" : "s"} could not be decoded as UTF-8`,
    );
  }
  return notes.length > 0 ? notes.join(" · ") : null;
}

/** One-line identity for the viewer header: which exact tree this came from. */
export function rootIdentityLabel(document: SourceDocument): string {
  const kind = document.rootKind === "isolated_worktree" ? "Isolated worktree" : "Workspace";
  const run = document.runId ? ` · run ${document.runId}` : "";
  return `${kind}${run} · ${document.rootPath}`;
}

/** What a caller knows about the boundary it wants to read from. */
export interface SourceRootPreference {
  /** An exact root id, when the caller already resolved one. */
  rootId?: string | null;
  /** A run whose isolated worktree should be preferred. */
  runId?: string | null;
}

/**
 * Choose which approved boundary to read from.
 *
 * A run's own worktree wins when one is named, because reviewing a run means
 * reading the tree the change would be promoted *from*. Otherwise the shared
 * workspace is the default. Returns null rather than guessing when nothing
 * is approved.
 */
export function pickSourceRoot(
  roots: readonly SourceRootInfo[],
  preference: SourceRootPreference = {},
): SourceRootInfo | null {
  if (!Array.isArray(roots) || roots.length === 0) return null;
  if (preference.rootId) {
    return roots.find((root) => root.id === preference.rootId) ?? null;
  }
  if (preference.runId) {
    const worktree = roots.find(
      (root) => root.kind === "isolated_worktree" && root.runId === preference.runId,
    );
    // A named run that has no inspectable worktree is a refusal, not a
    // silent fall back to the shared workspace: they are different trees.
    return worktree ?? null;
  }
  return roots.find((root) => root.kind === "workspace") ?? roots[0];
}
