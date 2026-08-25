//! Bounded range and cursor reads.
//!
//! Every projection this module returns is explicitly bounded and explicitly
//! honest about its bounds. A chunk says how many bytes it consumed, where to
//! resume, whether the classification it reports covers the whole file or only
//! the prefix that was scanned, and which kind of identity it can vouch for.
//! Nothing is described as complete unless it was read completely.

use std::io::{Read, Seek, SeekFrom};

use crate::digest::to_hex;
use crate::error::SourceViewError;
use crate::identity::IdentityStability;
use crate::open::OpenedDocument;
use crate::utf8::Utf8Decoder;

/// Largest file this boundary will project at all.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest single chunk.
pub const MAX_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
/// Largest number of lines in one chunk.
pub const MAX_CHUNK_LINES: usize = 5_000;
/// Widest rendered line.
pub const MAX_LINE_CHARS: usize = 10_000;
/// Above this size the whole-content digest is skipped for a pinned identity.
pub const CONTENT_DIGEST_BUDGET: u64 = 16 * 1024 * 1024;
/// How much of a file is scanned before classifying it text or binary.
pub const BINARY_SCAN_BYTES: u64 = 1024 * 1024;

const DEFAULT_CHUNK_BYTES: u64 = 512 * 1024;
const DEFAULT_CHUNK_LINES: usize = 1_200;
const DEFAULT_LINE_CHARS: usize = 2_000;

/// Caller-requested bounds, before clamping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestedLimits {
    pub max_bytes: Option<u64>,
    pub max_lines: Option<usize>,
    pub max_line_chars: Option<usize>,
}

/// Bounds actually applied, echoed back so a caller never has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveLimits {
    pub max_bytes: u64,
    pub max_lines: usize,
    pub max_line_chars: usize,
}

impl EffectiveLimits {
    /// Clamp caller input into the supported window.
    ///
    /// `Some(0)` and `None` both mean "use the default"; a value above a
    /// ceiling is lowered, never honoured. Nothing here can widen a bound.
    pub fn clamp(requested: RequestedLimits) -> Self {
        Self {
            max_bytes: clamp_u64(requested.max_bytes, DEFAULT_CHUNK_BYTES, 1, MAX_CHUNK_BYTES),
            max_lines: clamp_usize(requested.max_lines, DEFAULT_CHUNK_LINES, 1, MAX_CHUNK_LINES),
            max_line_chars: clamp_usize(
                requested.max_line_chars,
                DEFAULT_LINE_CHARS,
                16,
                MAX_LINE_CHARS,
            ),
        }
    }
}

fn clamp_u64(value: Option<u64>, fallback: u64, low: u64, high: u64) -> u64 {
    match value {
        None | Some(0) => fallback,
        Some(found) => found.clamp(low, high),
    }
}

fn clamp_usize(value: Option<usize>, fallback: usize, low: usize, high: usize) -> usize {
    match value {
        None | Some(0) => fallback,
        Some(found) => found.clamp(low, high),
    }
}

/// What a scan concluded about a file's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentVerdict {
    /// Valid UTF-8 with no NUL.
    Text,
    /// No NUL, but some bytes are not valid UTF-8 and decode lossily.
    TextLossy,
    /// Contains NUL. Never rendered as text.
    Binary,
}

/// A classification together with the evidence behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentClass {
    pub verdict: ContentVerdict,
    /// How many bytes the classifier actually looked at.
    pub scanned_bytes: u64,
    /// True when `scanned_bytes` covered the whole file. When false the
    /// verdict describes the scanned prefix and nothing more.
    pub complete_scan: bool,
}

/// How strongly the boundary can vouch that two reads saw the same document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocumentIdentity {
    /// BLAKE3-256 over the entire file.
    Content { digest: String },
    /// Handle identity only, because the file is larger than the digest
    /// budget. Distinct content can share a pinned identity if the filesystem
    /// reuses a node and preserves every observed field.
    Pinned {
        digest: String,
        stability: IdentityStability,
    },
}

impl DocumentIdentity {
    pub fn digest(&self) -> &str {
        match self {
            Self::Content { digest } | Self::Pinned { digest, .. } => digest,
        }
    }
}

/// Line-ending shape of one chunk.
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
    pub truncated: bool,
}

/// Where to resume a paged read.
///
/// Deserialisable because it round-trips through the caller: the boundary
/// hands one out and takes it back. Every field is re-validated on the way in
/// — a cursor is a hint about where to resume, never an authority to read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCursor {
    pub byte_offset: u64,
    /// Number of the first line the next chunk will emit. When
    /// `continues_line` is set this is the number of the line the previous
    /// chunk left unfinished, and the next chunk's first entry continues it.
    pub next_line_number: usize,
    /// Hex of the partial UTF-8 sequence held back at the split, if any.
    pub carry_hex: String,
    /// True when the previous chunk ended mid-line.
    pub continues_line: bool,
    /// Identity this cursor was minted against; a cursor is refused against
    /// any other document.
    pub document_digest: String,
}

