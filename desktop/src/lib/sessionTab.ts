import type { SessionKind, SessionSummary, SessionTab } from "./protocol";

/** Prefer tab metadata because the session list is filtered by workspace mode. */
export function kindForTab(
  tab: SessionTab | null | undefined,
  sessions: SessionSummary[],
  fallback: SessionKind,
): SessionKind {
  return tab?.kind ?? sessions.find((s) => s.id === tab?.id)?.kind ?? fallback;
}

export function findTabOfKind(
  tabs: SessionTab[],
  kind: SessionKind,
): SessionTab | undefined {
  return tabs.find((tab) => tab.kind === kind);
}
