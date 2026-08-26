/**
 * Credential and private-path redaction for Help queries.
 *
 * Users paste secrets into search boxes. Redaction happens before tokenization
 * so a credential is never indexed, never scored, never echoed in an excerpt,
 * and never forwarded to a provider by the answer contract.
 *
 * It also improves retrieval: with the token removed, "my key <redacted>
 * stopped working on the gateway" is a clear gateway question instead of a
 * query dominated by a high-entropy string the corpus has no word for.
 */

export const HELP_REDACTION_PLACEHOLDER = "[redacted]";

type RedactionRule = {
  readonly kind: string;
  readonly pattern: RegExp;
};

/**
 * Ordered so that longer, more specific constructs match before the generic
 * high-entropy rule can consume part of them.
 */
const RULES: readonly RedactionRule[] = Object.freeze([
  { kind: "pem", pattern: /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)/g },
  { kind: "pem-header", pattern: /-----BEGIN [A-Z0-9 ]*(?:PRIVATE KEY|CERTIFICATE)-----/g },
  { kind: "authorization-header", pattern: /\bAuthorization\s*:\s*(?:Bearer|Basic|Token)\s+[A-Za-z0-9._~+/=-]+/gi },
  { kind: "bearer-token", pattern: /\b(?:Bearer|Basic|Token)\s+[A-Za-z0-9._~+/=-]{12,}/gi },
  { kind: "assignment", pattern: /\b[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL)[A-Z0-9_]*\s*[=:]\s*["']?[^\s"']{6,}["']?/gi },
  { kind: "provider-key", pattern: /\b(?:xai|sk|pk|rk|ghp|gho|ghs|ghu|github_pat|glpat|npm|AKIA|ASIA)[-_][A-Za-z0-9._-]{10,}/g },
  { kind: "jwt", pattern: /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}/g },
  { kind: "posix-home-path", pattern: /\/(?:Users|home)\/[A-Za-z0-9._-]+(?:\/[^\s"']*)?/g },
  { kind: "posix-private-path", pattern: /\/private\/(?:var|tmp|etc)(?:\/[^\s"']*)?/g },
  { kind: "windows-user-path", pattern: /[A-Za-z]:\\Users\\[^\s"']+/g },
  // Generic high-entropy blob, last so specific rules win.
  { kind: "high-entropy", pattern: /\b(?=[A-Za-z0-9+/=_-]{24,}\b)(?=[^\s]*[0-9])(?=[^\s]*[A-Za-z])[A-Za-z0-9+/=_-]{24,}\b/g },
]);

export type HelpRedaction = {
  readonly kind: string;
  readonly count: number;
};

export type HelpRedactionResult = {
  readonly text: string;
  readonly redactions: readonly HelpRedaction[];
  readonly redacted: boolean;
};

/**
 * Replace credential-shaped and private-path substrings with a placeholder.
 *
 * Reports only the *kind* and count of what was removed — never the matched
 * text, which would defeat the purpose.
 */
export function redactHelpText(value: string): HelpRedactionResult {
  let text = value;
  const redactions: HelpRedaction[] = [];
  for (const rule of RULES) {
    let count = 0;
    text = text.replace(new RegExp(rule.pattern.source, rule.pattern.flags), () => {
      count += 1;
      return HELP_REDACTION_PLACEHOLDER;
    });
    if (count > 0) redactions.push(Object.freeze({ kind: rule.kind, count }));
  }
  return {
    text: redactions.length > 0 ? text.replace(/\s+/g, " ").trim() : value,
    redactions: Object.freeze(redactions),
    redacted: redactions.length > 0,
  };
}

/** True when the value contains something a redaction rule matched. */
export function containsHelpSecret(value: string): boolean {
  return redactHelpText(value).redacted;
}

/**
 * How sure the scan is.
 *
 * The scan used to be binary: a rule matched, or the text was clean. That
 * reported certainty it did not have. A nineteen-character mixed token is
 * below the high-entropy floor and matches nothing, so it came back "clean" —
 * the same answer given to a sentence about checkpoints. An unfamiliar key
 * prefix did too.
 *
 * `possible` is the missing third answer. It is not enough to redact on: a
 * redaction destroys legitimate text, and a heuristic this loose would fire on
 * ordinary identifiers. It is enough to *refuse* on, which is why untrusted
 * provider output is held to it.
 */
export type HelpSecretConfidence = "clean" | "possible" | "certain";

export type HelpSecretScan = {
  readonly confidence: HelpSecretConfidence;
  /** Redaction rule kinds that matched. Never the matched text. */
  readonly kinds: readonly string[];
  /** What made the scan uncertain, when nothing matched outright. */
  readonly indicators: readonly string[];
};

/**
 * Shapes that are not conclusive but are not nothing either.
 *
 * Each is deliberately below the threshold of a redaction rule. Together they
 * are the difference between "the scan found nothing" and "the scan is not
 * sure", which are not the same statement and should never have been reported
 * with the same value.
 */
const UNCERTAIN: readonly RedactionRule[] = Object.freeze([
  // Mixed-case alphanumeric run just under the high-entropy floor.
  {
    kind: "short-high-entropy",
    pattern: /\b(?=[A-Za-z0-9_-]{16,23}\b)(?=[^\s]*[0-9])(?=[^\s]*[a-z])(?=[^\s]*[A-Z])[A-Za-z0-9_-]{16,23}\b/g,
  },
  // Base64-looking run with padding, too short for the generic rule.
  { kind: "padded-base64", pattern: /\b[A-Za-z0-9+/]{12,}={1,2}(?![A-Za-z0-9+/=])/g },
  // A credential word next to something *value*-shaped that the assignment rule
  // did not claim, e.g. `token abc123def456`.
  //
  // The value shape is required, and it is what makes this usable: matching a
  // credential word followed by any word at all flagged "Rotate the key
  // regularly." and "Use token rotation." — ordinary Help sentences, and
  // exactly the sentences a Help answer about providers is made of. A rule
  // that refuses those refuses the product.
  {
    kind: "credential-adjacent",
    pattern:
      /\b(?:key|token|secret|password|passwd|credential)s?\b[\s:=]{1,3}["']?(?=[^\s"']*[0-9])(?=[^\s"']*[A-Za-z])[^\s"']{8,}/gi,
  },
  // A long hex run. Usually a digest, which is why this is not certain —
  // `sha256:` prefixed values are excluded below because the corpus, the
  // receipts, and the contracts are full of them.
  { kind: "long-hex", pattern: /\b[0-9a-f]{24,}\b/g },
  // An unfamiliar prefixed key: the shape of a provider credential without a
  // prefix any rule recognizes.
  { kind: "unknown-prefixed-key", pattern: /\b[a-z]{2,8}[-_][A-Za-z0-9]{16,}\b/g },
]);

/**
 * Scan without pretending to be sure.
 *
 * A `certain` result is a redaction rule match. A `possible` result is a shape
 * that could be a credential and could be an ordinary identifier; the caller
 * decides what that is worth. `clean` means neither, and now genuinely means
 * it.
 */
export function scanHelpForSecrets(value: string): HelpSecretScan {
  // Digests are named with their algorithm throughout the corpus, the
  // contracts, and the answers, so the hex that follows one is not an
  // unexplained blob.
  //
  // The exemption has to run *before* the redaction rules, not after them. The
  // generic high-entropy rule matches a 64-character hex string, so a sentence
  // naming the corpus digest scanned as a `certain` credential — and since
  // untrusted provider text is refused on anything but `clean`, every answer
  // that mentioned a digest was rejected.
  const withoutDigests = value.replace(
    /\b(?:sha256|sha1|md5|blake3|hmac-sha256):[0-9a-f]+/gi,
    " ",
  );
  const redaction = redactHelpText(withoutDigests);
  if (redaction.redacted) {
    return Object.freeze({
      confidence: "certain" as const,
      kinds: Object.freeze(redaction.redactions.map((entry) => entry.kind)),
      indicators: Object.freeze([]),
    });
  }

  const indicators: string[] = [];
  for (const rule of UNCERTAIN) {
    if (new RegExp(rule.pattern.source, rule.pattern.flags).test(withoutDigests)) {
      indicators.push(rule.kind);
    }
  }

  return Object.freeze({
    confidence: indicators.length > 0 ? ("possible" as const) : ("clean" as const),
    kinds: Object.freeze([]),
    indicators: Object.freeze(indicators),
  });
}
