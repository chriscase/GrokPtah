/**
 * TypeScript mirror of the Rust Help authority.
 *
 * `crates/common/grokptah-help-authority` is the reference implementation.
 * This module exists because the browser broker and any embedding consumer
 * need the same decision without a Rust runtime — not because the rule set
 * lives in two places. Both are executed against the same fixture set
 * (`fixtures/authority-parity.json`) and must agree case for case; the parity
 * test fails if they diverge.
 *
 * Same three properties as the Rust side: default deny for anything not
 * public, closed contracts that reject unknown fields rather than dropping
 * them, and receipts that carry ids and digests only.
 */
import { domainDigest } from "../canonical/digest";

export const HELP_DECISION_REQUEST_SCHEMA = "grokptah.help-authority-request.v1" as const;
export const HELP_DECISION_RESPONSE_SCHEMA = "grokptah.help-authority-response.v1" as const;

/** Mirrors `MAX_SOURCES_PER_DECISION` / `MAX_ID_BYTES` in the Rust crate. */
export const HELP_MAX_SOURCES_PER_DECISION = 64;
export const HELP_MAX_ID_BYTES = 256;

export type HelpVisibility = "public" | "project" | "private";

export type HelpCapability =
  | "help_search"
  | "help_search_project"
  | "help_search_private"
  | "help_answer";

export type HelpAction = "search" | "answer" | "read_source";

export type HelpDenyReason =
  | "unknown_schema"
  | "missing_capability"
  | "tenant_mismatch"
  | "scope_mismatch"
  | "malformed_scope"
  | "stale_index"
  | "bounds";

export type HelpPrincipal = {
  readonly principal_id: string;
  readonly tenant_id: string;
  readonly project_ids?: readonly string[];
  readonly capabilities?: readonly HelpCapability[];
};

export type HelpSourceDescriptor = {
  readonly source_id: string;
  readonly visibility: HelpVisibility;
  readonly tenant_id: string;
  readonly project_id?: string;
  readonly owner_principal_id?: string;
  readonly digest: string;
};

export type HelpDecisionRequest = {
  readonly schema: string;
  readonly action: HelpAction;
  readonly principal: HelpPrincipal;
  readonly corpus_digest: string;
  readonly index_digest: string;
  readonly sources?: readonly HelpSourceDescriptor[];
};

export type HelpSourceDecision = {
  readonly source_id: string;
  readonly allowed: boolean;
  readonly denied_because?: HelpDenyReason;
};

export type HelpDecisionReceipt = {
  readonly schema: typeof HELP_DECISION_RESPONSE_SCHEMA;
  readonly action: HelpAction;
  readonly principal_id: string;
  readonly tenant_id: string;
  readonly corpus_digest: string;
  readonly index_digest: string;
  readonly allowed_source_ids: readonly string[];
  readonly denied: readonly HelpSourceDecision[];
  readonly receipt_digest: string;
};

export type HelpDecisionResponse = {
  readonly schema: typeof HELP_DECISION_RESPONSE_SCHEMA;
  readonly allowed: boolean;
  readonly denied_because?: HelpDenyReason;
  readonly receipt: HelpDecisionReceipt;
};

/** Thrown when a payload cannot be parsed under the closed contract. */
export class HelpAuthorityMalformedError extends Error {
  constructor(detail: string) {
    super(`help authority: request could not be parsed: ${detail}`);
    this.name = "HelpAuthorityMalformedError";
  }
}

const VISIBILITIES: readonly string[] = ["public", "project", "private"];
const CAPABILITIES: readonly string[] = [
  "help_search",
  "help_search_project",
  "help_search_private",
  "help_answer",
];
const ACTIONS: readonly string[] = ["search", "answer", "read_source"];

const REQUEST_KEYS = new Set([
  "schema", "action", "principal", "corpus_digest", "index_digest", "sources",
]);
const PRINCIPAL_KEYS = new Set(["principal_id", "tenant_id", "project_ids", "capabilities"]);
const SOURCE_KEYS = new Set([
  "source_id", "visibility", "tenant_id", "project_id", "owner_principal_id", "digest",
]);

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function rejectUnknownKeys(value: Record<string, unknown>, allowed: Set<string>, where: string): void {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) throw new HelpAuthorityMalformedError(`unknown field ${where}.${key}`);
  }
}

function requireString(value: unknown, where: string): string {
  if (typeof value !== "string") throw new HelpAuthorityMalformedError(`${where} must be a string`);
  return value;
}

