/**
 * Singleton bus for `session://update`.
 *
 * Vite HMR + React StrictMode re-ran `listen()` without tearing down prior
 * subscriptions, so each model reply was delivered N times and the UI
 * concatenated the full string (Yes…Yes…Yes…). One Tauri subscription for
 * the whole page lifetime; components only register/unregister handlers.
 */

import { listen } from "@tauri-apps/api/event";
import {
  normalizeSessionUpdate,
  type SessionUpdate,
} from "./protocol";
import { nextAssistantText } from "./streamApply";

export type SessionUpdateHandler = (u: SessionUpdate) => void;

type Bus = {
  handlers: Set<SessionUpdateHandler>;
  started: boolean;
  unlisten: (() => void) | null;
};

const GLOBAL_KEY = "__grokptahSessionUpdateBus";

function bus(): Bus {
  const g = globalThis as unknown as Record<string, Bus | undefined>;
  if (!g[GLOBAL_KEY]) {
    g[GLOBAL_KEY] = {
      handlers: new Set(),
      started: false,
      unlisten: null,
    };
  }
  return g[GLOBAL_KEY]!;
}

function ensureListening() {
  const b = bus();
  if (b.started) return;
  b.started = true;
  void listen("session://update", (event) => {
    const u = normalizeSessionUpdate(event.payload);
    if (!u) return;
    // Snapshot handlers so a re-entrant unsubscribe is safe.
    for (const h of [...b.handlers]) {
      try {
        h(u);
      } catch (e) {
        console.warn("session update handler error", e);
      }
    }
  }).then((fn) => {
    b.unlisten = fn;
  });
}

/** Register a handler. Returns unsubscribe. Safe under HMR / StrictMode. */
export function subscribeSessionUpdates(
  handler: SessionUpdateHandler,
): () => void {
  const b = bus();
  b.handlers.add(handler);
  ensureListening();
  return () => {
    b.handlers.delete(handler);
  };
}

/**
 * If `text` is the same phrase repeated N times (concatenated), return one
 * copy. Handles the multi-listener failure mode in already-shown bubbles.
 */
export function collapseRepeatedText(text: string): string {
  const t = text.trim();
  if (t.length < 8) return text;

  // Try period-based split first (full sentences glued).
  for (let n = 2; n <= 8; n++) {
    if (t.length % n !== 0) continue;
    const unitLen = t.length / n;
    const unit = t.slice(0, unitLen);
    if (!unit.trim()) continue;
    if (unit.repeat(n) === t) return unit;
  }

  // Prefix that tiles the whole string (e.g. short replies).
  for (let len = Math.floor(t.length / 2); len >= 4; len--) {
    if (t.length % len !== 0) continue;
    const unit = t.slice(0, len);
    const times = t.length / len;
    if (times < 2 || times > 12) continue;
    if (unit.repeat(times) === t) return unit;
  }

  return text;
}

/**
 * Merge a new agent message chunk into the last assistant bubble without
 * duplicating a full re-delivery of the same content.
 *
 * Implementation lives in `streamApply` (content/seq based — not
 * length-only, and does not collapse legitimate repeated code lines).
 */
export function mergeAssistantChunk(
  existing: string,
  chunk: string,
): string | "skip" {
  return nextAssistantText(existing, chunk);
}
