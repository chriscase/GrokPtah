/**
 * Small, dependency-free syntax highlighter.
 *
 * Deliberately modest: it colours comments, strings, numbers, and keywords,
 * and leaves everything else alone. That is enough to make source readable
 * without pulling a grammar engine into the desktop bundle, and it degrades
 * to plain text for any language it does not know.
 *
 * Block comments and multi-line strings are handled by threading a small
 * carry state from line to line, so a viewer can highlight a window of a file
 * without re-tokenising from the top every render.
 *
 * Pure and browser-safe: strings in, tokens out. It never produces HTML, so
 * it cannot be an injection vector — React renders the token text as text.
 */

/** Token classes the stylesheet knows how to colour. */
export type HighlightKind = "plain" | "comment" | "string" | "number" | "keyword";

/** A run of characters sharing one class. */
export interface HighlightToken {
  kind: HighlightKind;
  text: string;
}

/** Carry state between lines: which multi-line construct is still open. */
export interface HighlightState {
  inBlockComment: boolean;
  /** Delimiter of an open multi-line string, or null. */
  openString: string | null;
}

export const INITIAL_HIGHLIGHT_STATE: HighlightState = {
  inBlockComment: false,
  openString: null,
};

interface LanguageRules {
  lineComments: string[];
  blockComment: [string, string] | null;
  /** Quote delimiters. Multi-char entries (```) are matched first. */
  strings: string[];
  /** Delimiters that may span lines. */
  multilineStrings: string[];
  escape: string | null;
  keywords: ReadonlySet<string>;
  numbers: boolean;
}

function keywords(list: string): ReadonlySet<string> {
  return new Set(list.split(/\s+/).filter(Boolean));
}

const C_LIKE_STRINGS = ['"', "'", "`"];

const RUST: LanguageRules = {
  lineComments: ["//"],
  blockComment: ["/*", "*/"],
  strings: ['"'],
  multilineStrings: ['"'],
  escape: "\\",
  numbers: true,
  keywords: keywords(`as async await break const continue crate dyn else enum extern false fn for
    if impl in let loop match mod move mut pub ref return self Self static struct super trait true
    type unsafe use where while union macro_rules`),
};

const TYPESCRIPT: LanguageRules = {
  lineComments: ["//"],
  blockComment: ["/*", "*/"],
  strings: C_LIKE_STRINGS,
  multilineStrings: ["`"],
  escape: "\\",
  numbers: true,
  keywords: keywords(`abstract any as async await bigint boolean break case catch class const
    constructor continue declare default delete do else enum export extends false finally for from
    function get if implements import in infer instanceof interface is keyof let namespace never
    new null number object of private protected public readonly return satisfies set static string
    super switch symbol this throw true try type typeof undefined unique unknown var void while
    with yield`),
};

const PYTHON: LanguageRules = {
  lineComments: ["#"],
  blockComment: null,
  strings: ['"""', "'''", '"', "'"],
  multilineStrings: ['"""', "'''"],
  escape: "\\",
  numbers: true,
  keywords: keywords(`and as assert async await break class continue def del elif else except
    False finally for from global if import in is lambda None nonlocal not or pass raise return
    True try while with yield match case`),
};

const GO: LanguageRules = {
  lineComments: ["//"],
  blockComment: ["/*", "*/"],
  strings: ['"', "'", "`"],
  multilineStrings: ["`"],
  escape: "\\",
  numbers: true,
  keywords: keywords(`break case chan const continue default defer else fallthrough for func go
    goto if import interface map package range return select struct switch type var nil true false`),
};

const SHELL: LanguageRules = {
  lineComments: ["#"],
  blockComment: null,
  strings: ['"', "'"],
  multilineStrings: [],
  escape: "\\",
  numbers: false,
  keywords: keywords(`if then elif else fi for while until do done case esac function in return
    local export readonly set unset shift source echo exit trap`),
};

const JSON_RULES: LanguageRules = {
  lineComments: [],
  blockComment: null,
  strings: ['"'],
  multilineStrings: [],
  escape: "\\",
  numbers: true,
  keywords: keywords("true false null"),
};

const TOML_RULES: LanguageRules = {
  lineComments: ["#"],
  blockComment: null,
  strings: ['"""', "'''", '"', "'"],
  multilineStrings: ['"""', "'''"],
  escape: "\\",
  numbers: true,
  keywords: keywords("true false"),
};

const YAML_RULES: LanguageRules = {
  lineComments: ["#"],
  blockComment: null,
  strings: ['"', "'"],
  multilineStrings: [],
  escape: "\\",
  numbers: true,
  keywords: keywords("true false null yes no on off"),
};

const CSS_RULES: LanguageRules = {
  lineComments: [],
  blockComment: ["/*", "*/"],
  strings: ['"', "'"],
  multilineStrings: [],
  escape: "\\",
  numbers: true,
  keywords: keywords("important media supports keyframes from to and not only"),
};

const C_FAMILY: LanguageRules = {
  lineComments: ["//"],
  blockComment: ["/*", "*/"],
  strings: ['"', "'"],
  multilineStrings: [],
  escape: "\\",
  numbers: true,
  keywords: keywords(`auto bool break case char class const constexpr continue default delete do
    double else enum extern false float for goto if inline int long namespace new nullptr operator
    private protected public register return short signed sizeof static struct switch template
    this throw true try typedef typename union unsigned using virtual void volatile while`),
};