/// One bounded slice of a document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceChunk {
    pub lines: Vec<SourceLine>,
    pub start_byte: u64,
    pub bytes_consumed: u64,
    pub lossy_replacements: usize,
    pub eol: Eol,
    /// True when this chunk's first line continues the previous chunk's last
    /// line: they share a line number and the consumer concatenates them.
    pub continues_previous: bool,
    /// True when this chunk's last line is unfinished and the next chunk
    /// continues it under the same number.
    pub continues_next: bool,
    /// Absent once the read reached the end of the file.
    pub next_cursor: Option<ReadCursor>,
    pub eof: bool,
}

/// A complete read result: what the file is, and the slice that was read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentProjection {
    pub byte_len: u64,
    pub content: ContentClass,
    pub identity: DocumentIdentity,
    pub limits: EffectiveLimits,
    pub chunk: SourceChunk,
}

/// Read one bounded slice of an opened document.
///
/// `cursor` resumes a previous read; `start_byte` starts a new one. A cursor
/// minted against different content is refused rather than silently reseated.
pub fn read_projection(
    opened: &OpenedDocument,
    start_byte: u64,
    cursor: Option<&ReadCursor>,
    requested: RequestedLimits,
) -> Result<DocumentProjection, SourceViewError> {
    let byte_len = opened.identity.len;
    if byte_len > MAX_DOCUMENT_BYTES {
        return Err(SourceViewError::TooLarge {
            byte_len,
            max_bytes: MAX_DOCUMENT_BYTES,
        });
    }
    let limits = EffectiveLimits::clamp(requested);

    let identity = document_identity(opened, byte_len)?;
    if cursor.is_some_and(|cursor| cursor.document_digest != identity.digest()) {
        return Err(SourceViewError::CursorInvalid);
    }

    let offset = cursor.map_or(start_byte, |cursor| cursor.byte_offset);
    if offset > byte_len {
        return Err(SourceViewError::RangeInvalid);
    }
    let first_line = cursor.map_or(1, |cursor| cursor.next_line_number);
    if first_line == 0 {
        return Err(SourceViewError::CursorInvalid);
    }
    let carry = match cursor {
        Some(cursor) => {
            crate::digest::from_hex(&cursor.carry_hex).ok_or(SourceViewError::CursorInvalid)?
        }
        None => Vec::new(),
    };

    let content = classify(opened, byte_len)?;
    if content.verdict == ContentVerdict::Binary {
        return Ok(DocumentProjection {
            byte_len,
            content,
            identity,
            limits,
            chunk: SourceChunk {
                lines: Vec::new(),
                start_byte: offset,
                bytes_consumed: 0,
                lossy_replacements: 0,
                eol: Eol::None,
                continues_previous: false,
                continues_next: false,
                next_cursor: None,
                eof: true,
            },
        });
    }

    let chunk = read_chunk(
        opened,
        offset,
        first_line,
        cursor.is_some_and(|cursor| cursor.continues_line),
        carry,
        limits,
        byte_len,
        &identity,
    )?;
    opened.validate_unchanged()?;
    Ok(DocumentProjection {
        byte_len,
        content,
        identity,
        limits,
        chunk,
    })
}

/// Whole-content digest when the file fits the budget, pinned identity when it
/// does not. The choice is reported, never inferred by the caller.
fn document_identity(
    opened: &OpenedDocument,
    byte_len: u64,
) -> Result<DocumentIdentity, SourceViewError> {
    if byte_len > CONTENT_DIGEST_BUDGET {
        return Ok(DocumentIdentity::Pinned {
            digest: opened.identity.digest(),
            stability: opened.identity.stability(),
        });
    }
    let mut file = &opened.file;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| SourceViewError::io(&error))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| SourceViewError::io(&error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(DocumentIdentity::Content {
        digest: to_hex(hasher.finalize().as_bytes()),
    })
}

/// Classify from a bounded prefix, and say how much was actually examined.
fn classify(opened: &OpenedDocument, byte_len: u64) -> Result<ContentClass, SourceViewError> {
    let budget = BINARY_SCAN_BYTES.min(byte_len);
    let mut file = &opened.file;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| SourceViewError::io(&error))?;
    let mut sample = vec![0u8; usize::try_from(budget).unwrap_or(usize::MAX)];
    let mut filled = 0usize;
    while filled < sample.len() {
        let read = file
            .read(&mut sample[filled..])
            .map_err(|error| SourceViewError::io(&error))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    sample.truncate(filled);
    let scanned_bytes = filled as u64;
    let complete_scan = scanned_bytes >= byte_len;

    if sample.contains(&0) {
        return Ok(ContentClass {
            verdict: ContentVerdict::Binary,
            scanned_bytes,
            complete_scan,
        });
    }
    // A sample cut mid-character is not evidence of invalid UTF-8, so an
    // incomplete trailing sequence is ignored rather than counted against it.
    let verdict = match std::str::from_utf8(&sample) {
        Ok(_) => ContentVerdict::Text,
        Err(error) if error.error_len().is_none() && !complete_scan => ContentVerdict::Text,
        Err(_) => ContentVerdict::TextLossy,
    };
    Ok(ContentClass {
        verdict,
        scanned_bytes,
        complete_scan,
    })
}