/**
 * Parse under the closed contract.
 *
 * Mirrors `deny_unknown_fields`: an unrecognized key or enum value throws
 * rather than being dropped, because a dropped `visibility` or capability
 * restriction turns default-deny into allow-by-omission.
 */
export function parseHelpDecisionRequest(payload: unknown): HelpDecisionRequest {
  const raw = typeof payload === "string" ? (JSON.parse(payload) as unknown) : payload;
  if (!isPlainObject(raw)) throw new HelpAuthorityMalformedError("request must be an object");
  rejectUnknownKeys(raw, REQUEST_KEYS, "request");

  const schema = requireString(raw.schema, "request.schema");
  const action = requireString(raw.action, "request.action");
  if (!ACTIONS.includes(action)) {
    throw new HelpAuthorityMalformedError(`unknown action ${action}`);
  }

  if (!isPlainObject(raw.principal)) {
    throw new HelpAuthorityMalformedError("request.principal must be an object");
  }
  rejectUnknownKeys(raw.principal, PRINCIPAL_KEYS, "principal");
  const principalId = requireString(raw.principal.principal_id, "principal.principal_id");
  const tenantId = requireString(raw.principal.tenant_id, "principal.tenant_id");

  const projectIds: string[] = [];
  if (raw.principal.project_ids !== undefined) {
    if (!Array.isArray(raw.principal.project_ids)) {
      throw new HelpAuthorityMalformedError("principal.project_ids must be an array");
    }
    for (const entry of raw.principal.project_ids) projectIds.push(requireString(entry, "project_ids[]"));
  }

  const capabilities: HelpCapability[] = [];
  if (raw.principal.capabilities !== undefined) {
    if (!Array.isArray(raw.principal.capabilities)) {
      throw new HelpAuthorityMalformedError("principal.capabilities must be an array");
    }
    for (const entry of raw.principal.capabilities) {
      const capability = requireString(entry, "capabilities[]");
      if (!CAPABILITIES.includes(capability)) {
        throw new HelpAuthorityMalformedError(`unknown capability ${capability}`);
      }
      capabilities.push(capability as HelpCapability);
    }
  }

  const sources: HelpSourceDescriptor[] = [];
  if (raw.sources !== undefined) {
    if (!Array.isArray(raw.sources)) {
      throw new HelpAuthorityMalformedError("request.sources must be an array");
    }
    for (const entry of raw.sources) {
      if (!isPlainObject(entry)) throw new HelpAuthorityMalformedError("source must be an object");
      rejectUnknownKeys(entry, SOURCE_KEYS, "source");
      const visibility = requireString(entry.visibility, "source.visibility");
      if (!VISIBILITIES.includes(visibility)) {
        throw new HelpAuthorityMalformedError(`unknown visibility ${visibility}`);
      }
      sources.push({
        source_id: requireString(entry.source_id, "source.source_id"),
        visibility: visibility as HelpVisibility,
        tenant_id: requireString(entry.tenant_id, "source.tenant_id"),
        project_id: entry.project_id === undefined ? undefined : requireString(entry.project_id, "source.project_id"),
        owner_principal_id:
          entry.owner_principal_id === undefined
            ? undefined
            : requireString(entry.owner_principal_id, "source.owner_principal_id"),
        digest: requireString(entry.digest, "source.digest"),
      });
    }
  }

  return {
    schema,
    action: action as HelpAction,
    principal: { principal_id: principalId, tenant_id: tenantId, project_ids: projectIds, capabilities },
    corpus_digest: requireString(raw.corpus_digest, "request.corpus_digest"),
    index_digest: requireString(raw.index_digest, "request.index_digest"),
    sources,
  };
}

/** UTF-8 byte length, matching the Rust bound which counts bytes. */
function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function idWithinBounds(value: string): boolean {
  return value.length > 0 && byteLength(value) <= HELP_MAX_ID_BYTES;
}

function requiredCapability(action: HelpAction): HelpCapability {
  return action === "answer" ? "help_answer" : "help_search";
}

function holds(principal: HelpPrincipal, capability: HelpCapability): boolean {
  return (principal.capabilities ?? []).includes(capability);
}

