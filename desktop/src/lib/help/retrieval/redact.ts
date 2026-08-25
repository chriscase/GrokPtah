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

/** True when the value still contains anything credential-shaped. */
export function containsHelpSecret(value: string): boolean {
  return redactHelpText(value).redacted;
}
