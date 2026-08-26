import { HELP_CORPUS_DIGEST, getHelpArticle, getHelpChunk } from "../canonical/corpus";
import { canonicalDigest, canonicalJson, sha256Hex } from "../canonical/digest";
import { containsHelpSecret, redactHelpText } from "../retrieval/redact";
import { sanitizeHelpText } from "../retrieval/highlight";
import type { HelpRetrievalResult } from "../retrieval/hybrid";
import {
  HELP_AUTHORITY_IDENTITY,
  getHelpSourceBinding,
  helpSourceBindings,
  type HelpAuthorityIdentity,
  type HelpSourceBinding,
} from "./sourceIdentity";

export const HELP_AUTHORITY_SCHEMA = "grokptah.help-authority.v1" as const;

/**
 * These are wire ceilings, not hints. The byte checks below use UTF-8, while
 * the JSON schema expresses the corresponding structural/count constraints.
 */
export const HELP_AUTHORITY_LIMITS = Object.freeze({
  maxRequestBytes: 32_768,
  maxResponseBytes: 32_768,
  maxCleanupBytes: 8_192,
  maxRequestIdBytes: 256,
  maxQueryBytes: 512,
  maxContextChunks: 8,
  maxContextTextBytes: 512,
  maxSourceBindings: 8,
  maxClaims: 32,
  maxClaimTextBytes: 1_024,
  maxCitations: 16,
  maxQuotedTextBytes: 512,
  maxUncertaintyBytes: 1_024,
  maxDurationMs: 20_000,
  maxCapabilities: 64,
});

export type HelpAuthorityAccessMode = "public" | "authorized";
export type HelpAuthorityDialect =
  | "openai_chat"
  | "openai_responses"
  | "anthropic_messages"
  | "broker_native";

export type HelpAuthorityAuthorization = {
  readonly mode: HelpAuthorityAccessMode;
  readonly authorizedCapabilities: readonly string[];
};

export type HelpAuthorityProvider = {
  readonly profile: string;
  readonly tenant: string;
  readonly model: string;
  readonly routeRevision: string;
  readonly dialect: HelpAuthorityDialect;
};

export type HelpAuthorityDeadline = {
  readonly deadlineAt: string;
  readonly maxDurationMs: number;
};

export type HelpAuthorityContextChunk = {
  readonly chunkId: string;
  readonly articleId: string;
  readonly access: "public" | "gated" | "operator";
  readonly requiredCapabilities: readonly string[];
  readonly text: string;
  readonly textDigest: string;
  /** UTF-8 byte offsets over `text`; the initial context span covers all text. */
  readonly spanStart: number;
  readonly spanEnd: number;
  readonly sourceBindings: readonly HelpSourceBinding[];
};

export type HelpAuthorityRequest = {
  readonly schema: typeof HELP_AUTHORITY_SCHEMA;
  readonly kind: "request";
  readonly requestId: string;
  readonly authorization: HelpAuthorityAuthorization;
  readonly identity: HelpAuthorityIdentity;
  readonly provider: HelpAuthorityProvider;
  readonly deadline: HelpAuthorityDeadline;
  readonly query: string;
  readonly context: readonly HelpAuthorityContextChunk[];
  readonly toolsDisabled: true;
  readonly conversationDisabled: true;
};

export type HelpAuthorityClaim = {
  readonly claimId: string;
  readonly text: string;
  /** UTF-8 byte offsets over the complete answer. */
  readonly spanStart: number;
  readonly spanEnd: number;
  readonly citationIds: readonly string[];
};

export type HelpAuthorityCitation = {
  readonly citationId: string;
  readonly chunkId: string;
  readonly articleId: string;
  /** UTF-8 byte offsets over the cited context chunk. */
  readonly spanStart: number;
  readonly spanEnd: number;
  readonly quotedText: string;
  readonly quotedTextHash: string;
  readonly sourceId: string;
  readonly sourceSectionDigest: string;
  readonly claimIds: readonly string[];
};

export type HelpAuthorityArtifactCounts = {
  readonly chat: 0;
  readonly session: 0;
  readonly transcript: 0;
  readonly tool: 0;
  readonly workspace: 0;
};

export type HelpAuthorityCleanupReceipt = {
  readonly schema: typeof HELP_AUTHORITY_SCHEMA;
  readonly kind: "cleanup";
  readonly requestId: string;
  readonly status: "finalized" | "uncertain";
  readonly providerTask: "joined" | "not_joined";
  readonly abortRequested: boolean;
  readonly queueSlot: "released" | "not_released";
  readonly artifactCounts: HelpAuthorityArtifactCounts;
};

export type HelpAuthorityResponse = {
  readonly schema: typeof HELP_AUTHORITY_SCHEMA;
  readonly kind: "response";
  readonly requestId: string;
  readonly identity: HelpAuthorityIdentity;
  readonly provider: HelpAuthorityProvider;
  readonly deadline: HelpAuthorityDeadline;
  readonly answer: string;
  readonly claims: readonly HelpAuthorityClaim[];
  readonly citations: readonly HelpAuthorityCitation[];
  readonly uncertainty: string;
  readonly cleanup: HelpAuthorityCleanupReceipt;
};