#[allow(clippy::too_many_arguments)]
fn read_chunk(
    opened: &OpenedDocument,
    offset: u64,
    first_line: usize,
    continues_previous: bool,
    carry: Vec<u8>,
    limits: EffectiveLimits,
    byte_len: u64,
    identity: &DocumentIdentity,
) -> Result<SourceChunk, SourceViewError> {
    let mut file = &opened.file;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| SourceViewError::io(&error))?;

    let want = usize::try_from(limits.max_bytes).unwrap_or(usize::MAX);
    let mut raw = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let read = file
            .read(&mut raw[filled..])
            .map_err(|error| SourceViewError::io(&error))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    raw.truncate(filled);
    let reached_end = offset + filled as u64 >= byte_len;

    // Cap the chunk on the byte offset just past the line-cap-th newline, so
    // the line limit is enforced without decoding first. Newline is ASCII, so
    // this cut can never land inside a character.
    let cut = line_cap_cut(&raw, limits.max_lines).unwrap_or(raw.len());
    let at_eof = reached_end && cut == raw.len();
    let body = &raw[..cut];

    let mut decoder = Utf8Decoder::resume(carry).ok_or(SourceViewError::CursorInvalid)?;
    let decoded = decoder.decode(body, at_eof);
    let eol = detect_eol(&decoded.text);

    // Mid-line is a property of the *bytes*, not of the decoded text: a chunk
    // whose every byte went into the UTF-8 carry decodes to nothing and is
    // still in the middle of a line. Newline is ASCII, so this test is exact.
    let continues_next = !at_eof && !body.ends_with(b"\n");
    // `split_inclusive` yields no trailing empty piece for text that ends in a
    // newline, so a genuine blank line is preserved and a phantom one is not
    // created.
    let pieces: Vec<&str> = decoded.text.split_inclusive('\n').collect();

    let mut lines = Vec::with_capacity(pieces.len());
    for (index, piece) in pieces.iter().enumerate() {
        let stripped = piece.strip_suffix('\n').unwrap_or(piece);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        let (text, truncated) = cut_line(stripped, limits.max_line_chars);
        lines.push(SourceLine {
            number: first_line + index,
            text,
            truncated,
        });
    }
    let bytes_consumed = cut as u64;
    let next_offset = offset + bytes_consumed;
    let carry_out = decoder.carry().to_vec();
    let finished = next_offset >= byte_len && carry_out.is_empty();
    // When the chunk ends mid-line the next chunk resumes *that* line number.
    let emitted = lines.len();
    let next_line_number = if continues_next {
        first_line + emitted.saturating_sub(1)
    } else {
        first_line + emitted
    };
    let next_cursor = if finished {
        None
    } else {
        Some(ReadCursor {
            byte_offset: next_offset,
            next_line_number: next_line_number.max(1),
            carry_hex: to_hex(&carry_out),
            continues_line: continues_next,
            document_digest: identity.digest().to_string(),
        })
    };

    Ok(SourceChunk {
        lines,
        start_byte: offset,
        bytes_consumed,
        lossy_replacements: decoded.replacements,
        eol,
        continues_previous,
        continues_next,
        next_cursor,
        eof: finished,
    })
}

/// Byte offset just past the `max_lines`-th newline, when there are that many.
fn line_cap_cut(raw: &[u8], max_lines: usize) -> Option<usize> {
    let mut seen = 0usize;
    for (index, byte) in raw.iter().enumerate() {
        if *byte == b'\n' {
            seen += 1;
            if seen == max_lines {
                return Some(index + 1);
            }
        }
    }
    None
}

/// Cut on a character boundary so a wide line never yields invalid UTF-8.
fn cut_line(line: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    for (count, character) in line.chars().enumerate() {
        if count >= max_chars {
            return (out, true);
        }
        out.push(character);
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

/// Accumulates paged chunks back into whole lines.
///
/// A chunk may end mid-line; the next chunk continues that line under the same
/// number. Rather than leaving every consumer to rediscover that rule, this is
/// the one implementation of it, mirrored by `appendSourceChunk` on the
/// TypeScript side and asserted equivalent by contract test.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineAssembler {
    lines: Vec<SourceLine>,
}

impl LineAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one chunk, joining a continued line to the line it continues.
    pub fn push_chunk(&mut self, chunk: &SourceChunk) {
        let mut incoming = chunk.lines.iter();
        if chunk.continues_previous
            && let (Some(tail), Some(head)) = (self.lines.last_mut(), incoming.clone().next())
            && tail.number == head.number
        {
            tail.text.push_str(&head.text);
            tail.truncated |= head.truncated;
            incoming.next();
        }
        self.lines.extend(incoming.cloned());
    }

    pub fn lines(&self) -> &[SourceLine] {
        &self.lines
    }

    pub fn finish(self) -> Vec<SourceLine> {
        self.lines
    }
}