const PLAIN: LanguageRules = {
  lineComments: [],
  blockComment: null,
  strings: [],
  multilineStrings: [],
  escape: null,
  numbers: false,
  keywords: new Set<string>(),
};

const LANGUAGES: Record<string, LanguageRules> = {
  rust: RUST,
  typescript: TYPESCRIPT,
  tsx: TYPESCRIPT,
  javascript: TYPESCRIPT,
  jsx: TYPESCRIPT,
  python: PYTHON,
  go: GO,
  shell: SHELL,
  json: JSON_RULES,
  toml: TOML_RULES,
  yaml: YAML_RULES,
  css: CSS_RULES,
  c: C_FAMILY,
  cpp: C_FAMILY,
  swift: C_FAMILY,
  sql: { ...C_FAMILY, lineComments: ["--"] },
};

/** Whether a language has real highlighting rules. */
export function isHighlightedLanguage(language: string): boolean {
  return Object.prototype.hasOwnProperty.call(LANGUAGES, language);
}

function rulesFor(language: string): LanguageRules {
  return LANGUAGES[language] ?? PLAIN;
}

const WORD = /[A-Za-z_$][A-Za-z0-9_$]*/y;
const NUMBER = /(?:0[xXbBoO][0-9a-fA-F_]+|\d[\d_]*(?:\.[\d_]+)?(?:[eE][+-]?\d+)?)[a-zA-Z_]*/y;

function push(tokens: HighlightToken[], kind: HighlightKind, text: string): void {
  if (!text) return;
  const last = tokens[tokens.length - 1];
  if (last && last.kind === kind) {
    last.text += text;
    return;
  }
  tokens.push({ kind, text });
}

/**
 * Tokenise one line, continuing from `state` and returning the state the
 * next line should start in.
 */
export function highlightLine(
  line: string,
  language: string,
  state: HighlightState = INITIAL_HIGHLIGHT_STATE,
): { tokens: HighlightToken[]; state: HighlightState } {
  const rules = rulesFor(language);
  const tokens: HighlightToken[] = [];
  let inBlockComment = state.inBlockComment;
  let openString = state.openString;
  let index = 0;

  while (index < line.length) {
    if (inBlockComment && rules.blockComment) {
      const close = line.indexOf(rules.blockComment[1], index);
      if (close === -1) {
        push(tokens, "comment", line.slice(index));
        index = line.length;
        break;
      }
      push(tokens, "comment", line.slice(index, close + rules.blockComment[1].length));
      index = close + rules.blockComment[1].length;
      inBlockComment = false;
      continue;
    }

    if (openString) {
      const consumed = consumeString(line, index, openString, rules.escape);
      push(tokens, "string", consumed.text);
      index = consumed.index;
      openString = consumed.closed ? null : openString;
      if (!consumed.closed) break;
      continue;
    }

    const lineComment = rules.lineComments.find((marker) => line.startsWith(marker, index));
    if (lineComment) {
      push(tokens, "comment", line.slice(index));
      index = line.length;
      break;
    }

    if (rules.blockComment && line.startsWith(rules.blockComment[0], index)) {
      inBlockComment = true;
      push(tokens, "comment", rules.blockComment[0]);
      index += rules.blockComment[0].length;
      continue;
    }

    const delimiter = rules.strings.find((quote) => line.startsWith(quote, index));
    if (delimiter) {
      push(tokens, "string", delimiter);
      const consumed = consumeString(line, index + delimiter.length, delimiter, rules.escape);
      push(tokens, "string", consumed.text);
      index = consumed.index;
      if (!consumed.closed) {
        openString = rules.multilineStrings.includes(delimiter) ? delimiter : null;
        break;
      }
      continue;
    }

    const character = line.charAt(index);
    if (rules.numbers && character >= "0" && character <= "9") {
      NUMBER.lastIndex = index;
      const match = NUMBER.exec(line);
      if (match) {
        push(tokens, "number", match[0]);
        index += match[0].length;
        continue;
      }
    }

    WORD.lastIndex = index;
    const word = WORD.exec(line);
    if (word) {
      push(tokens, rules.keywords.has(word[0]) ? "keyword" : "plain", word[0]);
      index += word[0].length;
      continue;
    }

    push(tokens, "plain", character);
    index += 1;
  }

  return { tokens, state: { inBlockComment, openString } };
}

function consumeString(
  line: string,
  start: number,
  delimiter: string,
  escape: string | null,
): { text: string; index: number; closed: boolean } {
  let index = start;
  while (index < line.length) {
    if (escape && line.startsWith(escape, index)) {
      index += escape.length + 1;
      continue;
    }
    if (line.startsWith(delimiter, index)) {
      return { text: line.slice(start, index + delimiter.length), index: index + delimiter.length, closed: true };
    }
    index += 1;
  }
  return { text: line.slice(start), index: line.length, closed: false };
}

/** Tokenise a whole window of lines, threading block state through it. */
export function highlightLines(lines: string[], language: string): HighlightToken[][] {
  let state = INITIAL_HIGHLIGHT_STATE;
  return lines.map((line) => {
    const result = highlightLine(line, language, state);
    state = result.state;
    return result.tokens;
  });
}
