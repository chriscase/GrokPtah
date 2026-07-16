/**
 * Split streaming markdown into a "stable" prefix (safe to fully parse) and a
 * "tail" (live region — materialize + beam).
 */

export type StreamMarkdownSplit = {
  stable: string;
  tail: string;
};

/**
 * Keep a large live tail so new content spends real time in the beam zone.
 * Stable markdown only gets blocks that are well behind the stream tip.
 */
const BEAM_TAIL_MIN = 100;
const BEAM_TAIL_MAX = 280;

/**
 * Find start index of an unclosed ``` fence, or -1 if all fences closed.
 */
export function unclosedFenceStart(text: string): number {
  let inFence = false;
  let fenceAt = -1;
  let offset = 0;
  const lines = text.split("\n");
  for (const line of lines) {
    if (/^```/.test(line)) {
      if (!inFence) {
        inFence = true;
        fenceAt = offset;
      } else {
        inFence = false;
        fenceAt = -1;
      }
    }
    offset += line.length + 1;
  }
  return inFence ? fenceAt : -1;
}

/**
 * If the text ends with an incomplete GFM table, return the index where that
 * table fragment starts so it stays in the beam tail until more rows arrive.
 */
export function incompleteTableStart(text: string): number {
  const lines = text.split("\n");
  let i = lines.length - 1;
  while (i >= 0 && lines[i].trim() === "") i--;
  if (i < 0) return -1;

  const isTableLine = (l: string) => {
    const t = l.trim();
    return t.startsWith("|") && t.includes("|", 1);
  };
  const isSep = (l: string) =>
    /^\|?[\s:|-]+\|[\s:|-]*$/.test(l.trim()) ||
    /^\|?(?:\s*:?-+:?\s*\|)+\s*:?-+:?\s*\|?\s*$/.test(l.trim());

  if (!isTableLine(lines[i]) && !isSep(lines[i])) return -1;

  let start = i;
  while (
    start > 0 &&
    (isTableLine(lines[start - 1]) || isSep(lines[start - 1]))
  ) {
    start--;
  }

  const region = lines.slice(start, i + 1);
  const hasSep = region.some(isSep);
  const dataRows = region.filter((l) => isTableLine(l) && !isSep(l));

  if (!hasSep || dataRows.length < 2) {
    let off = 0;
    for (let k = 0; k < start; k++) off += lines[k].length + 1;
    return off;
  }
  return -1;
}

function lastSafeCut(text: string, minTail: number, maxTail: number): number {
  if (text.length <= minTail) return 0;
  const minCut = Math.max(0, text.length - maxTail);
  const maxCut = text.length - minTail;
  for (let i = maxCut; i >= minCut; i--) {
    if (text[i - 1] === "\n") return i;
  }
  for (let i = maxCut; i >= minCut; i--) {
    const ch = text[i - 1];
    if (".!?".includes(ch) && (text[i] === " " || text[i] === "\n")) return i;
  }
  for (let i = maxCut; i >= minCut; i--) {
    if (text[i - 1] === " ") return i;
  }
  return minCut;
}

/**
 * Split streaming markdown into parseable stable prefix + beamed tail.
 * Tail is intentionally large so materialization is visible.
 */
export function splitStreamingMarkdown(text: string): StreamMarkdownSplit {
  if (!text) return { stable: "", tail: "" };

  const fenceAt = unclosedFenceStart(text);
  if (fenceAt >= 0) {
    // Prefer some beamed content even inside long fences
    const fromFence = text.slice(fenceAt);
    if (fromFence.length > BEAM_TAIL_MAX) {
      const cut = fenceAt + lastSafeCut(fromFence, BEAM_TAIL_MIN, BEAM_TAIL_MAX);
      return {
        stable: text.slice(0, cut),
        tail: text.slice(cut),
      };
    }
    return {
      stable: text.slice(0, fenceAt),
      tail: fromFence,
    };
  }

  const tableAt = incompleteTableStart(text);
  if (tableAt >= 0) {
    return {
      stable: text.slice(0, tableAt),
      tail: text.slice(tableAt),
    };
  }

  // Always keep a substantial beam tail — this is what makes materialization
  // feel real. Stable only gets older content.
  if (text.length <= BEAM_TAIL_MIN) {
    return { stable: "", tail: text };
  }

  const cut = lastSafeCut(text, BEAM_TAIL_MIN, BEAM_TAIL_MAX);
  return {
    stable: text.slice(0, cut),
    tail: text.slice(cut),
  };
}
