/**
 * Read-only source inspection contract.
 *
 * This is the browser-safe half of the source viewer: the wire types the Rust
 * `xai-source-view` crate emits, strict parsers for them, and the one
 * implementation of the chunk-reassembly rule. It imports no Tauri API,
 * touches no credential, performs no I/O, and is safe to publish.
 *
 * Two properties are load-bearing and asserted by test:
 *
 * * **The contract is closed.** Every payload rejects unknown keys, every
 *   enumeration is a fixed set, and every integer is range-checked. A field
 *   the boundary did not promise cannot reach a consumer as data.
 * * **Nothing here carries an absolute path.** A tree is identified by
 *   `pathDigest`, a file by `identity`. A parser that meets an absolute path
 *   refuses the payload rather than passing it through.
 */

export const SOURCE_VIEW_CONTRACT = "grokptah.source-view.v1";
export const SOURCE_VIEW_TOKEN_VERSION = "sv1";
export const SOURCE_VIEW_REPLAY_POLICY = "idempotent-within-validity";

/** Largest exactly-representable integer; every count is checked against it. */
const MAX_SAFE = Number.MAX_SAFE_INTEGER;
const MAX_CHUNK_BYTES = 4 * 1024 * 1024;
const MAX_CHUNK_LINES = 5_000;
const MAX_LINE_CHARS = 10_000;

const TOKEN_PATTERN = /^sv1\.[0-9a-f]{32}\.(0|[1-9][0-9]{0,3})\.[0-9a-f]{32}$/;
const DIGEST_PATTERN = /^[0-9a-f]{64}$/;
const SNAPSHOT_ID_PATTERN = /^[0-9a-f]{32}$/;
const CARRY_PATTERN = /^([0-9a-f]{2}){0,3}$/;

/** Keys that would mean the boundary leaked a location. */
const FORBIDDEN_KEYS = ["path", "absolutePath", "rootPath", "workspacePath", "cwd"] as const;

export type SourceRootKind = "workspace" | "isolated_worktree";
export type SourceContentVerdict = "text" | "text_lossy" | "binary";
export type SourceEol = "none" | "lf" | "crlf" | "mixed";
export type SourceIdentityStability = "exact" | "heuristic";

/** One approved boundary. `token` is the only way to name it. */
export interface SourceRootDescriptor {
  token: string;
  kind: SourceRootKind;
  /** Short human label. Chrome, not identity. */
  label: string;
  /** Identity of the directory, without its location. */
  pathDigest: string;
  /** Identity of the directory *node*, so a swap is visible. */
  identityDigest: string;
  runId: string | null;
}

/** A non-mutating projection of everything one principal may inspect. */
export interface SourceRootSnapshot {
  snapshotId: string;
  revision: number;
  issuedAtMs: number;
  expiresAtMs: number;
  principalFingerprint: string;
  policyFingerprint: string;
  replayPolicy: typeof SOURCE_VIEW_REPLAY_POLICY;
  roots: SourceRootDescriptor[];
}

export interface SourceContentClass {
  verdict: SourceContentVerdict;
  scannedBytes: number;
  /** False means the verdict describes only the scanned prefix. */
  completeScan: boolean;
}

export type SourceDocumentIdentity =
  | { kind: "content"; digest: string }
  | { kind: "pinned"; digest: string; stability: SourceIdentityStability };

export interface SourceEffectiveLimits {
  maxBytes: number;
  maxLines: number;
  maxLineChars: number;
}

export interface SourceLine {
  number: number;
  text: string;
  truncated: boolean;
}

export interface SourceReadCursor {
  byteOffset: number;
  nextLineNumber: number;
  carryHex: string;
  continuesLine: boolean;
  documentDigest: string;
}

export interface SourceChunk {
  lines: SourceLine[];
  startByte: number;
  bytesConsumed: number;
  lossyReplacements: number;
  eol: SourceEol;
  continuesPrevious: boolean;
  continuesNext: boolean;
  nextCursor: SourceReadCursor | null;
  eof: boolean;
}

export interface SourceDocument {
  contract: typeof SOURCE_VIEW_CONTRACT;
  root: SourceRootDescriptor;
  snapshotId: string;
  revision: number;
  relativePath: string;
  language: string;
  byteLen: number;
  content: SourceContentClass;
  identity: SourceDocumentIdentity;
  limits: SourceEffectiveLimits;
  chunk: SourceChunk;
}