export type HelpAuthorityRejection =
  | "not-an-object"
  | "unknown-key"
  | "wrong-kind"
  | "wrong-schema"
  | "oversized"
  | "invalid-field"
  | "secret"
  | "stale-identity"
  | "unauthorized-access"
  | "invalid-context"
  | "invalid-citation"
  | "unsupported-claim"
  | "invalid-cleanup";

export type HelpAuthorityValidation =
  | { readonly accepted: true; readonly value: HelpAuthorityRequest | HelpAuthorityResponse | HelpAuthorityCleanupReceipt }
  | { readonly accepted: false; readonly reason: HelpAuthorityRejection; readonly detail: string };

const ACCESS_KEYS = new Set(["mode", "authorizedCapabilities"]);
const IDENTITY_KEYS = new Set(["corpusDigest", "sourceDigest", "modelDigest", "modelId", "modelVersion"]);
const PROVIDER_KEYS = new Set(["profile", "tenant", "model", "routeRevision", "dialect"]);
const DEADLINE_KEYS = new Set(["deadlineAt", "maxDurationMs"]);
const SOURCE_BINDING_KEYS = new Set(["sourceId", "sourceSectionDigest", "sourceByteLength"]);
const CONTEXT_KEYS = new Set([
  "chunkId",
  "articleId",
  "access",
  "requiredCapabilities",
  "text",
  "textDigest",
  "spanStart",
  "spanEnd",
  "sourceBindings",
]);
const REQUEST_KEYS = new Set([
  "schema",
  "kind",
  "requestId",
  "authorization",
  "identity",
  "provider",
  "deadline",
  "query",
  "context",
  "toolsDisabled",
  "conversationDisabled",
]);
const CLAIM_KEYS = new Set(["claimId", "text", "spanStart", "spanEnd", "citationIds"]);
const CITATION_KEYS = new Set([
  "citationId",
  "chunkId",
  "articleId",
  "spanStart",
  "spanEnd",
  "quotedText",
  "quotedTextHash",
  "sourceId",
  "sourceSectionDigest",
  "claimIds",
]);
const ARTIFACT_KEYS = new Set(["chat", "session", "transcript", "tool", "workspace"]);
const CLEANUP_KEYS = new Set([
  "schema",
  "kind",
  "requestId",
  "status",
  "providerTask",
  "abortRequested",
  "queueSlot",
  "artifactCounts",
]);
const RESPONSE_KEYS = new Set([
  "schema",
  "kind",
  "requestId",
  "identity",
  "provider",
  "deadline",
  "answer",
  "claims",
  "citations",
  "uncertainty",
  "cleanup",
]);
const CAPABILITY_ID = /^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$/u;
const DIGEST = /^sha256:[0-9a-f]{64}$/u;
const MARKUP = /<\s*\/?\s*[a-z][^>]*>|<!--|javascript:|data:text\/html/i;
const PRIVILEGED = /(?:\/(?:users|private|var|tmp|home|volumes)\/|https?:\/\/|(?:^|[\s=:])(authorization|bearer|api[_ -]?key|xai_api_key|grokptah_home|clipboard|private[_ -]?key|secret(?:[_ -]?key)?)(?:[\s=:]|$))/i;
const DIALECTS: ReadonlySet<HelpAuthorityDialect> = new Set([
  "openai_chat",
  "openai_responses",
  "anthropic_messages",
  "broker_native",
]);

