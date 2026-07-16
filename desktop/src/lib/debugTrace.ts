/**
 * Host status crumbs (kind/model/auth, "calling chat API…") used to land in the
 * transcript as full thought bubbles. These helpers classify and group them so
 * the UI can collapse them into a single diagnostics control.
 */

export function isDebugLine(line: string): boolean {
  const l = line.trim().toLowerCase();
  if (!l) return false;
  if (l.startsWith("kind=")) return true;
  if (l.includes("model=") && (l.includes("effort=") || l.includes("auth="))) {
    return true;
  }
  if (l.includes("calling chat api") || l.includes("chat api as")) return true;
  if (l.includes("calling xai") || l.includes(" as grok_build")) return true;
  if (l.startsWith("auth=") || l.includes(" via grok_build")) return true;
  if (l.includes("via ") && (l.includes("oidc") || l.includes("api_key"))) {
    return true;
  }
  // Agent-loop status (must not appear as full thought bubbles)
  if (l.includes("agent loop starting")) return true;
  if (/^agent round \d+\/\d+/.test(l)) return true;
  if (/^tool `[a-z0-9_./-]+`/.test(l)) return true;
  if (l.includes("tools on") && l.includes("rounds")) return true;
  if (l.includes("sandbox=") && l.includes("effort=")) return true;
  if (l.includes("offline agent")) return true;
  // Single-line host crumbs that are not natural-language reasoning
  if (/^[a-z_]+=\S+/.test(l) && l.length < 200) return true;
  return false;
}

/** True when every non-empty line looks like a host status crumb. */
export function isDebugThought(text: string): boolean {
  const lines = text
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
  if (lines.length === 0) return false;
  return lines.every(isDebugLine);
}

export function expandDebugLines(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split("\n")) {
    const t = raw.trim();
    if (!t) continue;
    // Older multi-listener bugs glued copies with no newline — split on kind=
    if (t.includes("kind=") && t.indexOf("kind=") !== t.lastIndexOf("kind=")) {
      for (const part of t.split(/(?=kind=)/)) {
        const p = part.trim();
        if (p && !out.includes(p)) out.push(p);
      }
      continue;
    }
    if (!out.includes(t)) out.push(t);
  }
  return out;
}

/** Short label for the collapsed chip. */
export function debugChipLabel(lines: string[]): string {
  const blob = lines.join(" ").toLowerCase();
  if (blob.includes("agent round") || blob.includes("agent loop")) {
    return "Agent activity";
  }
  if (blob.includes("kind=chat")) return "Chat turn";
  if (blob.includes("kind=build")) return "Build turn";
  if (blob.includes("chat api") || blob.includes("calling")) return "API call";
  return "Turn details";
}