/** Closed refusal set, mirrored from the Rust contract. */
export const SOURCE_VIEW_ERROR_CODES = [
  "no_approved_root",
  "snapshot_unknown",
  "token_malformed",
  "token_signature_invalid",
  "token_expired",
  "token_revoked",
  "principal_mismatch",
  "policy_drift",
  "unknown_root",
  "empty_path",
  "nul_byte",
  "absolute_path_outside_root",
  "parent_escape",
  "invalid_component",
  "reserved_device_name",
  "alternate_data_stream",
  "unsupported_path_form",
  "symlink_rejected",
  "reparse_point_rejected",
  "not_found",
  "not_a_file",
  "outside_root",
  "root_identity_changed",
  "document_changed",
  "too_large",
  "range_invalid",
  "cursor_invalid",
  "root_unavailable",
  "io",
] as const;

export type SourceViewErrorCode = (typeof SOURCE_VIEW_ERROR_CODES)[number];

/** Codes that mean *who is asking* was refused, not *what was asked*. */
export const SOURCE_VIEW_AUTHORIZATION_CODES: readonly SourceViewErrorCode[] = [
  "no_approved_root",
  "snapshot_unknown",
  "token_malformed",
  "token_signature_invalid",
  "token_expired",
  "token_revoked",
  "principal_mismatch",
  "policy_drift",
  "unknown_root",
];

// ------------------------------------------------------------- primitives