const ZERO_ARTIFACTS: HelpAuthorityArtifactCounts = Object.freeze({
  chat: 0,
  session: 0,
  transcript: 0,
  tool: 0,
  workspace: 0,
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOnlyKeys(value: Record<string, unknown>, keys: ReadonlySet<string>): boolean {
  return Object.keys(value).every((key) => keys.has(key));
}

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function jsonBytes(value: unknown): number | null {
  try {
    const serialized = JSON.stringify(value);
    return serialized === undefined ? null : utf8Bytes(serialized);
  } catch {
    return null;
  }
}

function digest(value: string): string {
  return `sha256:${sha256Hex(value)}`;
}

function validBoundedString(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.trim().length > 0 && utf8Bytes(value) <= maxBytes;
}

function validSafeString(value: unknown, maxBytes: number): value is string {
  return validBoundedString(value, maxBytes) && !containsHelpSecret(value) && !PRIVILEGED.test(value);
}

function validOpaqueString(value: unknown, maxBytes: number): value is string {
  return validBoundedString(value, maxBytes) && !PRIVILEGED.test(value) && !/(?:xai-|sk-|api[_ -]?key|private key)/i.test(value);
}

function validDigestString(value: unknown): value is string {
  return validBoundedString(value, 71) && DIGEST.test(value);
}

function reject(reason: HelpAuthorityRejection, detail: string): HelpAuthorityValidation {
  return { accepted: false, reason, detail };
}

function equal(left: unknown, right: unknown): boolean {
  return canonicalJson(left) === canonicalJson(right);
}

function validSpan(value: unknown, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= max;
}

function sliceUtf8(value: string, start: number, end: number): string | null {
  const bytes = new TextEncoder().encode(value);
  if (!validSpan(start, bytes.length) || !validSpan(end, bytes.length) || end <= start) return null;
  const decoder = new TextDecoder("utf-8", { fatal: true });
  try {
    return decoder.decode(bytes.slice(start, end));
  } catch {
    return null;
  }
}

function containsNonWhitespace(value: string): boolean {
  return value.trim().length > 0;
}

function validCapabilitySet(value: unknown): value is string[] {
  return (
    Array.isArray(value) &&
    value.length <= HELP_AUTHORITY_LIMITS.maxCapabilities &&
    new Set(value).size === value.length &&
    value.every(
      (capability) =>
        validOpaqueString(capability, 128) && CAPABILITY_ID.test(capability),
    )
  );
}

/** Normalize access so a missing/empty capability set can never widen access. */
export function createHelpAuthorization(
  authorizedCapabilities?: readonly string[],
): HelpAuthorityAuthorization {
  const capabilities = authorizedCapabilities === undefined ? [] : [...authorizedCapabilities];
  if (!validCapabilitySet(capabilities)) {
    throw new Error("help authority: authorized capability set is invalid");
  }
  return Object.freeze({
    mode: capabilities.length > 0 ? "authorized" : "public",
    authorizedCapabilities: Object.freeze(capabilities.sort()),
  });
}

/** Public articles are always readable; every non-public article needs all of its declared gates. */
export function helpArticleIsAuthorized(
  articleId: string,
  authorization: HelpAuthorityAuthorization,
): boolean {
  const article = getHelpArticle(articleId);
  if (!article) return false;
  if (article.access === "public") return true;
  if (authorization.mode !== "authorized" || authorization.authorizedCapabilities.length === 0) return false;
  const capabilities = new Set(authorization.authorizedCapabilities);
  return (
    article.capabilityIds.length > 0 &&
    article.capabilityIds.every((capability) => capabilities.has(capability))
  );
}

function buildDeadline(
  deadlineAt: string | undefined,
  maxDurationMs: number | undefined,
): HelpAuthorityDeadline {
  const duration = maxDurationMs ?? HELP_AUTHORITY_LIMITS.maxDurationMs;
  if (!Number.isSafeInteger(duration) || duration < 1 || duration > HELP_AUTHORITY_LIMITS.maxDurationMs) {
    throw new Error("help authority: deadline exceeds the hard limit");
  }
  const at = deadlineAt ?? new Date(Date.now() + duration).toISOString();
  if (!validSafeString(at, 64) || Number.isNaN(Date.parse(at))) {
    throw new Error("help authority: deadlineAt is invalid");
  }
  return Object.freeze({ deadlineAt: at, maxDurationMs: duration });
}

export type HelpAuthorityRequestOptions = {
  readonly requestId: string;
  readonly query: string;
  readonly results: readonly HelpRetrievalResult[];
  readonly provider: HelpAuthorityProvider;
  readonly deadlineAt?: string;
  readonly maxDurationMs?: number;
  readonly authorizedCapabilities?: readonly string[];
};

function contextFromResults(
  results: readonly HelpRetrievalResult[],
  authorization: HelpAuthorityAuthorization,
): HelpAuthorityContextChunk[] {
  const seen = new Set<string>();
  const context: HelpAuthorityContextChunk[] = [];
  for (const result of results) {
    if (context.length >= HELP_AUTHORITY_LIMITS.maxContextChunks) break;
    if (seen.has(result.chunkId) || !helpArticleIsAuthorized(result.articleId, authorization)) continue;
    const chunk = getHelpChunk(result.chunkId);
    if (!chunk || chunk.articleId !== result.articleId || chunk.text.length === 0) continue;
    const sourceBindings = helpSourceBindings(chunk.sourceIds);
    const text = sanitizeHelpText(chunk.text, HELP_AUTHORITY_LIMITS.maxContextTextBytes);
    const textBytes = utf8Bytes(text);
    if (textBytes === 0 || textBytes > HELP_AUTHORITY_LIMITS.maxContextTextBytes) continue;
    seen.add(result.chunkId);
    context.push(
      Object.freeze({
        chunkId: chunk.id,
        articleId: chunk.articleId,
        access: getHelpArticle(chunk.articleId)!.access,
        requiredCapabilities: Object.freeze([...getHelpArticle(chunk.articleId)!.capabilityIds]),
        text,
        textDigest: digest(text),
        spanStart: 0,
        spanEnd: textBytes,
        sourceBindings,
      }),
    );
  }
  return context;
}

/** Build the only request shape accepted by either a Tauri or broker executor. */
export function buildHelpAuthorityRequest(
  options: HelpAuthorityRequestOptions,
): HelpAuthorityRequest {
  if (!validOpaqueString(options.requestId, HELP_AUTHORITY_LIMITS.maxRequestIdBytes)) {
    throw new Error("help authority: requestId is invalid");
  }
  const redacted = redactHelpText(typeof options.query === "string" ? options.query : "").text.trim();
  const query = sanitizeHelpText(redacted, HELP_AUTHORITY_LIMITS.maxQueryBytes);
  if (!query || utf8Bytes(query) > HELP_AUTHORITY_LIMITS.maxQueryBytes) {
    throw new Error("help authority: query is empty or oversized");
  }
  if (
    !isRecord(options.provider) ||
    !validOpaqueString(options.provider.profile, 256) ||
    !validOpaqueString(options.provider.tenant, 256) ||
    !validOpaqueString(options.provider.model, 256) ||
    !validOpaqueString(options.provider.routeRevision, 256) ||
    !DIALECTS.has(options.provider.dialect)
  ) {
    throw new Error("help authority: provider identity is invalid");
  }
  const authorization = createHelpAuthorization(options.authorizedCapabilities);
  const context = contextFromResults(options.results, authorization);
  if (context.length === 0) throw new Error("help authority: no authorized cited context");
  const request: HelpAuthorityRequest = Object.freeze({
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "request",
    requestId: options.requestId,
    authorization,
    identity: HELP_AUTHORITY_IDENTITY,
    provider: Object.freeze({ ...options.provider }),
    deadline: buildDeadline(options.deadlineAt, options.maxDurationMs),
    query,
    context: Object.freeze(context),
    toolsDisabled: true,
    conversationDisabled: true,
  });
  const validation = validateHelpAuthorityRequest(request);
  if (!validation.accepted) throw new Error(`${validation.reason}: ${validation.detail}`);
  if (utf8Bytes(JSON.stringify(request)) > HELP_AUTHORITY_LIMITS.maxRequestBytes) {
    throw new Error("help authority: request exceeds the byte ceiling");
  }
  return request;
}

function parseAuthorization(value: unknown): HelpAuthorityValidation | HelpAuthorityAuthorization {
  if (!isRecord(value) || !hasOnlyKeys(value, ACCESS_KEYS)) return reject("invalid-field", "authorization");
  if (
    (value.mode !== "public" && value.mode !== "authorized") ||
    !validCapabilitySet(value.authorizedCapabilities)
  ) {
    return reject("invalid-field", "authorization fields");
  }
  if (
    (value.mode === "public" && value.authorizedCapabilities.length !== 0) ||
    (value.mode === "authorized" && value.authorizedCapabilities.length === 0)
  ) {
    return reject("invalid-field", "authorization mode does not match its capability set");
  }
  return Object.freeze({
    mode: value.mode,
    authorizedCapabilities: Object.freeze([...value.authorizedCapabilities].sort()),
  });
}

function parseIdentity(value: unknown): HelpAuthorityValidation | HelpAuthorityIdentity {
  if (!isRecord(value) || !hasOnlyKeys(value, IDENTITY_KEYS)) return reject("invalid-field", "identity");
  if (
    !validDigestString(value.corpusDigest) ||
    !validDigestString(value.sourceDigest) ||
    !validDigestString(value.modelDigest) ||
    !validOpaqueString(value.modelId, 256) ||
    !validOpaqueString(value.modelVersion, 256)
  ) {
    return reject("invalid-field", "identity fields");
  }
  const identity = {
    corpusDigest: value.corpusDigest,
    sourceDigest: value.sourceDigest,
    modelDigest: value.modelDigest,
    modelId: value.modelId,
    modelVersion: value.modelVersion,
  };
  if (!equal(identity, HELP_AUTHORITY_IDENTITY)) return reject("stale-identity", "identity is not this shipped build");
  return Object.freeze(identity);
}

function parseProvider(value: unknown): HelpAuthorityValidation | HelpAuthorityProvider {
  if (!isRecord(value) || !hasOnlyKeys(value, PROVIDER_KEYS)) return reject("invalid-field", "provider");
  if (
    !validOpaqueString(value.profile, 256) ||
    !validOpaqueString(value.tenant, 256) ||
    !validOpaqueString(value.model, 256) ||
    !validOpaqueString(value.routeRevision, 256) ||
    typeof value.dialect !== "string" ||
    !DIALECTS.has(value.dialect as HelpAuthorityDialect)
  ) {
    return reject("invalid-field", "provider fields");
  }
  return Object.freeze({
    profile: value.profile,
    tenant: value.tenant,
    model: value.model,
    routeRevision: value.routeRevision,
    dialect: value.dialect as HelpAuthorityDialect,
  });
}

function parseDeadline(value: unknown): HelpAuthorityValidation | HelpAuthorityDeadline {
  if (!isRecord(value) || !hasOnlyKeys(value, DEADLINE_KEYS)) return reject("invalid-field", "deadline");
  if (
    !validSafeString(value.deadlineAt, 64) ||
    Number.isNaN(Date.parse(value.deadlineAt)) ||
    typeof value.maxDurationMs !== "number" ||
    !Number.isSafeInteger(value.maxDurationMs) ||
    value.maxDurationMs < 1 ||
    value.maxDurationMs > HELP_AUTHORITY_LIMITS.maxDurationMs
  ) {
    return reject("invalid-field", "deadline fields");
  }
  return Object.freeze({ deadlineAt: value.deadlineAt, maxDurationMs: value.maxDurationMs });
}

function parseSourceBinding(value: unknown): HelpAuthorityValidation | HelpSourceBinding {
  if (!isRecord(value) || !hasOnlyKeys(value, SOURCE_BINDING_KEYS)) return reject("invalid-context", "source binding shape");
  if (
    !validOpaqueString(value.sourceId, 256) ||
    !validDigestString(value.sourceSectionDigest) ||
    typeof value.sourceByteLength !== "number" ||
    !Number.isSafeInteger(value.sourceByteLength) ||
    value.sourceByteLength < 1 ||
    value.sourceByteLength > 1_048_576
  ) {
    return reject("invalid-context", "source binding fields");
  }
  try {
    const expected = getHelpSourceBinding(value.sourceId);
    if (!equal(expected, value)) return reject("invalid-context", `source bytes do not match ${value.sourceId}`);
  } catch {
    return reject("invalid-context", `unknown source ${value.sourceId}`);
  }
  return Object.freeze({
    sourceId: value.sourceId,
    sourceSectionDigest: value.sourceSectionDigest,
    sourceByteLength: value.sourceByteLength,
  });
}

function parseContext(value: unknown): HelpAuthorityValidation | HelpAuthorityContextChunk[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > HELP_AUTHORITY_LIMITS.maxContextChunks) {
    return reject("invalid-context", "context count");
  }
  const chunks: HelpAuthorityContextChunk[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item) || !hasOnlyKeys(item, CONTEXT_KEYS)) return reject("invalid-context", "context chunk shape");
    if (
      !validOpaqueString(item.chunkId, 256) ||
      !validOpaqueString(item.articleId, 256) ||
      (item.access !== "public" && item.access !== "gated" && item.access !== "operator") ||
      !validCapabilitySet(item.requiredCapabilities) ||
      !validSafeString(item.text, HELP_AUTHORITY_LIMITS.maxContextTextBytes) ||
      !validDigestString(item.textDigest) ||
      !validSpan(item.spanStart, 4096) ||
      !validSpan(item.spanEnd, 4096) ||
      item.spanEnd <= item.spanStart ||
      !Array.isArray(item.sourceBindings) ||
      item.sourceBindings.length === 0 ||
      item.sourceBindings.length > HELP_AUTHORITY_LIMITS.maxSourceBindings ||
      seen.has(item.chunkId)
    ) {
      return reject("invalid-context", "context chunk fields");
    }
    const chunk = getHelpChunk(item.chunkId);
    const article = getHelpArticle(item.articleId);
    if (
      !chunk ||
      !article ||
      chunk.articleId !== item.articleId ||
      article.access !== item.access ||
      !equal(article.capabilityIds, item.requiredCapabilities)
    ) {
      return reject("unauthorized-access", item.articleId);
    }
    const textBytes = utf8Bytes(item.text);
    if (
      item.spanStart !== 0 ||
      item.spanEnd !== textBytes ||
      item.textDigest !== digest(item.text) ||
      item.text !== sanitizeHelpText(chunk.text, HELP_AUTHORITY_LIMITS.maxContextTextBytes)
    ) {
      return reject("invalid-context", `chunk bytes do not match ${item.chunkId}`);
    }
    const bindings: HelpSourceBinding[] = [];
    for (const binding of item.sourceBindings) {
      const parsed = parseSourceBinding(binding);
      if ("accepted" in parsed) {
        if (!parsed.accepted) return parsed;
      } else bindings.push(parsed);
    }
    const expectedSources = new Set(chunk.sourceIds);
    if (
      bindings.length !== expectedSources.size ||
      bindings.some((binding) => !expectedSources.has(binding.sourceId))
    ) {
      return reject("invalid-context", `source bindings do not cover ${item.chunkId}`);
    }
    seen.add(item.chunkId);
    chunks.push(Object.freeze({
      chunkId: item.chunkId,
      articleId: item.articleId,
      access: item.access,
      requiredCapabilities: Object.freeze([...item.requiredCapabilities]),
      text: item.text,
      textDigest: item.textDigest,
      spanStart: item.spanStart,
      spanEnd: item.spanEnd,
      sourceBindings: Object.freeze(bindings),
    }));
  }
  return chunks;
}

