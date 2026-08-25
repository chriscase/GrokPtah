import { describe, expect, it } from "vitest";
import {
  INITIAL_HIGHLIGHT_STATE,
  highlightLine,
  highlightLines,
  isHighlightedLanguage,
  type HighlightToken,
} from "./sourceHighlight";

/** Reassembling tokens must always give back the original line. */
function rejoin(tokens: HighlightToken[]): string {
  return tokens.map((token) => token.text).join("");
}

function kinds(tokens: HighlightToken[]): string[] {
  return tokens.map((token) => token.kind);
}

describe("highlightLine", () => {
  it("is lossless for every language it knows", () => {
    const line = 'const x = "a\\"b"; // note /* not a block */';
    for (const language of ["rust", "typescript", "python", "go", "shell", "json", "toml", "plain"]) {
      expect(rejoin(highlightLine(line, language).tokens)).toBe(line);
    }
  });

  it("is lossless for lines with tabs, unicode, and lone quotes", () => {
    for (const line of ["\tlet café = 1;", "it's fine", "emoji 🎯 here", "", "   "]) {
      expect(rejoin(highlightLine(line, "typescript").tokens)).toBe(line);
    }
  });

  it("marks keywords, strings, numbers, and comments in Rust", () => {
    const { tokens } = highlightLine('let n = 42; // count', "rust");
    const byKind = (kind: string) =>
      tokens.filter((t) => t.kind === kind).map((t) => t.text.trim());
    expect(byKind("keyword")).toContain("let");
    expect(byKind("number")).toContain("42");
    expect(byKind("comment")).toContain("// count");
  });

  it("does not treat an identifier containing a keyword as a keyword", () => {
    const { tokens } = highlightLine("letter = 1", "rust");
    expect(kinds(tokens)).not.toContain("keyword");
  });

  it("keeps an escaped quote inside the string", () => {
    const line = 'const s = "a\\"b";';
    const { tokens } = highlightLine(line, "typescript");
    const strings = tokens.filter((t) => t.kind === "string").map((t) => t.text);
    expect(strings.join("")).toBe('"a\\"b"');
    expect(rejoin(tokens)).toBe(line);
  });

  it("carries an unterminated block comment to the next line", () => {
    const first = highlightLine("/* open", "rust");
    expect(first.state.inBlockComment).toBe(true);
    expect(kinds(first.tokens)).toEqual(["comment"]);

    const second = highlightLine("still comment */ let x = 1;", "rust", first.state);
    expect(second.state.inBlockComment).toBe(false);
    expect(second.tokens[0].kind).toBe("comment");
    expect(second.tokens.some((t) => t.kind === "keyword" && t.text === "let")).toBe(true);
  });

  it("closes a block comment opened and closed on the same line", () => {
    const { tokens, state } = highlightLine("let a /* mid */ = 1;", "rust");
    expect(state.inBlockComment).toBe(false);
    expect(tokens.some((t) => t.kind === "comment" && t.text === "/* mid */")).toBe(true);
  });

  it("carries a multi-line template string but not a broken single quote", () => {
    const template = highlightLine("const t = `open", "typescript");
    expect(template.state.openString).toBe("`");

    const broken = highlightLine("const t = 'open", "typescript");
    expect(broken.state.openString).toBeNull();
  });

  it("closes a carried multi-line string on a later line", () => {
    const first = highlightLine("const t = `line one", "typescript");
    const second = highlightLine("line two` + x", "typescript", first.state);
    expect(second.state.openString).toBeNull();
    expect(second.tokens[0].kind).toBe("string");
    expect(rejoin(second.tokens)).toBe("line two` + x");
  });

  it("handles Python triple-quoted strings across lines", () => {
    const first = highlightLine('doc = """start', "python");
    expect(first.state.openString).toBe('"""');
    const second = highlightLine('end""" # done', "python", first.state);
    expect(second.state.openString).toBeNull();
    expect(second.tokens.some((t) => t.kind === "comment")).toBe(true);
  });

  it("uses the SQL line comment marker", () => {
    const { tokens } = highlightLine("select 1 -- why", "sql");
    expect(tokens.some((t) => t.kind === "comment" && t.text === "-- why")).toBe(true);
  });

  it("leaves an unknown language entirely plain", () => {
    const line = 'let x = "y"; // z';
    const { tokens } = highlightLine(line, "brainfuck");
    expect(kinds(tokens)).toEqual(["plain"]);
    expect(rejoin(tokens)).toBe(line);
  });

  it("reports which languages have rules", () => {
    expect(isHighlightedLanguage("rust")).toBe(true);
    expect(isHighlightedLanguage("plain")).toBe(false);
    expect(isHighlightedLanguage("markdown")).toBe(false);
  });

  it("recognises hex, binary, and exponent numbers", () => {
    for (const literal of ["0xFF", "0b1010", "1_000", "1.5e-3"]) {
      const { tokens } = highlightLine(`x = ${literal}`, "rust");
      expect(tokens.some((t) => t.kind === "number" && t.text === literal)).toBe(true);
    }
  });

  it("does not treat a digit inside an identifier as a number", () => {
    const { tokens } = highlightLine("var a1 = 2", "typescript");
    const plain = tokens.filter((t) => t.kind === "plain").map((t) => t.text);
    expect(plain.join("")).toContain("a1");
  });

  it("starts from the documented initial state by default", () => {
    expect(INITIAL_HIGHLIGHT_STATE).toEqual({ inBlockComment: false, openString: null });
  });
});

describe("highlightLines", () => {
  it("threads block state across a window", () => {
    const rows = highlightLines(["/* a", "b", "c */ let d = 1;"], "rust");
    expect(kinds(rows[0])).toEqual(["comment"]);
    expect(kinds(rows[1])).toEqual(["comment"]);
    expect(rows[2].some((t) => t.kind === "keyword" && t.text === "let")).toBe(true);
  });

  it("is lossless line by line", () => {
    const lines = ["fn main() {", '    println!("hi");', "}"];
    expect(highlightLines(lines, "rust").map(rejoin)).toEqual(lines);
  });

  it("returns one token row per input line, including empties", () => {
    expect(highlightLines(["a", "", "b"], "rust")).toHaveLength(3);
  });
});
