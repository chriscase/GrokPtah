/**
 * Transport-neutral source-inspection contract.
 *
 * The desktop reaches the boundary over Tauri IPC; a browser reaches an
 * equivalent boundary through an authenticated ContextDesk broker. Both must
 * behave identically, so both implement this interface and share this
 * request validation. A parity test asserts neither side has an operation the
 * other lacks and that both refuse the same malformed requests.
 *
 * Nothing here talks to Tauri or to the network: it defines the shape and
 * validates it. The adapters supply the wire.
 */

import {
  parseSourceDocument,
  parseSourceReadCursor,
  parseSourceRootSnapshot,
  type SourceDocument,
  type SourceReadCursor,
  type SourceRootSnapshot,
} from "./sourceView";

/** The closed set of operations a source-view transport provides. */
export const SOURCE_VIEW_OPERATIONS = ["snapshot", "read", "revoke"] as const;
export type SourceViewOperation = (typeof SOURCE_VIEW_OPERATIONS)[number];

/** Which wire an adapter speaks. */
export type SourceViewChannel = "tauri" | "broker";

const TOKEN_PATTERN = /^sv1\.[0-9a-f]{32}\.(0|[1-9][0-9]{0,3})\.[0-9a-f]{32}$/;
const SNAPSHOT_ID_PATTERN = /^[0-9a-f]{32}$/;
const MAX_PATH_LENGTH = 4096;
const MAX_BYTES_CEILING = 4 * 1024 * 1024;
const MAX_LINES_CEILING = 5_000;
const MAX_LINE_CHARS_CEILING = 10_000;

export interface SourceSnapshotRequest {
  sessionId?: string | null;
}

export interface SourceReadRequest {
  token: string;
  path: string;
  sessionId?: string | null;
  /** Byte offset for a fresh range read. Mutually exclusive with `cursor`. */
  startByte?: number;
  cursor?: SourceReadCursor | null;
  maxBytes?: number;
  maxLines?: number;
  maxLineChars?: number;
}

export interface SourceViewTransport {
  readonly channel: SourceViewChannel;
  snapshot(request: SourceSnapshotRequest): Promise<SourceRootSnapshot>;
  read(request: SourceReadRequest): Promise<SourceDocument>;
  revoke(snapshotId: string): Promise<boolean>;
}

/** Thrown before anything reaches a wire. */
export class SourceViewRequestError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SourceViewRequestError";
  }
}

function refuse(message: string): never {
  throw new SourceViewRequestError(message);
}

function optionalCount(value: unknown, name: string, high: number): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1 || value > high) {
    refuse(`${name} must be an integer between 1 and ${high}`);
  }
  return value;
}

/** Validate an opaque identifier without interpreting it. */
export function validateSourceToken(token: unknown): string {
  if (typeof token !== "string" || !TOKEN_PATTERN.test(token)) {
    refuse("A source root token is required and must be well formed");
  }
  return token;
}

export function validateSnapshotId(snapshotId: unknown): string {
  if (typeof snapshotId !== "string" || !SNAPSHOT_ID_PATTERN.test(snapshotId)) {
    refuse("A snapshot id is required and must be well formed");
  }
  return snapshotId;
}

/**
 * Validate a read request and return the normalised form both adapters send.
 *
 * A request naming both a byte offset and a cursor is refused rather than
 * resolved by precedence: two answers to "where does this read start" is a
 * contract defect, not a preference.
 */
export function validateSourceReadRequest(request: SourceReadRequest): SourceReadRequest {
  const token = validateSourceToken(request.token);
  if (typeof request.path !== "string" || request.path.length === 0) {
    refuse("A file path is required");
  }
  if (request.path.length > MAX_PATH_LENGTH) {
    refuse(`A file path may not exceed ${MAX_PATH_LENGTH} characters`);
  }
  if (request.path.includes("\0")) {
    refuse("A file path may not contain a NUL byte");
  }
  if (request.sessionId !== undefined && request.sessionId !== null) {
    if (typeof request.sessionId !== "string" || request.sessionId.length > 128) {
      refuse("A session id must be a bounded string");
    }
  }

  let startByte: number | undefined;
  if (request.startByte !== undefined && request.startByte !== null) {
    if (
      typeof request.startByte !== "number" ||
      !Number.isSafeInteger(request.startByte) ||
      request.startByte < 0
    ) {
      refuse("A start byte must be a non-negative safe integer");
    }
    startByte = request.startByte;
  }

  let cursor: SourceReadCursor | undefined;
  if (request.cursor !== undefined && request.cursor !== null) {
    try {
      cursor = parseSourceReadCursor(request.cursor);
    } catch (caught) {
      refuse(`A continuation cursor is malformed: ${(caught as Error).message}`);
    }
  }
  if (cursor && startByte !== undefined && startByte !== 0) {
    refuse("A read names either a start byte or a cursor, never both");
  }

  return {
    token,
    path: request.path,
    sessionId: request.sessionId ?? null,
    startByte: cursor ? undefined : (startByte ?? 0),
    cursor: cursor ?? null,
    maxBytes: optionalCount(request.maxBytes, "maxBytes", MAX_BYTES_CEILING),
    maxLines: optionalCount(request.maxLines, "maxLines", MAX_LINES_CEILING),
    maxLineChars: optionalCount(request.maxLineChars, "maxLineChars", MAX_LINE_CHARS_CEILING),
  };
}

/** The wire body both adapters send for a read. */
export function sourceReadPayload(request: SourceReadRequest): Record<string, unknown> {
  const validated = validateSourceReadRequest(request);
  return {
    token: validated.token,
    path: validated.path,
    sessionId: validated.sessionId ?? null,
    startByte: validated.startByte ?? null,
    cursor: validated.cursor ?? null,
    maxBytes: validated.maxBytes ?? null,
    maxLines: validated.maxLines ?? null,
    maxLineChars: validated.maxLineChars ?? null,
  };
}

/** Parse a snapshot response, whichever wire it arrived on. */
export function parseSnapshotResponse(value: unknown): SourceRootSnapshot {
  return parseSourceRootSnapshot(value);
}

/** Parse a document response, whichever wire it arrived on. */
export function parseDocumentResponse(value: unknown): SourceDocument {
  return parseSourceDocument(value);
}

/** Parse a revoke response. */
export function parseRevokeResponse(value: unknown): boolean {
  if (typeof value === "boolean") return value;
  if (value && typeof value === "object" && "revoked" in value) {
    const revoked = (value as { revoked: unknown }).revoked;
    if (typeof revoked === "boolean") return revoked;
  }
  throw new TypeError("revoke response must report whether a snapshot was revoked");
}

/**
 * Confirm a transport implements the whole contract.
 *
 * Used by the parity test and by any host wiring a new channel: a transport
 * missing an operation is refused at construction rather than at first use.
 */
export function assertTransportComplete(transport: SourceViewTransport): SourceViewTransport {
  for (const operation of SOURCE_VIEW_OPERATIONS) {
    if (typeof (transport as unknown as Record<string, unknown>)[operation] !== "function") {
      refuse(`A source view transport must implement \`${operation}\``);
    }
  }
  if (transport.channel !== "tauri" && transport.channel !== "broker") {
    refuse("A source view transport must declare a known channel");
  }
  return transport;
}