function parseCleanup(value: unknown, maxBytes: number): HelpAuthorityValidation | HelpAuthorityCleanupReceipt {
  if (!isRecord(value) || !hasOnlyKeys(value, CLEANUP_KEYS)) return reject("invalid-cleanup", "cleanup shape");
  if (
    (jsonBytes(value) === null || (jsonBytes(value) as number) > maxBytes) ||
    value.schema !== HELP_AUTHORITY_SCHEMA ||
    value.kind !== "cleanup" ||
    !validOpaqueString(value.requestId, HELP_AUTHORITY_LIMITS.maxRequestIdBytes) ||
    (value.status !== "finalized" && value.status !== "uncertain") ||
    (value.providerTask !== "joined" && value.providerTask !== "not_joined") ||
    typeof value.abortRequested !== "boolean" ||
    (value.queueSlot !== "released" && value.queueSlot !== "not_released") ||
    !isRecord(value.artifactCounts) ||
    !hasOnlyKeys(value.artifactCounts, ARTIFACT_KEYS) ||
    value.artifactCounts.chat !== 0 ||
    value.artifactCounts.session !== 0 ||
    value.artifactCounts.transcript !== 0 ||
    value.artifactCounts.tool !== 0 ||
    value.artifactCounts.workspace !== 0
  ) {
    return reject("invalid-cleanup", "cleanup is incomplete or uncertain");
  }
  return Object.freeze({
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "cleanup",
    requestId: value.requestId,
    status: value.status,
    providerTask: value.providerTask,
    abortRequested: value.abortRequested,
    queueSlot: value.queueSlot,
    artifactCounts: ZERO_ARTIFACTS,
  });
}

