/**
 * Tauri-free Grok Build account readiness contract.
 *
 * Mirrors `grokptah-agent-sdk::account` and the `grokptah-account.v1` schema.
 * The parser is strict in the same way the Rust projection is: unknown keys,
 * out-of-vocabulary values, and out-of-bounds references all fail closed to
 * `null` rather than reaching a UI.
 *
 * Nothing in this module can carry a bearer, refresh token, API key,
 * credential reference, credential fingerprint, or a free-form auth mode:
 * those fields do not exist in the contract. It also makes no claim about
 * account balance, quota, entitlement, or live provider certification — only
 * a provider round-trip could establish those, and this projection performs
 * none.
 */

export const GROK_ACCOUNT_CONTRACT = "grokptah.account.v1" as const;
export const GROK_ACCOUNT_SCHEMA_VERSION = 1 as const;
/** Maximum UTF-8 bytes accepted in a public account reference. */
export const MAX_ACCOUNT_REFERENCE_BYTES = 64;

export type CredentialMethod =
  | "absent"
  | "api_key"
  | "token_command"
  | "provider_env"
  | "provider_keychain"
  | "grok_build_oidc"
  | "grok_build_api_key"
  | "unknown";

export type AccountReferenceSource = "user_id" | "principal_id" | "team_id";

export type ExpiryStatus = "absent" | "unparseable" | "valid" | "expired";

export type AccountReadiness = "usable" | "unknown" | "unusable";

export type ReadinessReason =
  | "no_credential"
  | "credential_expired"
  | "expiry_in_future"
  | "expiry_not_provided"
  | "expiry_unparseable"
  | "method_unrecognized";

export type AccountReference = {
  value: string;
  source: AccountReferenceSource;
};

export type ExpiryFacts = {
  status: ExpiryStatus;
  expiresAt?: string | null;
  secondsRemaining?: number | null;
};

export type GrokAccountFacts = {
  contract: typeof GROK_ACCOUNT_CONTRACT;
  schemaVersion: typeof GROK_ACCOUNT_SCHEMA_VERSION;
  credentialMethod: CredentialMethod;
  accountReference?: AccountReference | null;
  expiry: ExpiryFacts;
  readiness: AccountReadiness;
  readinessReason: ReadinessReason;
};

export type RunAttribution = {
  credentialMethod: CredentialMethod;
  accountReference?: AccountReference | null;
};

/** Closed vocabularies, in the exact order the v1 schema pins them. */
export const CREDENTIAL_METHODS: readonly CredentialMethod[] = Object.freeze([
  "absent",
  "api_key",
  "token_command",
  "provider_env",
  "provider_keychain",
  "grok_build_oidc",
  "grok_build_api_key",
  "unknown",
] as const);

export const ACCOUNT_REFERENCE_SOURCES: readonly AccountReferenceSource[] = Object.freeze([
  "user_id",
  "principal_id",
  "team_id",
] as const);

export const EXPIRY_STATUSES: readonly ExpiryStatus[] = Object.freeze([
  "absent",
  "unparseable",
  "valid",
  "expired",
] as const);

export const ACCOUNT_READINESS_STATES: readonly AccountReadiness[] = Object.freeze([
  "usable",
  "unknown",
  "unusable",
] as const);

export const READINESS_REASONS: readonly ReadinessReason[] = Object.freeze([
  "no_credential",
  "credential_expired",
  "expiry_in_future",
  "expiry_not_provided",
  "expiry_unparseable",
  "method_unrecognized",
] as const);

const METHODS = new Set<string>(CREDENTIAL_METHODS);
const REFERENCE_SOURCES = new Set<string>(ACCOUNT_REFERENCE_SOURCES);
const EXPIRY = new Set<string>(EXPIRY_STATUSES);
const READINESS = new Set<string>(ACCOUNT_READINESS_STATES);
const REASONS = new Set<string>(READINESS_REASONS);

