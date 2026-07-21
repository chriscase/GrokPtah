/**
 * Session/project deny history for permission modal (#175).
 * Persists in localStorage when available; memory fallback for tests/SSR.
 */

export type DenyHistoryEntry = {
  at: number;
  tool_name: string;
  summary: string;
  session_id: string;
  risk?: string;
  risk_tier?: string;
};

const KEY = "grokptah.permissionDenyHistory.v1";
const MAX = 20;

/** In-memory fallback when localStorage is unavailable (vitest/node). */
let memory: DenyHistoryEntry[] = [];

function storageGet(): DenyHistoryEntry[] | null {
  try {
    if (typeof localStorage === "undefined" || !localStorage) return null;
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as DenyHistoryEntry[];
    return Array.isArray(parsed) ? parsed.slice(0, MAX) : [];
  } catch {
    return null;
  }
}

function storageSet(entries: DenyHistoryEntry[]): void {
  try {
    if (typeof localStorage === "undefined" || !localStorage) return;
    localStorage.setItem(KEY, JSON.stringify(entries.slice(0, MAX)));
  } catch {
    /* ignore */
  }
}

export function loadDenyHistory(): DenyHistoryEntry[] {
  const fromStore = storageGet();
  if (fromStore !== null) return fromStore;
  return memory.slice(0, MAX);
}

export function saveDenyHistory(entries: DenyHistoryEntry[]): void {
  const next = entries.slice(0, MAX);
  memory = next;
  storageSet(next);
}

export function appendDeny(
  entries: DenyHistoryEntry[],
  entry: Omit<DenyHistoryEntry, "at"> & { at?: number },
): DenyHistoryEntry[] {
  const next: DenyHistoryEntry[] = [
    {
      at: entry.at ?? Date.now(),
      tool_name: entry.tool_name,
      summary: entry.summary,
      session_id: entry.session_id,
      risk: entry.risk,
      risk_tier: entry.risk_tier,
    },
    ...entries,
  ].slice(0, MAX);
  saveDenyHistory(next);
  return next;
}

export function clearDenyHistory(): DenyHistoryEntry[] {
  saveDenyHistory([]);
  return [];
}