function claimSupportedByCitations(
  claim: HelpAuthorityClaim,
  citations: readonly HelpAuthorityCitation[],
): boolean {
  const quoted = citations
    .filter((citation) => claim.citationIds.includes(citation.citationId))
    .map((citation) => citation.quotedText.toLocaleLowerCase())
    .join(" ");
  const terms = claim.text
    .toLocaleLowerCase()
    .split(/[^\p{L}\p{N}]+/gu)
    .filter((term) => term.length >= 3);
  return terms.length > 0 && terms.every((term) => quoted.includes(term));
}

function parseClaims(
  value: unknown,
  answer: string,
  citations: readonly HelpAuthorityCitation[],
): HelpAuthorityValidation | HelpAuthorityClaim[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > HELP_AUTHORITY_LIMITS.maxClaims) {
    return reject("unsupported-claim", "claims count");
  }
  const claims: HelpAuthorityClaim[] = [];
  const ids = new Set<string>();
  let cursor = 0;
  const answerBytes = utf8Bytes(answer);
  for (const item of value) {
    if (
      !isRecord(item) ||
      !hasOnlyKeys(item, CLAIM_KEYS) ||
      !validOpaqueString(item.claimId, 256) ||
      ids.has(item.claimId) ||
      !validSafeString(item.text, HELP_AUTHORITY_LIMITS.maxClaimTextBytes) ||
      !validSpan(item.spanStart, answerBytes) ||
      !validSpan(item.spanEnd, answerBytes) ||
      item.spanEnd <= item.spanStart ||
      !Array.isArray(item.citationIds) ||
      item.citationIds.length === 0 ||
      item.citationIds.length > HELP_AUTHORITY_LIMITS.maxCitations ||
      new Set(item.citationIds).size !== item.citationIds.length ||
      !item.citationIds.every((id) => validOpaqueString(id, 256))
    ) {
      return reject("unsupported-claim", "claim fields");
    }
    const expectedText = sliceUtf8(answer, item.spanStart, item.spanEnd);
    if (expectedText !== item.text) return reject("unsupported-claim", `claim ${item.claimId} span mismatch`);
    const gap = sliceUtf8(answer, cursor, item.spanStart);
    if (cursor > item.spanStart || (gap !== null && containsNonWhitespace(gap))) {
      return reject("unsupported-claim", `answer text before ${item.claimId} is uncited`);
    }
    const claim: HelpAuthorityClaim = Object.freeze({
      claimId: item.claimId,
      text: item.text,
      spanStart: item.spanStart,
      spanEnd: item.spanEnd,
      citationIds: Object.freeze([...item.citationIds]),
    });
    if (!claimSupportedByCitations(claim, citations)) {
      return reject("unsupported-claim", `claim ${claim.claimId} is not supported by its quoted text`);
    }
    ids.add(claim.claimId);
    cursor = item.spanEnd;
    claims.push(claim);
  }
  const tail = sliceUtf8(answer, cursor, answerBytes);
  if (tail !== null && containsNonWhitespace(tail)) return reject("unsupported-claim", "answer has uncited trailing text");
  return claims;
}