/** Decide one source. Mirrors `decide_source` in the Rust crate exactly. */
function decideSource(
  principal: HelpPrincipal,
  source: HelpSourceDescriptor,
): HelpDenyReason | null {
  if (!idWithinBounds(source.source_id) || !idWithinBounds(source.tenant_id)) return "bounds";

  if (source.visibility === "public") return null;

  // Tenant before scope, so a cross-tenant probe cannot learn a project exists.
  if (source.tenant_id !== principal.tenant_id) return "tenant_mismatch";

  if (source.visibility === "project") {
    if (!holds(principal, "help_search_project")) return "missing_capability";
    if (source.project_id === undefined) return "malformed_scope";
    if (!idWithinBounds(source.project_id)) return "bounds";
    return (principal.project_ids ?? []).includes(source.project_id) ? null : "scope_mismatch";
  }

  if (!holds(principal, "help_search_private")) return "missing_capability";
  if (source.owner_principal_id === undefined) return "malformed_scope";
  if (!idWithinBounds(source.owner_principal_id)) return "bounds";
  return source.owner_principal_id === principal.principal_id ? null : "scope_mismatch";
}

function buildReceipt(
  request: HelpDecisionRequest,
  allowedSourceIds: readonly string[],
  denied: readonly HelpSourceDecision[],
): HelpDecisionReceipt {
  return Object.freeze({
    schema: HELP_DECISION_RESPONSE_SCHEMA,
    action: request.action,
    principal_id: request.principal.principal_id,
    tenant_id: request.principal.tenant_id,
    corpus_digest: request.corpus_digest,
    index_digest: request.index_digest,
    allowed_source_ids: Object.freeze([...allowedSourceIds]),
    denied: Object.freeze(denied.map((decision) => Object.freeze({ ...decision }))),
    receipt_digest: domainDigest("grokptah.help.receipt.v1", [
      request.action,
      request.principal.principal_id,
      request.principal.tenant_id,
      request.corpus_digest,
      request.index_digest,
      ...allowedSourceIds,
      ...denied.map((decision) => decision.source_id),
    ]),
  });
}

function denyAll(request: HelpDecisionRequest, reason: HelpDenyReason): HelpDecisionResponse {
  const denied = (request.sources ?? [])
    .slice(0, HELP_MAX_SOURCES_PER_DECISION)
    .map((source) => ({ source_id: source.source_id, allowed: false, denied_because: reason }));
  return Object.freeze({
    schema: HELP_DECISION_RESPONSE_SCHEMA,
    allowed: false,
    denied_because: reason,
    receipt: buildReceipt(request, [], denied),
  });
}

/**
 * Authorize one Help action against the corpus and index actually being served.
 *
 * Mirrors `authorize` in the Rust crate. The served digests are what this
 * process really has; the request carries what the caller believes. A mismatch
 * denies rather than answering from a different corpus than the caller
 * reasoned about.
 */
export function authorizeHelpDecision(
  request: HelpDecisionRequest,
  servedCorpusDigest: string,
  servedIndexDigest: string,
): HelpDecisionResponse {
  if (request.schema !== HELP_DECISION_REQUEST_SCHEMA) return denyAll(request, "unknown_schema");

  const sources = request.sources ?? [];
  if (
    !idWithinBounds(request.principal.principal_id) ||
    !idWithinBounds(request.principal.tenant_id) ||
    sources.length > HELP_MAX_SOURCES_PER_DECISION
  ) {
    return denyAll(request, "bounds");
  }
  if (request.corpus_digest !== servedCorpusDigest || request.index_digest !== servedIndexDigest) {
    return denyAll(request, "stale_index");
  }
  if (!holds(request.principal, requiredCapability(request.action))) {
    return denyAll(request, "missing_capability");
  }

  const allowedSourceIds: string[] = [];
  const denied: HelpSourceDecision[] = [];
  for (const source of sources) {
    const reason = decideSource(request.principal, source);
    if (reason === null) allowedSourceIds.push(source.source_id);
    else denied.push({ source_id: source.source_id, allowed: false, denied_because: reason });
  }

  // A read of exactly one source is meaningless if that source was denied.
  if (request.action === "read_source" && allowedSourceIds.length === 0) {
    const reason = denied[0]?.denied_because ?? "scope_mismatch";
    return Object.freeze({
      schema: HELP_DECISION_RESPONSE_SCHEMA,
      allowed: false,
      denied_because: reason,
      receipt: buildReceipt(request, [], denied),
    });
  }

  return Object.freeze({
    schema: HELP_DECISION_RESPONSE_SCHEMA,
    allowed: true,
    receipt: buildReceipt(request, allowedSourceIds, denied),
  });
}

/** Parse then authorize, matching the Rust `authorize_json` entry point. */
export function authorizeHelpDecisionJson(
  payload: string,
  servedCorpusDigest: string,
  servedIndexDigest: string,
): HelpDecisionResponse {
  return authorizeHelpDecision(
    parseHelpDecisionRequest(payload),
    servedCorpusDigest,
    servedIndexDigest,
  );
}