/** Opaque durable identifiers only: no whitespace, markup, or path fragments. */
const ACCOUNT_REFERENCE_PATTERN = /^[A-Za-z0-9._:-]+$/;
/** Normalized UTC instants only, re-serialized by the projection. */
const UTC_INSTANT_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOnlyKeys(value: Record<string, unknown>, keys: ReadonlySet<string>): boolean {
  return Object.keys(value).every((key) => keys.has(key));
}

const ACCOUNT_REFERENCE_KEYS = new Set(["value", "source"]);
const EXPIRY_KEYS = new Set(["status", "expiresAt", "secondsRemaining"]);
const FACTS_KEYS = new Set([
  "contract",
  "schemaVersion",
  "credentialMethod",
  "accountReference",
  "expiry",
  "readiness",
  "readinessReason",
]);
const ATTRIBUTION_KEYS = new Set(["credentialMethod", "accountReference"]);

function parseAccountReference(value: unknown): AccountReference | null {
  if (!isRecord(value) || !hasOnlyKeys(value, ACCOUNT_REFERENCE_KEYS)) return null;
  if (typeof value.value !== "string" || typeof value.source !== "string") return null;
  if (!REFERENCE_SOURCES.has(value.source)) return null;
  if (
    !ACCOUNT_REFERENCE_PATTERN.test(value.value) ||
    new TextEncoder().encode(value.value).byteLength > MAX_ACCOUNT_REFERENCE_BYTES
  ) {
    return null;
  }
  return { value: value.value, source: value.source as AccountReferenceSource };
}

function parseExpiryFacts(value: unknown): ExpiryFacts | null {
  if (!isRecord(value) || !hasOnlyKeys(value, EXPIRY_KEYS)) return null;
  if (typeof value.status !== "string" || !EXPIRY.has(value.status)) return null;
  const status = value.status as ExpiryStatus;
  const expiresAt = value.expiresAt ?? null;
  const secondsRemaining = value.secondsRemaining ?? null;
  if (expiresAt !== null && (typeof expiresAt !== "string" || !UTC_INSTANT_PATTERN.test(expiresAt))) {
    return null;
  }
  if (
    secondsRemaining !== null &&
    (typeof secondsRemaining !== "number" || !Number.isSafeInteger(secondsRemaining))
  ) {
    return null;
  }
  // Absent and unparseable expiry must not smuggle a timestamp back in.
  if ((status === "absent" || status === "unparseable") && (expiresAt !== null || secondsRemaining !== null)) {
    return null;
  }
  if ((status === "valid" || status === "expired") && (expiresAt === null || secondsRemaining === null)) {
    return null;
  }
  return { status, expiresAt, secondsRemaining };
}

/**
 * Whether a readiness verdict actually follows from the evidence beside it.
 *
 * Mirrors `decide_readiness` in the Rust projection so a tampered or
 * out-of-date producer cannot talk the editor into launching.
 */
function readinessFollows(
  method: CredentialMethod,
  expiry: ExpiryStatus,
  readiness: AccountReadiness,
  reason: ReadinessReason,
): boolean {
  const expected: [AccountReadiness, ReadinessReason] =
    method === "absent"
      ? ["unusable", "no_credential"]
      : expiry === "expired"
        ? ["unusable", "credential_expired"]
        : expiry === "unparseable"
          ? ["unknown", "expiry_unparseable"]
          : method === "unknown"
            ? ["unknown", "method_unrecognized"]
            : expiry === "valid"
              ? ["usable", "expiry_in_future"]
              : ["unknown", "expiry_not_provided"];
  return readiness === expected[0] && reason === expected[1];
}