function parseCitations(
  value: unknown,
  context: readonly HelpAuthorityContextChunk[],
): HelpAuthorityValidation | HelpAuthorityCitation[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > HELP_AUTHORITY_LIMITS.maxCitations) {
    return reject("invalid-citation", "citation count");
  }
  const chunks = new Map(context.map((chunk) => [chunk.chunkId, chunk]));
  const citations: HelpAuthorityCitation[] = [];
  const ids = new Set<string>();
  for (const item of value) {
    if (
      !isRecord(item) ||
      !hasOnlyKeys(item, CITATION_KEYS) ||
      !validOpaqueString(item.citationId, 256) ||
      ids.has(item.citationId) ||
      !validOpaqueString(item.chunkId, 256) ||
      !validOpaqueString(item.articleId, 256) ||
      !validSpan(item.spanStart, 4096) ||
      !validSpan(item.spanEnd, 4096) ||
      item.spanEnd <= item.spanStart ||
      !validSafeString(item.quotedText, HELP_AUTHORITY_LIMITS.maxQuotedTextBytes) ||
      !validDigestString(item.quotedTextHash) ||
      !validOpaqueString(item.sourceId, 256) ||
      !validDigestString(item.sourceSectionDigest) ||
      !Array.isArray(item.claimIds) ||
      item.claimIds.length === 0 ||
      item.claimIds.length > HELP_AUTHORITY_LIMITS.maxClaims ||
      new Set(item.claimIds).size !== item.claimIds.length ||
      !item.claimIds.every((id) => validOpaqueString(id, 256))
    ) {
      return reject("invalid-citation", "citation fields");
    }
    const chunk = chunks.get(item.chunkId);
    if (!chunk || chunk.articleId !== item.articleId) {
      return reject("invalid-citation", `citation ${item.citationId} is outside context`);
    }
    const expectedQuote = sliceUtf8(chunk.text, item.spanStart, item.spanEnd);
    if (
      expectedQuote !== item.quotedText ||
      item.quotedTextHash !== digest(item.quotedText) ||
      !chunk.sourceBindings.some(
        (binding) =>
          binding.sourceId === item.sourceId &&
          binding.sourceSectionDigest === item.sourceSectionDigest,
      )
    ) {
      return reject("invalid-citation", `citation ${item.citationId} does not bind chunk/source bytes`);
    }
    ids.add(item.citationId);
    citations.push(Object.freeze({
      citationId: item.citationId,
      chunkId: item.chunkId,
      articleId: item.articleId,
      spanStart: item.spanStart,
      spanEnd: item.spanEnd,
      quotedText: item.quotedText,
      quotedTextHash: item.quotedTextHash,
      sourceId: item.sourceId,
      sourceSectionDigest: item.sourceSectionDigest,
      claimIds: Object.freeze([...item.claimIds]),
    }));
  }
  return citations;
}

