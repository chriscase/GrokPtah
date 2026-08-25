//! Bounded, read-only document projection.
//!
//! The reader never streams an unbounded file into the UI: it takes at most
//! `max_bytes + 1` bytes so it can *prove* truncation, caps line count and
//! per-line width, and classifies the payload as UTF-8, lossy UTF-8, or
//! binary before any of it reaches the view layer.

use std::io::Read;

use crate::error::SourceViewError;
use crate::root::{ResolvedSource, RootKind, fnv1a64};

/// Hard ceilings. Caller-supplied limits are clamped into these so a
/// malformed request cannot ask the desktop to buffer an arbitrary file.
pub const MAX_BYTES_CEILING: u64 = 8 * 1024 * 1024;
pub const MAX_LINES_CEILING: usize = 200_000;
pub const MAX_LINE_CHARS_CEILING: usize = 10_000;

const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_LINES: usize = 20_000;
const DEFAULT_MAX_LINE_CHARS: usize = 2_000;

/// Per-request read bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLimits {
    pub max_bytes: u64,
    pub max_lines: usize,
    pub max_line_chars: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: DEFAULT_MAX_LINES,
            max_line_chars: DEFAULT_MAX_LINE_CHARS,
        }
    }
}

impl SourceLimits {
    /// Clamp caller input into the supported window. Zero means "use the
    /// default"; anything above a ceiling is lowered, never honoured.
    pub fn clamped(
        max_bytes: Option<u64>,
        max_lines: Option<usize>,
        max_line_chars: Option<usize>,
    ) -> Self {
        let defaults = Self::default();
        Self {
            max_bytes: clamp_u64(max_bytes, defaults.max_bytes, 1, MAX_BYTES_CEILING),
            max_lines: clamp_usize(max_lines, defaults.max_lines, 1, MAX_LINES_CEILING),
            max_line_chars: clamp_usize(
                max_line_chars,
                defaults.max_line_chars,
                16,
                MAX_LINE_CHARS_CEILING,
            ),
        }
    }
}

fn clamp_u64(value: Option<u64>, fallback: u64, low: u64, high: u64) -> u64 {
    match value {
        None | Some(0) => fallback,
        Some(v) => v.clamp(low, high),
    }
}

fn clamp_usize(value: Option<usize>, fallback: usize, low: usize, high: usize) -> usize {
    match value {
        None | Some(0) => fallback,
        Some(v) => v.clamp(low, high),
    }
}

/// How the bytes decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    /// Valid UTF-8 throughout the window that was read.
    Utf8,
    /// Decoded with replacement characters; shown, but flagged.
    Utf8Lossy,
    /// Contains NUL. Never rendered as text.
    Binary,
}

/// Line-ending shape of the window that was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Eol {
    None,
    Lf,
    Crlf,
    Mixed,
}

/// One rendered line with its real 1-based file line number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLine {
    pub number: usize,
    pub text: String,
    /// True when the line was wider than `max_line_chars` and was cut.
    pub truncated: bool,
}

/// A bounded read-only projection of one file inside one approved root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDocument {
    pub root_id: String,
    pub root_kind: RootKind,
    /// Exact canonical boundary this document was read from.
    pub root_path: String,
    pub root_label: String,
    pub run_id: Option<String>,
    pub relative_path: String,
    pub absolute_path: String,
    /// Lowercase language hint for the highlighter; `"plain"` when unknown.
    pub language: String,
    pub encoding: TextEncoding,
    /// Size on disk, independent of how much was read.
    pub byte_len: u64,
    pub bytes_read: u64,
    pub lines: Vec<SourceLine>,
    /// Lines present in the window that was read, before the line cap.
    pub line_count: usize,
    pub truncated_bytes: bool,
    pub truncated_lines: bool,
    pub lossy_replacements: usize,
    pub eol: Eol,
    /// Fingerprint of the bytes read, so the view and a diff can prove they
    /// are describing the same content.
    pub content_fingerprint: String,
}