/** Parse published account facts. Returns `null` for anything off-contract. */
export function parseGrokAccountFacts(value: unknown): GrokAccountFacts | null {
  if (!isRecord(value) || !hasOnlyKeys(value, FACTS_KEYS)) return null;
  if (value.contract !== GROK_ACCOUNT_CONTRACT) return null;
  if (value.schemaVersion !== GROK_ACCOUNT_SCHEMA_VERSION) return null;
  if (typeof value.credentialMethod !== "string" || !METHODS.has(value.credentialMethod)) return null;
  if (typeof value.readiness !== "string" || !READINESS.has(value.readiness)) return null;
  if (typeof value.readinessReason !== "string" || !REASONS.has(value.readinessReason)) return null;
  const expiry = parseExpiryFacts(value.expiry);
  if (!expiry) return null;
  const rawReference = value.accountReference ?? null;
  const accountReference = rawReference === null ? null : parseAccountReference(rawReference);
  if (rawReference !== null && !accountReference) return null;
  const credentialMethod = value.credentialMethod as CredentialMethod;
  const readiness = value.readiness as AccountReadiness;
  const readinessReason = value.readinessReason as ReadinessReason;
  if (!readinessFollows(credentialMethod, expiry.status, readiness, readinessReason)) return null;
  return {
    contract: GROK_ACCOUNT_CONTRACT,
    schemaVersion: GROK_ACCOUNT_SCHEMA_VERSION,
    credentialMethod,
    accountReference,
    expiry,
    readiness,
    readinessReason,
  };
}

/** Parse bounded run attribution. Returns `null` for anything off-contract. */
export function parseRunAttribution(value: unknown): RunAttribution | null {
  if (!isRecord(value) || !hasOnlyKeys(value, ATTRIBUTION_KEYS)) return null;
  if (typeof value.credentialMethod !== "string" || !METHODS.has(value.credentialMethod)) return null;
  const rawReference = value.accountReference ?? null;
  const accountReference = rawReference === null ? null : parseAccountReference(rawReference);
  if (rawReference !== null && !accountReference) return null;
  return { credentialMethod: value.credentialMethod as CredentialMethod, accountReference };
}

/** Facts for a host that reported nothing usable. Blocks new launches. */
export function absentGrokAccountFacts(): GrokAccountFacts {
  return {
    contract: GROK_ACCOUNT_CONTRACT,
    schemaVersion: GROK_ACCOUNT_SCHEMA_VERSION,
    credentialMethod: "absent",
    accountReference: null,
    expiry: { status: "absent", expiresAt: null, secondsRemaining: null },
    readiness: "unusable",
    readinessReason: "no_credential",
  };
}

/**
 * Whether the editor may start a *new* Grok Build run.
 *
 * Only positive negative-evidence blocks. `unknown` stays permissive so a
 * credential with no expiry field is never locked out by our ignorance, and
 * unparsable facts (`null`) block because we cannot vouch for them.
 */
export function canLaunchGrokBuild(facts: GrokAccountFacts | null): boolean {
  return facts !== null && facts.readiness !== "unusable";
}

/** Human-readable label for a credential route. */
export function credentialMethodLabel(method: CredentialMethod): string {
  switch (method) {
    case "absent":
      return "No credential";
    case "api_key":
      return "xAI API key";
    case "token_command":
      return "Token helper";
    case "provider_env":
      return "Provider env key";
    case "provider_keychain":
      return "Provider keychain key";
    case "grok_build_oidc":
      return "Grok Build sign-in";
    case "grok_build_api_key":
      return "Grok Build API key";
    case "unknown":
      return "Unrecognized route";
  }
}

/** Bounded, deterministic duration text. Never renders a raw timestamp. */
export function formatSecondsRemaining(seconds: number): string {
  const elapsed = seconds <= 0;
  const magnitude = Math.abs(seconds);
  const days = Math.floor(magnitude / 86_400);
  const hours = Math.floor((magnitude % 86_400) / 3_600);
  const minutes = Math.floor((magnitude % 3_600) / 60);
  const amount =
    days > 0
      ? `${days}d ${hours}h`
      : hours > 0
        ? `${hours}h ${minutes}m`
        : minutes > 0
          ? `${minutes}m`
          : `${magnitude}s`;
  return elapsed ? `${amount} ago` : `in ${amount}`;
}

