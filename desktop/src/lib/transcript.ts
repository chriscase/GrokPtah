import type { TranscriptItem } from "./protocol";

/** Disk/API transcript line from the bridge. */
export type TranscriptEntryDto = {
  role: string;
  text: string;
  tool_call_id?: string | null;
  tool_title?: string | null;
  tool_status?: string | null;
  tool_output?: string | null;
};

/** Map durable transcript entries → UI items (including tools). */
export function entriesToTranscriptItems(
  entries: TranscriptEntryDto[],
): TranscriptItem[] {
  return entries.map((e) => {
    const role = (e.role || "").toLowerCase();
    if (role === "user") {
      return { kind: "user" as const, text: e.text };
    }
    if (role === "tool") {
      return {
        kind: "tool" as const,
        callId: e.tool_call_id || e.text || cryptoRandomId(),
        title: e.tool_title || e.text.split(" · ")[0] || "tool",
        status: e.tool_status || "completed",
        output: e.tool_output ?? undefined,
      };
    }
    if (role === "thought") {
      return { kind: "thought" as const, text: e.text };
    }
    if (role === "error") {
      return { kind: "error" as const, text: e.text };
    }
    // system notices and assistant share assistant styling for now
    if (role === "system") {
      return { kind: "thought" as const, text: e.text };
    }
    return { kind: "assistant" as const, text: e.text, streaming: false };
  });
}

function cryptoRandomId(): string {
  return `tool-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}