/// Read a resolved file into a bounded document.
pub fn read_document(
    resolved: &ResolvedSource,
    limits: SourceLimits,
) -> Result<SourceDocument, SourceViewError> {
    let metadata = std::fs::symlink_metadata(&resolved.absolute_path)?;
    if metadata.file_type().is_symlink() {
        return Err(SourceViewError::SymlinkRejected {
            at: resolved.relative_path.clone(),
        });
    }
    if !metadata.is_file() {
        return Err(SourceViewError::NotAFile {
            at: resolved.relative_path.clone(),
        });
    }
    let byte_len = metadata.len();
    if byte_len > MAX_BYTES_CEILING {
        return Err(SourceViewError::TooLarge {
            byte_len,
            max_bytes: MAX_BYTES_CEILING,
        });
    }

    let file = std::fs::File::open(&resolved.absolute_path)?;
    // Read one byte past the budget so truncation is observed, not inferred.
    let mut buffer = Vec::new();
    file.take(limits.max_bytes.saturating_add(1))
        .read_to_end(&mut buffer)?;
    let truncated_bytes = buffer.len() as u64 > limits.max_bytes;
    if truncated_bytes {
        buffer.truncate(limits.max_bytes as usize);
    }
    let bytes_read = buffer.len() as u64;
    let content_fingerprint = format!("fnv1a64:{:016x}", fnv1a64(&buffer));

    if buffer.contains(&0) {
        return Ok(SourceDocument {
            root_id: resolved.root.id.clone(),
            root_kind: resolved.root.kind,
            root_path: resolved.root.path.display().to_string(),
            root_label: resolved.root.label.clone(),
            run_id: resolved.root.run_id.clone(),
            relative_path: resolved.relative_path.clone(),
            absolute_path: resolved.absolute_path.display().to_string(),
            language: language_for(&resolved.relative_path).to_string(),
            encoding: TextEncoding::Binary,
            byte_len,
            bytes_read,
            lines: Vec::new(),
            line_count: 0,
            truncated_bytes,
            truncated_lines: false,
            lossy_replacements: 0,
            eol: Eol::None,
            content_fingerprint,
        });
    }

    let (text, encoding) = match std::str::from_utf8(&buffer) {
        Ok(valid) => (valid.to_string(), TextEncoding::Utf8),
        Err(_) => (
            String::from_utf8_lossy(&buffer).into_owned(),
            TextEncoding::Utf8Lossy,
        ),
    };
    let lossy_replacements = if encoding == TextEncoding::Utf8Lossy {
        text.matches('\u{FFFD}').count()
    } else {
        0
    };
    // A leading BOM is metadata, not the first character of line 1.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_string();

    let eol = detect_eol(&text);
    let raw_lines: Vec<&str> = split_lines(&text);
    let line_count = raw_lines.len();
    let truncated_lines = line_count > limits.max_lines;
    let lines = raw_lines
        .iter()
        .take(limits.max_lines)
        .enumerate()
        .map(|(index, raw)| {
            let stripped = raw.strip_suffix('\r').unwrap_or(raw);
            let (text, truncated) = cut_line(stripped, limits.max_line_chars);
            SourceLine {
                number: index + 1,
                text,
                truncated,
            }
        })
        .collect();

    Ok(SourceDocument {
        root_id: resolved.root.id.clone(),
        root_kind: resolved.root.kind,
        root_path: resolved.root.path.display().to_string(),
        root_label: resolved.root.label.clone(),
        run_id: resolved.root.run_id.clone(),
        relative_path: resolved.relative_path.clone(),
        absolute_path: resolved.absolute_path.display().to_string(),
        language: language_for(&resolved.relative_path).to_string(),
        encoding,
        byte_len,
        bytes_read,
        lines,
        line_count,
        truncated_bytes,
        truncated_lines,
        lossy_replacements,
        eol,
        content_fingerprint,
    })
}

/// Split into lines without inventing a trailing empty line for a file that
/// simply ends with a newline.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<&str> = text.split('\n').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    parts
}

/// Cut on a char boundary so a wide line never yields invalid UTF-8.
fn cut_line(line: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    for (count, ch) in line.chars().enumerate() {
        if count >= max_chars {
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

fn detect_eol(text: &str) -> Eol {
    let total = text.matches('\n').count();
    if total == 0 {
        return Eol::None;
    }
    let crlf = text.matches("\r\n").count();
    if crlf == 0 {
        Eol::Lf
    } else if crlf == total {
        Eol::Crlf
    } else {
        Eol::Mixed
    }
}

/// Language hint for the highlighter, from the file's extension or name.
pub fn language_for(relative_path: &str) -> &'static str {
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let lowered = name.to_ascii_lowercase();
    match lowered.as_str() {
        "dockerfile" => return "dockerfile",
        "makefile" => return "makefile",
        "cargo.lock" => return "toml",
        _ => {}
    }
    let extension = lowered.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "css" => "css",
        "html" | "htm" => "html",
        "py" => "python",
        "go" => "go",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" => "cpp",
        _ => "plain",
    }
}
