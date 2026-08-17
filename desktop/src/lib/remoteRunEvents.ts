/** Singleton bus for backend-forwarded remote run events. */

import { listen } from "@tauri-apps/api/event";
import type { RemoteRunEvent, RemoteRunRecovery } from "./protocol";

export type RemoteRunNotification =
  | { type: "event"; payload: RemoteRunEvent }
  | { type: "recovery"; payload: RemoteRunRecovery };

type Handler = (notification: RemoteRunNotification) => void;

type Bus = {
  handlers: Set<Handler>;
  started: boolean;
};

const GLOBAL_KEY = "__grokptahRemoteRunEventBus";

function bus(): Bus {
  const global = globalThis as unknown as Record<string, Bus | undefined>;
  if (!global[GLOBAL_KEY]) {
    global[GLOBAL_KEY] = { handlers: new Set(), started: false };
  }
  return global[GLOBAL_KEY]!;
}

function ensureListening() {
  const current = bus();
  if (current.started) return;
  const runtime = globalThis as unknown as { __TAURI_INTERNALS__?: unknown };
  if (!runtime.__TAURI_INTERNALS__) return;
  current.started = true;
  void Promise.all([
    listen<RemoteRunEvent>("remote://run-event", (event) => {
      const notification: RemoteRunNotification = {
        type: "event",
        payload: event.payload,
      };
      for (const handler of [...bus().handlers]) handler(notification);
    }),
    listen<RemoteRunRecovery>("remote://run-recovery", (event) => {
      const notification: RemoteRunNotification = {
        type: "recovery",
        payload: event.payload,
      };
      for (const handler of [...bus().handlers]) handler(notification);
    }),
  ]).catch((error) => {
    current.started = false;
    console.warn("remote run event bus unavailable", error);
  });
}

export function subscribeRemoteRunEvents(handler: Handler): () => void {
  const current = bus();
  current.handlers.add(handler);
  ensureListening();
  return () => current.handlers.delete(handler);
}
