/**
 * Split streaming markdown into a "stable" prefix (safe to fully parse) and a
 * "tail" (incomplete / in-flight — show with beam effect).
 */

export type StreamMarkdownSplit = {
  stable: string;
  tail: string;
};

/** Min chars kept in the beam tail for a visible “arriving” region. */
const BEAM_TAIL_MIN = 28;
const BEAM_TAIL_MAX = 96;

/**
 * Find start index of an unclosed ``` fence, or -1 if all fences closed.
 */
export function unclosedFenceStart(text: string): number {
  let inFence = false;
  let fenceAt = -1;
  let offset = 0;
  const lines = text.split("\n");
  for (const line of lines) {
    // Opening/closing fence: line starts with ```
    if (/^```/.test(line)) {
      if (!inFence) {
        inFence = true;
        fenceAt = offset;
      } else {
        inFence = false;
        fenceAt = -1;
      }
    }
    offset += line.length + 1; // + newline (last line may overcount by 1 — ok)
  }
  return inFence ? fenceAt : -1;
}

/**
 * If the text ends with an incomplete GFM table, return the index where that
 * table fragment starts so it stays in the beam tail until more rows arrive.
 */
export function incompleteTableStart(text: string): number {
  const lines = text.split("\n");
  // Walk back over trailing table-ish lines
  let i = lines.length - 1;
  // Skip trailing empty
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
  while (start > 0 && (isTableLine(lines[start - 1]) || isSep(lines[start - 1]))) {
    start--;
  }

  // Collect the table region
  const region = lines.slice(start, i + 1);
  const hasSep = region.some(isSep);
  const dataRows = region.filter((l) => isTableLine(l) && !isSep(l));

  // Incomplete: header only, or header+partial without separator, or
  // separator with no data row yet
  if (!hasSep || dataRows.length < 2) {
    // char offset of start line
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
  // Prefer sentence / clause boundary
  for (let i = maxCut; i >= minCut; i--) {
    const ch = text[i - 1];
    if (ch === "\n") return i;
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
 */
export function splitStreamingMarkdown(text: string): StreamMarkdownSplit {
  if (!text) return { stable: "", tail: "" };

  // 1) Unclosed code fence → everything from fence is tail
  const fenceAt = unclosedFenceStart(text);
  if (fenceAt >= 0) {
    return {
      stable: text.slice(0, fenceAt),
      tail: text.slice(fenceAt),
    };
  }

  // 2) Incomplete trailing table → keep table in tail
  const tableAt = incompleteTableStart(text);
  if (tableAt >= 0) {
    return {
      stable: text.slice(0, tableAt),
      tail: text.slice(tableAt),
    };
  }

  // 3) Prefer last blank-line boundary (complete blocks stay stable)
  const dbl = text.lastIndexOf("\n\n");
  if (dbl > 0 && dbl < text.length - 1) {
    let stable = text.slice(0, dbl + 2);
    let tail = text.slice(dbl + 2);
    // If tail is huge, pull more into stable via beam cut inside tail
    if (tail.length > BEAM_TAIL_MAX * 2) {
      const cut = lastSafeCut(tail, BEAM_TAIL_MIN, BEAM_TAIL_MAX);
      stable += tail.slice(0, cut);
      tail = tail.slice(cut);
    }
    return { stable, tail };
  }

  // 4) Single block: keep a small beamed tail so new tokens glow
  if (text.length <= BEAM_TAIL_MIN + 8) {
    return { stable: "", tail: text };
  }
  const cut = lastSafeCut(text, BEAM_TAIL_MIN, BEAM_TAIL_MAX);
  return {
    stable: text.slice(0, cut),
    tail: text.slice(cut),
  };
}
