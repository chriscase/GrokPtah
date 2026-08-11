import { isDebugLine } from "./debugTrace";

/** Join live thought chunks without turning token-sized deliveries into rows. */
export function appendThoughtChunk(current: string, next: string): string {
  const chunk = next.trim();
  if (!chunk) return current;
  if (!current) return chunk;
  if (current === chunk || current.endsWith(chunk)) return current;

  const lastLine = current.slice(current.lastIndexOf("\n") + 1).trim();
  if (current.endsWith("\n") || chunk.startsWith("\n")) {
    return current + chunk;
  }
  // Keep host diagnostics legible and groupable by DebugTrace.
  if (isDebugLine(lastLine) || isDebugLine(chunk)) {
    return `${current}\n${chunk}`;
  }
  if (/[ \t]$/.test(current) || /^[ \t]/.test(chunk)) {
    return current + chunk;
  }
  if (/^[,.;:!?%)\]}]/.test(chunk)) {
    return current + chunk;
  }
  return `${current} ${chunk}`;
}