/** Validate and normalize a request before any transport is called. */
export function validateHelpAuthorityRequest(raw: unknown): HelpAuthorityValidation {
  if (!isRecord(raw)) return reject("not-an-object", typeof raw);
  if (!hasOnlyKeys(raw, REQUEST_KEYS)) return reject("unknown-key", Object.keys(raw).join(","));
  const requestBytes = jsonBytes(raw);
  if (requestBytes === null || requestBytes > HELP_AUTHORITY_LIMITS.maxRequestBytes) return reject("oversized", "request");
  if (raw.schema !== HELP_AUTHORITY_SCHEMA) return reject("wrong-schema", String(raw.schema));
  if (raw.kind !== "request") return reject("wrong-kind", String(raw.kind));
  if (!validOpaqueString(raw.requestId, HELP_AUTHORITY_LIMITS.maxRequestIdBytes)) return reject("invalid-field", "requestId");
  const authorization = parseAuthorization(raw.authorization);
  if ("accepted" in authorization) return authorization;
  const identity = parseIdentity(raw.identity);
  if ("accepted" in identity) return identity;
  const provider = parseProvider(raw.provider);
  if ("accepted" in provider) return provider;
  const deadline = parseDeadline(raw.deadline);
  if ("accepted" in deadline) return deadline;
  if (
    !validSafeString(raw.query, HELP_AUTHORITY_LIMITS.maxQueryBytes) ||
    raw.toolsDisabled !== true ||
    raw.conversationDisabled !== true
  ) {
    return reject("invalid-field", "query or authority flags");
  }
  const context = parseContext(raw.context);
  if ("accepted" in context) return context;
  for (const chunk of context) {
    const article = getHelpArticle(chunk.articleId);
    if (article && article.access !== "public" && authorization.mode !== "authorized") {
      return reject("unauthorized-access", chunk.articleId);
    }
    if (
      article &&
      article.access !== "public" &&
      !article.capabilityIds.every((capability) => authorization.authorizedCapabilities.includes(capability))
    ) {
      return reject("unauthorized-access", chunk.articleId);
    }
  }
  const value: HelpAuthorityRequest = Object.freeze({
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "request",
    requestId: raw.requestId,
    authorization,
    identity,
    provider,
    deadline,
    query: raw.query,
    context: Object.freeze(context),
    toolsDisabled: true,
    conversationDisabled: true,
  });
  return { accepted: true, value };
}

/** Validate a finalization receipt independently of a response. */
export function validateHelpAuthorityCleanup(
  raw: unknown,
): HelpAuthorityValidation {
  const cleanup = parseCleanup(raw, HELP_AUTHORITY_LIMITS.maxCleanupBytes);
  if ("accepted" in cleanup) return cleanup;
  return { accepted: true, value: cleanup };
}

