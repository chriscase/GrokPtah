/** Last path segment for title disambiguation (#130). */
export function pathBasename(path: string | null | undefined): string {
  if (!path) return "";
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/**
 * Display title: when multiple sessions share the same title, append cwd
 * basename so they stay distinguishable. Long labels use middle ellipsis.
 */
export function displaySessionTitle(
  session: { id: string; title: string; cwd?: string | null },
  peers: { id: string; title: string; cwd?: string | null }[],
  maxLen = 36,
): string {
  const raw = (session.title || "Untitled").trim() || "Untitled";
  const sameTitle = peers.filter(
    (p) => p.id !== session.id && (p.title || "").trim() === raw,
  );
  let label = raw;
  if (sameTitle.length > 0) {
    const base = pathBasename(session.cwd);
    // Put basename first so CSS end-ellipsis keeps the disambiguator visible.
    if (base) label = `${base} · ${raw}`;
  }
  if (label.length <= maxLen) return label;
  const keep = Math.max(8, Math.floor((maxLen - 1) / 2));
  return `${label.slice(0, keep)}…${label.slice(-keep)}`;
}