export type GrokAccountNoticeTone = "ready" | "unknown" | "blocked";

export type GrokAccountNotice = {
  tone: GrokAccountNoticeTone;
  /** Whether new Grok Build launches are disabled. */
  blocksLaunch: boolean;
  /** Short badge label. */
  summary: string;
  /** Full sentence explaining what is and is not known. */
  detail: string;
  /** Concrete recovery step, present exactly when a launch is blocked. */
  remedy: string | null;
};

/**
 * Turn facts into exact UI copy.
 *
 * The distinction the copy must carry: *expired* is positive evidence the
 * credential cannot work and therefore blocks; *unknown* is an absence of
 * evidence and never blocks. Existing runs stay inspectable in both cases —
 * only new launches are gated.
 */
export function grokAccountNotice(facts: GrokAccountFacts | null): GrokAccountNotice {
  if (facts === null) {
    return {
      tone: "blocked",
      blocksLaunch: true,
      summary: "Account status unreadable",
      detail:
        "The host returned account facts this build cannot validate, so it will not vouch for the credential. Existing runs remain readable.",
      remedy: "Update GrokPtah so the desktop and host account contracts match, then reload.",
    };
  }

  const route = credentialMethodLabel(facts.credentialMethod);
  // Two forms: a compact chip label, and a phrase that reads correctly inside
  // a sentence when no durable identity was published.
  const accountChip = facts.accountReference
    ? `account ${facts.accountReference.value}`
    : "account not identified";
  const account = facts.accountReference
    ? `account ${facts.accountReference.value}`
    : "an unidentified account";
  const remaining = facts.expiry.secondsRemaining;

  switch (facts.readinessReason) {
    case "no_credential":
      return {
        tone: "blocked",
        blocksLaunch: true,
        summary: "Not signed in",
        detail:
          "No Grok Build credential was found on any route, so new runs are disabled. Existing runs remain readable.",
        remedy: "Sign in to Grok Build, or add an xAI API key in Settings → Auth.",
      };
    case "credential_expired":
      return {
        tone: "blocked",
        blocksLaunch: true,
        summary: `Session expired · ${accountChip}`,
        detail: `${route} for ${account} expired${
          typeof remaining === "number" ? ` ${formatSecondsRemaining(remaining)}` : ""
        }. New runs are disabled until it is refreshed. Existing runs remain readable.`,
        remedy: "Sign in to Grok Build again to refresh this session, or add an xAI API key in Settings → Auth.",
      };
    case "expiry_in_future":
      return {
        tone: "ready",
        blocksLaunch: false,
        summary: `${route} · ${accountChip}`,
        detail: `${route} for ${account} is valid${
          typeof remaining === "number" ? `, expiring ${formatSecondsRemaining(remaining)}` : ""
        }. This reflects local credential state only — nothing here was checked against the provider.`,
        remedy: null,
      };
    case "expiry_not_provided":
      return {
        tone: "unknown",
        blocksLaunch: false,
        summary: `${route} · expiry unknown`,
        detail: `${route} for ${account} carries no expiry, so this build cannot say when it lapses. That is unknown, not expired — runs are not blocked.`,
        remedy: null,
      };
    case "expiry_unparseable":
      return {
        tone: "unknown",
        blocksLaunch: false,
        summary: `${route} · expiry unreadable`,
        detail: `${route} for ${account} has an expiry this build could not read, so it is treated as unknown rather than expired. Runs are not blocked.`,
        remedy: null,
      };
    case "method_unrecognized":
      return {
        tone: "unknown",
        blocksLaunch: false,
        summary: "Route unrecognized",
        detail:
          "A credential is present but its sign-in route is not one this build recognizes, so no readiness claim is made. Runs are not blocked.",
        remedy: null,
      };
  }
}