function object(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

/**
 * Enforce a closed shape.
 *
 * Unknown keys are refused rather than ignored: silently dropping a field the
 * boundary did not promise is how a leak survives review.
 */
function closed(source: Record<string, unknown>, allowed: readonly string[], what: string): void {
  // Location-bearing keys are named first, because "this payload leaked a
  // path" is a different and more urgent finding than "this payload has an
  // extra field".
  for (const forbidden of FORBIDDEN_KEYS) {
    if (forbidden in source) {
      throw new TypeError(`${what} must not carry \`${forbidden}\``);
    }
  }
  for (const key of Object.keys(source)) {
    if (!allowed.includes(key)) {
      throw new TypeError(`${what} carries unexpected field \`${key}\``);
    }
  }
}

function text(source: Record<string, unknown>, key: string, what: string, max = 4096): string {
  const value = source[key];
  if (typeof value !== "string") throw new TypeError(`${what}.${key} must be a string`);
  if (value.length > max) throw new TypeError(`${what}.${key} is longer than ${max}`);
  return value;
}

function pattern(
  source: Record<string, unknown>,
  key: string,
  expression: RegExp,
  what: string,
): string {
  const value = text(source, key, what);
  if (!expression.test(value)) throw new TypeError(`${what}.${key} is malformed`);
  return value;
}

function nullableText(source: Record<string, unknown>, key: string, what: string): string | null {
  if (!(key in source)) throw new TypeError(`${what}.${key} is required, even as null`);
  const value = source[key];
  if (value === null) return null;
  if (typeof value !== "string" || value.length === 0 || value.length > 256) {
    throw new TypeError(`${what}.${key} must be a bounded string or null`);
  }
  return value;
}

function integer(
  source: Record<string, unknown>,
  key: string,
  what: string,
  low = 0,
  high = MAX_SAFE,
): number {
  const value = source[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new TypeError(`${what}.${key} must be a safe integer`);
  }
  if (value < low || value > high) {
    throw new TypeError(`${what}.${key} must be between ${low} and ${high}`);
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

// --------------------------------------------------------------- parsers

const ROOT_KEYS = ["token", "kind", "label", "pathDigest", "identityDigest", "runId"] as const;

export function parseSourceRootDescriptor(value: unknown): SourceRootDescriptor {
  const source = object(value, "source root");
  closed(source, ROOT_KEYS, "source root");
  return {
    token: pattern(source, "token", TOKEN_PATTERN, "source root"),
    kind: oneOf(source, "kind", ["workspace", "isolated_worktree"] as const, "source root"),
    label: text(source, "label", "source root", 512),
    pathDigest: pattern(source, "pathDigest", DIGEST_PATTERN, "source root"),
    identityDigest: pattern(source, "identityDigest", DIGEST_PATTERN, "source root"),
    runId: nullableText(source, "runId", "source root"),
  };
}

const SNAPSHOT_KEYS = [
  "snapshotId",
  "revision",
  "issuedAtMs",
  "expiresAtMs",
  "principalFingerprint",
  "policyFingerprint",
  "replayPolicy",
  "roots",
] as const;

export function parseSourceRootSnapshot(value: unknown): SourceRootSnapshot {
  const source = object(value, "source snapshot");
  closed(source, SNAPSHOT_KEYS, "source snapshot");
  const replayPolicy = text(source, "replayPolicy", "source snapshot", 64);
  if (replayPolicy !== SOURCE_VIEW_REPLAY_POLICY) {
    throw new TypeError("source snapshot declares an unexpected replay policy");
  }
  const rawRoots = source.roots;
  if (!Array.isArray(rawRoots)) throw new TypeError("source snapshot.roots must be an array");
  if (rawRoots.length > 256) throw new TypeError("source snapshot.roots is unbounded");
  const roots = rawRoots.map(parseSourceRootDescriptor);
  const tokens = new Set(roots.map((root) => root.token));
  if (tokens.size !== roots.length) {
    throw new TypeError("source snapshot repeats a root token");
  }
  return {
    snapshotId: pattern(source, "snapshotId", SNAPSHOT_ID_PATTERN, "source snapshot"),
    revision: integer(source, "revision", "source snapshot", 1),
    issuedAtMs: integer(source, "issuedAtMs", "source snapshot"),
    expiresAtMs: integer(source, "expiresAtMs", "source snapshot"),
    principalFingerprint: pattern(
      source,
      "principalFingerprint",
      DIGEST_PATTERN,
      "source snapshot",
    ),
    policyFingerprint: pattern(source, "policyFingerprint", DIGEST_PATTERN, "source snapshot"),
    replayPolicy: SOURCE_VIEW_REPLAY_POLICY,
    roots,
  };
}

const CURSOR_KEYS = [
  "byteOffset",
  "nextLineNumber",
  "carryHex",
  "continuesLine",
  "documentDigest",
] as const;

export function parseSourceReadCursor(value: unknown): SourceReadCursor {
  const source = object(value, "read cursor");
  closed(source, CURSOR_KEYS, "read cursor");
  return {
    byteOffset: integer(source, "byteOffset", "read cursor"),
    nextLineNumber: integer(source, "nextLineNumber", "read cursor", 1),
    carryHex: pattern(source, "carryHex", CARRY_PATTERN, "read cursor"),
    continuesLine: flag(source, "continuesLine", "read cursor"),
    documentDigest: pattern(source, "documentDigest", DIGEST_PATTERN, "read cursor"),
  };
}

function parseSourceLine(value: unknown, index: number): SourceLine {
  const what = `line ${index}`;
  const source = object(value, what);
  closed(source, ["number", "text", "truncated"], what);
  return {
    number: integer(source, "number", what, 1),
    text: text(source, "text", what, MAX_LINE_CHARS),
    truncated: flag(source, "truncated", what),
  };
}

const CHUNK_KEYS = [
  "lines",
  "startByte",
  "bytesConsumed",
  "lossyReplacements",
  "eol",
  "continuesPrevious",
  "continuesNext",
  "nextCursor",
  "eof",
] as const;

export function parseSourceChunk(value: unknown): SourceChunk {
  const source = object(value, "source chunk");
  closed(source, CHUNK_KEYS, "source chunk");
  const rawLines = source.lines;
  if (!Array.isArray(rawLines)) throw new TypeError("source chunk.lines must be an array");
  if (rawLines.length > MAX_CHUNK_LINES) throw new TypeError("source chunk.lines exceeds the cap");
  const lines = rawLines.map(parseSourceLine);
  for (let index = 1; index < lines.length; index += 1) {
    if (lines[index].number !== lines[index - 1].number + 1) {
      throw new TypeError("source chunk lines must be consecutive");
    }
  }
  const nextCursorRaw = source.nextCursor;
  if (nextCursorRaw === undefined) throw new TypeError("source chunk.nextCursor is required");
  const nextCursor = nextCursorRaw === null ? null : parseSourceReadCursor(nextCursorRaw);
  const eof = flag(source, "eof", "source chunk");
  if (eof && nextCursor !== null) {
    throw new TypeError("a finished chunk must not carry a continuation cursor");
  }
  const continuesNext = flag(source, "continuesNext", "source chunk");
  if (continuesNext && nextCursor === null) {
    throw new TypeError("a continued chunk must carry a continuation cursor");
  }
  if (nextCursor !== null && nextCursor.continuesLine !== continuesNext) {
    throw new TypeError("chunk and cursor disagree about line continuation");
  }
  return {
    lines,
    startByte: integer(source, "startByte", "source chunk"),
    bytesConsumed: integer(source, "bytesConsumed", "source chunk"),
    lossyReplacements: integer(source, "lossyReplacements", "source chunk"),
    eol: oneOf(source, "eol", ["none", "lf", "crlf", "mixed"] as const, "source chunk"),
    continuesPrevious: flag(source, "continuesPrevious", "source chunk"),
    continuesNext,
    nextCursor,
    eof,
  };
}

function parseContentClass(value: unknown): SourceContentClass {
  const source = object(value, "content class");
  closed(source, ["verdict", "scannedBytes", "completeScan"], "content class");
  return {
    verdict: oneOf(source, "verdict", ["text", "text_lossy", "binary"] as const, "content class"),
    scannedBytes: integer(source, "scannedBytes", "content class"),
    completeScan: flag(source, "completeScan", "content class"),
  };
}

function parseDocumentIdentity(value: unknown): SourceDocumentIdentity {
  const source = object(value, "document identity");
  const kind = oneOf(source, "kind", ["content", "pinned"] as const, "document identity");
  if (kind === "content") {
    closed(source, ["kind", "digest"], "document identity");
    return { kind, digest: pattern(source, "digest", DIGEST_PATTERN, "document identity") };
  }
  closed(source, ["kind", "digest", "stability"], "document identity");
  return {
    kind,
    digest: pattern(source, "digest", DIGEST_PATTERN, "document identity"),
    stability: oneOf(source, "stability", ["exact", "heuristic"] as const, "document identity"),
  };
}

function parseEffectiveLimits(value: unknown): SourceEffectiveLimits {
  const source = object(value, "effective limits");
  closed(source, ["maxBytes", "maxLines", "maxLineChars"], "effective limits");
  return {
    maxBytes: integer(source, "maxBytes", "effective limits", 1, MAX_CHUNK_BYTES),
    maxLines: integer(source, "maxLines", "effective limits", 1, MAX_CHUNK_LINES),
    maxLineChars: integer(source, "maxLineChars", "effective limits", 16, MAX_LINE_CHARS),
  };
}

const DOCUMENT_KEYS = [
  "contract",
  "root",
  "snapshotId",
  "revision",
  "relativePath",
  "language",
  "byteLen",
  "content",
  "identity",
  "limits",
  "chunk",
] as const;

export function parseSourceDocument(value: unknown): SourceDocument {
  const source = object(value, "source document");
  closed(source, DOCUMENT_KEYS, "source document");
  if (source.contract !== SOURCE_VIEW_CONTRACT) {
    throw new TypeError("source document declares an unexpected contract");
  }
  const content = parseContentClass(source.content);
  const chunk = parseSourceChunk(source.chunk);
  if (content.verdict === "binary" && chunk.lines.length > 0) {
    throw new TypeError("a binary document must not carry rendered lines");
  }
  const relativePath = text(source, "relativePath", "source document");
  if (relativePath.length === 0) throw new TypeError("source document.relativePath is empty");
  if (relativePath.startsWith("/") || /^[A-Za-z]:/.test(relativePath)) {
    throw new TypeError("source document.relativePath must be root-relative");
  }
  return {
    contract: SOURCE_VIEW_CONTRACT,
    root: parseSourceRootDescriptor(source.root),
    snapshotId: pattern(source, "snapshotId", SNAPSHOT_ID_PATTERN, "source document"),
    revision: integer(source, "revision", "source document", 1),
    relativePath,
    language: text(source, "language", "source document", 32),
    byteLen: integer(source, "byteLen", "source document"),
    content,
    identity: parseDocumentIdentity(source.identity),
    limits: parseEffectiveLimits(source.limits),
    chunk,
  };
}

// ------------------------------------------------------------- refusals

export function parseSourceViewErrorCode(error: unknown): SourceViewErrorCode | null {
  const message = error instanceof Error ? error.message : String(error ?? "");
  const separator = message.indexOf(":");
  if (separator <= 0) return null;
  const candidate = message.slice(0, separator).trim();
  return (SOURCE_VIEW_ERROR_CODES as readonly string[]).includes(candidate)
    ? (candidate as SourceViewErrorCode)
    : null;
}

/** True when the refusal means the caller must obtain a fresh snapshot. */
export function isAuthorizationRefusal(error: unknown): boolean {
  const code = parseSourceViewErrorCode(error);
  return code !== null && SOURCE_VIEW_AUTHORIZATION_CODES.includes(code);
}

export function sourceViewErrorSummary(error: unknown): string {
  switch (parseSourceViewErrorCode(error)) {
    case "no_approved_root":
      return "Open a project folder before inspecting files.";
    case "snapshot_unknown":
    case "token_expired":
    case "token_revoked":
      return "This view's authorization expired. Reopen the file to refresh it.";
    case "token_malformed":
    case "token_signature_invalid":
      return "That request was not authorized and was refused.";
    case "principal_mismatch":
      return "That authorization belongs to a different session.";
    case "policy_drift":
      return "The workspace changed after this view was authorized. Reopen the file.";
    case "unknown_root":
      return "That workspace is no longer approved for inspection.";
    case "empty_path":
      return "No file path was supplied.";
    case "nul_byte":
    case "invalid_component":
    case "reserved_device_name":
    case "alternate_data_stream":
    case "unsupported_path_form":
      return "That path is not a readable file name.";
    case "absolute_path_outside_root":
    case "parent_escape":
    case "outside_root":
      return "That path is outside the approved workspace, so it was not read.";
    case "symlink_rejected":
    case "reparse_point_rejected":
      return "That path crosses a link. Links are never followed.";
    case "not_found":
      return "That file is not in the approved workspace.";
    case "not_a_file":
      return "That path is not a regular file.";
    case "root_identity_changed":
      return "The workspace was replaced since this view was authorized.";
    case "document_changed":
      return "The file changed while it was being read. Reopen it.";
    case "too_large":
      return "That file is larger than the viewer will read.";
    case "range_invalid":
    case "cursor_invalid":
      return "That position is no longer valid for this file. Reopen it.";
    case "root_unavailable":
      return "The approved workspace is no longer readable.";
    case "io":
      return "The file could not be read.";
    default:
      return "The file could not be opened.";
  }
}

// ---------------------------------------------------------- reassembly

/**
 * Append one chunk to accumulated lines, joining a continued line.
 *
 * Mirrors `LineAssembler::push_chunk` in the Rust crate. A chunk may end
 * mid-line; the next chunk continues that line under the same number. This is
 * the only implementation of that rule on this side of the boundary.
 */
export function appendSourceChunk(lines: SourceLine[], chunk: SourceChunk): SourceLine[] {
  const merged = lines.slice();
  let start = 0;
  if (chunk.continuesPrevious && merged.length > 0 && chunk.lines.length > 0) {
    const tail = merged[merged.length - 1];
    const head = chunk.lines[0];
    if (tail.number === head.number) {
      merged[merged.length - 1] = {
        number: tail.number,
        text: tail.text + head.text,
        truncated: tail.truncated || head.truncated,
      };
      start = 1;
    }
  }
  for (let index = start; index < chunk.lines.length; index += 1) {
    merged.push(chunk.lines[index]);
  }
  return merged;
}

// ------------------------------------------------------------- display

/** Short, non-reversible form of a digest, for the identity strip. */
export function digestLabel(digest: string): string {
  return digest.slice(0, 12);
}

/**
 * One line naming the exact tree a document came from.
 *
 * Deliberately a digest rather than a path: the reader needs to know *which*
 * tree, and telling them the host's directory layout is not required for that.
 */
export function rootIdentityLabel(document: SourceDocument): string {
  const kind = document.root.kind === "isolated_worktree" ? "Isolated worktree" : "Workspace";
  const run = document.root.runId ? ` · run ${document.root.runId}` : "";
  return `${kind}${run} · ${document.root.label} · ${digestLabel(document.root.pathDigest)}`;
}

/** What a projection is withholding, or null when it is complete. */
export function projectionNotice(document: SourceDocument): string | null {
  const notes: string[] = [];
  if (document.content.verdict === "binary") {
    notes.push(`binary content, ${document.byteLen} bytes, not rendered as text`);
  }
  if (document.content.verdict === "text_lossy") {
    notes.push("some bytes are not valid UTF-8 and are shown as replacement characters");
  }
  if (!document.content.completeScan && document.content.verdict !== "binary") {
    notes.push(`classified from the first ${document.content.scannedBytes} bytes`);
  }
  if (document.chunk.lossyReplacements > 0) {
    notes.push(
      `${document.chunk.lossyReplacements} undecodable byte${
        document.chunk.lossyReplacements === 1 ? "" : "s"
      } in this section`,
    );
  }
  if (document.identity.kind === "pinned") {
    notes.push(
      document.identity.stability === "exact"
        ? "identity pinned to the open file rather than its content"
        : "identity pinned heuristically; a replaced file may not be detected",
    );
  }
  return notes.length > 0 ? notes.join(" · ") : null;
}

/** Progress through a paged document, for the reader and for tests. */
export function readProgress(document: SourceDocument, loadedLines: number): string {
  if (document.chunk.eof) {
    return `${loadedLines} line${loadedLines === 1 ? "" : "s"} · complete`;
  }
  const read = document.chunk.startByte + document.chunk.bytesConsumed;
  const percent = document.byteLen === 0 ? 100 : Math.floor((read / document.byteLen) * 100);
  return `${loadedLines} line${loadedLines === 1 ? "" : "s"} · ${percent}% of ${document.byteLen} bytes`;
}

// ------------------------------------------------------- root selection

/** How a caller names the one root it means. */
export type SourceRootSelector =
  | { by: "token"; token: string }
  | { by: "run"; runId: string }
  | { by: "workspace" };

/**
 * The outcome of naming a root.
 *
 * `ambiguous` is a first-class answer rather than a tie broken by order:
 * silently picking the first workspace is how a reader ends up reviewing one
 * tree while believing they are reading another.
 */
export type SourceRootSelection =
  | { kind: "resolved"; root: SourceRootDescriptor }
  | { kind: "ambiguous"; candidates: SourceRootDescriptor[] }
  | { kind: "absent" };

/**
 * Resolve a selector against exactly one snapshot.
 *
 * Never falls back to another root, another kind, or another snapshot. A
 * caller that cannot name one root gets `ambiguous` or `absent` and must
 * choose or refuse.
 */
export function selectSourceRoot(
  snapshot: SourceRootSnapshot | null,
  selector: SourceRootSelector,
): SourceRootSelection {
  if (!snapshot) return { kind: "absent" };
  let candidates: SourceRootDescriptor[];
  switch (selector.by) {
    case "token":
      candidates = snapshot.roots.filter((root) => root.token === selector.token);
      break;
    case "run":
      candidates = snapshot.roots.filter(
        (root) => root.kind === "isolated_worktree" && root.runId === selector.runId,
      );
      break;
    case "workspace":
      candidates = snapshot.roots.filter((root) => root.kind === "workspace");
      break;
  }
  if (candidates.length === 1) return { kind: "resolved", root: candidates[0] };
  if (candidates.length === 0) return { kind: "absent" };
  return { kind: "ambiguous", candidates };
}

/** Whether a snapshot is still inside its validity window. */
export function isSnapshotLive(snapshot: SourceRootSnapshot | null, nowMs: number): boolean {
  return snapshot !== null && nowMs < snapshot.expiresAtMs;
}

/**
 * Whether a snapshot should be refreshed before it is used again.
 *
 * Refreshing early avoids a refusal the reader would experience as the viewer
 * breaking mid-scroll.
 */
export function shouldRefreshSnapshot(
  snapshot: SourceRootSnapshot | null,
  nowMs: number,
  marginMs = 30_000,
): boolean {
  if (!snapshot) return true;
  return nowMs + marginMs >= snapshot.expiresAtMs;
}
