const PLACEHOLDER_SECRETS = [
  "Saved (leave blank to keep)",
  "Provider key",
  "xai-…",
];

const SECRET_PATTERNS = [
  /\bxai-[A-Za-z0-9._-]+/gi,
  /\bsk-[A-Za-z0-9._-]+/gi,
  /Bearer\s+\S+/gi,
  /authorization\s*[:=]\s*\S+/gi,
  /api[_-]?key\s*[:=]\s*\S+/gi,
  /(?:x-api-key|x-auth-token)\s*[:=]\s*\S+/gi,
];

const LOCAL_PATH_PATTERN = /(?:\/Users\/|\/private\/tmp\/|\/var\/folders\/|[A-Za-z]:\\)[^\s),;]+/g;

/** Remove credentials, local filesystem paths, and UI-only secret placeholders. */
export function sanitizeSensitiveText(text: string): string {
  let safe = text;
  for (const placeholder of PLACEHOLDER_SECRETS) {
    safe = safe.split(placeholder).join("[redacted]");
  }
  for (const pattern of SECRET_PATTERNS) {
    safe = safe.replace(pattern, "[redacted]");
  }
  return safe.replace(LOCAL_PATH_PATTERN, "[local path redacted]");
}

/** Convert an unknown backend failure into bounded, display-safe text. */
export function safeErrorMessage(
  reason: unknown,
  fallback = "The operation could not be completed.",
): string {
  if (reason === null || reason === undefined) return fallback;
  const raw = reason instanceof Error ? reason.message : String(reason);
  const safe = sanitizeSensitiveText(raw).trim();
  if (!safe) return fallback;
  return safe.length > 320 ? `${safe.slice(0, 317)}…` : safe;
}