/** Validate a response against the exact request that produced it. */
export function validateHelpAuthorityResponse(
  raw: unknown,
  request: HelpAuthorityRequest,
): HelpAuthorityValidation {
  if (!isRecord(raw)) return reject("not-an-object", typeof raw);
  if (!hasOnlyKeys(raw, RESPONSE_KEYS)) return reject("unknown-key", Object.keys(raw).join(","));
  const responseBytes = jsonBytes(raw);
  if (responseBytes === null || responseBytes > HELP_AUTHORITY_LIMITS.maxResponseBytes) return reject("oversized", "response");
  if (raw.schema !== HELP_AUTHORITY_SCHEMA) return reject("wrong-schema", String(raw.schema));
  if (raw.kind !== "response") return reject("wrong-kind", String(raw.kind));
  if (!validOpaqueString(raw.requestId, HELP_AUTHORITY_LIMITS.maxRequestIdBytes) || raw.requestId !== request.requestId) {
    return reject("invalid-field", "requestId");
  }
  const identity = parseIdentity(raw.identity);
  if ("accepted" in identity) return identity;
  const provider = parseProvider(raw.provider);
  if ("accepted" in provider) return provider;
  const deadline = parseDeadline(raw.deadline);
  if ("accepted" in deadline) return deadline;
  if (!equal(identity, request.identity) || !equal(provider, request.provider) || !equal(deadline, request.deadline)) {
    return reject("stale-identity", "response route or deadline differs from request");
  }
  if (
    !validSafeString(raw.answer, 4_096) ||
    MARKUP.test(raw.answer) ||
    !validSafeString(raw.uncertainty, HELP_AUTHORITY_LIMITS.maxUncertaintyBytes) ||
    MARKUP.test(raw.uncertainty)
  ) {
    return reject("invalid-field", "answer or uncertainty");
  }
  const citations = parseCitations(raw.citations, request.context);
  if ("accepted" in citations) return citations;
  const claims = parseClaims(raw.claims, raw.answer, citations);
  if ("accepted" in claims) return claims;
  const claimIds = new Set(claims.map((claim) => claim.claimId));
  for (const citation of citations) {
    if (citation.claimIds.some((claimId) => !claimIds.has(claimId))) {
      return reject("unsupported-claim", `citation ${citation.citationId} names an unknown claim`);
    }
  }
  for (const claim of claims) {
    for (const citationId of claim.citationIds) {
      const citation = citations.find((candidate) => candidate.citationId === citationId);
      if (!citation || !citation.claimIds.includes(claim.claimId)) {
        return reject("unsupported-claim", `claim ${claim.claimId} is not bidirectionally cited`);
      }
    }
  }
  const cleanup = parseCleanup(raw.cleanup, HELP_AUTHORITY_LIMITS.maxCleanupBytes);
  if ("accepted" in cleanup) return cleanup;
  if (
    cleanup.requestId !== request.requestId ||
    cleanup.status !== "finalized" ||
    cleanup.providerTask !== "joined" ||
    cleanup.queueSlot !== "released"
  ) {
    return reject("invalid-cleanup", "cleanup uncertainty cannot produce an answer");
  }
  const response: HelpAuthorityResponse = Object.freeze({
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "response",
    requestId: request.requestId,
    identity,
    provider,
    deadline,
    answer: sanitizeHelpText(raw.answer, 4_096),
    claims: Object.freeze(claims),
    citations: Object.freeze(citations),
    uncertainty: sanitizeHelpText(raw.uncertainty, HELP_AUTHORITY_LIMITS.maxUncertaintyBytes),
    cleanup,
  });
  return { accepted: true, value: response };
}

export function parseHelpAuthorityRequest(value: unknown): HelpAuthorityRequest | null {
  const validation = validateHelpAuthorityRequest(value);
  return validation.accepted && validation.value.kind === "request" ? validation.value : null;
}

export function parseHelpAuthorityResponse(
  value: unknown,
  request: HelpAuthorityRequest,
): HelpAuthorityResponse | null {
  const validation = validateHelpAuthorityResponse(value, request);
  return validation.accepted && validation.value.kind === "response" ? validation.value : null;
}

export function parseHelpAuthorityCleanup(
  value: unknown,
): HelpAuthorityCleanupReceipt | null {
  const validation = validateHelpAuthorityCleanup(value);
  return validation.accepted && validation.value.kind === "cleanup" ? validation.value : null;
}

export function createHelpAuthorityCleanupReceipt(
  requestId: string,
  status: HelpAuthorityCleanupReceipt["status"],
  providerTask: HelpAuthorityCleanupReceipt["providerTask"],
  abortRequested: boolean,
  queueSlot: HelpAuthorityCleanupReceipt["queueSlot"],
): HelpAuthorityCleanupReceipt {
  const receipt: HelpAuthorityCleanupReceipt = Object.freeze({
    schema: HELP_AUTHORITY_SCHEMA,
    kind: "cleanup",
    requestId,
    status,
    providerTask,
    abortRequested,
    queueSlot,
    artifactCounts: ZERO_ARTIFACTS,
  });
  const validation = validateHelpAuthorityCleanup(receipt);
  if (!validation.accepted) throw new Error(`${validation.reason}: ${validation.detail}`);
  return receipt;
}

export function helpAuthorityIdentityDigest(): string {
  return canonicalDigest({
    corpusDigest: HELP_CORPUS_DIGEST,
    sourceDigest: HELP_AUTHORITY_IDENTITY.sourceDigest,
    modelDigest: HELP_AUTHORITY_IDENTITY.modelDigest,
    modelId: HELP_AUTHORITY_IDENTITY.modelId,
    modelVersion: HELP_AUTHORITY_IDENTITY.modelVersion,
  });
}
