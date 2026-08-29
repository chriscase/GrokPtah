//! Strip a raw provider reply down to plain text the host is willing to render.
//!
//! Every rule here removes a way that reply text could act rather than inform:
//!
//! * **Markup** is removed because a reply is rendered as text. An anchor, an
//!   image, or a code fence in what the UI treats as prose is a way to make the
//!   surface do something the validator did not sanction.
//! * **Control characters** are removed because a carriage return or an escape
//!   sequence can rewrite what a terminal or log already printed, so a receipt
//!   or a transcript could be made to read differently from what happened.
//! * **Bidirectional overrides** are removed because they reorder rendered text
//!   without changing its bytes: a citation can be made to display as its own
//!   opposite while still matching the corpus byte for byte.
//! * **Secrets and paths** are removed because a provider reply is the one
//!   place in this system where text of unknown origin reaches a person. If the
//!   model reproduces a credential or a home directory, that is the moment it
//!   would escape.
//!
//! Each rule reports a count. The counts reach the receipt; the removed text
//! never does.

use grokptah_help_contract::dto::RedactionKind;

/// The marker left where something was removed.
pub const PLACEHOLDER: &str = "[redacted]";

/// A redacted string and what was taken out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub counts: Vec<(RedactionKind, usize)>,
}

impl Redacted {
    #[must_use]
    pub fn kinds(&self) -> Vec<RedactionKind> {
        let mut kinds: Vec<RedactionKind> = self
            .counts
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(kind, _)| *kind)
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.counts.iter().map(|(_, count)| count).sum()
    }
}

fn is_bidi(character: char) -> bool {
    matches!(
        character,
        '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{061C}'
    )
}

fn is_disallowed_control(character: char) -> bool {
    if character == '\n' || character == '\t' {
        return false;
    }
    character.is_control() || ('\u{80}'..='\u{9F}').contains(&character)
}

/// Whether a token looks like a credential rather than a word.
///
/// Deliberately shape-based rather than a vendor list: a rule that enumerates
/// known prefixes fails on the next provider. Length plus entropy-ish mixing
/// catches the general case, and the well-known prefixes are caught early.
fn looks_like_secret(token: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "ASIA",
    ];
    if PREFIXES.iter().any(|prefix| token.starts_with(prefix)) {
        return true;
    }
    // A long run of base64/hex-ish characters with both cases and digits.
    let body: String = token
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    if body.len() < 24 {
        return false;
    }
    let has_upper = body.chars().any(|character| character.is_ascii_uppercase());
    let has_lower = body.chars().any(|character| character.is_ascii_lowercase());
    let has_digit = body.chars().any(|character| character.is_ascii_digit());
    let non_alnum = token
        .chars()
        .filter(|character| !character.is_ascii_alphanumeric())
        .count();
    has_upper && has_lower && has_digit && non_alnum <= 2
}

/// Whether a token is an absolute or home-relative filesystem path.
fn looks_like_path(token: &str) -> bool {
    let trimmed = token.trim_matches(|character: char| matches!(character, '.' | ',' | ')' | '('));
    if trimmed.len() < 4 {
        return false;
    }
    if trimmed.starts_with("~/") {
        return true;
    }
    if trimmed.starts_with('/') && trimmed[1..].contains('/') {
        return true;
    }
    // Windows drive path.
    let bytes = trimmed.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Remove markup: fenced code, inline code, HTML tags, and link targets.
fn strip_markup(input: &str) -> (String, usize) {
    let mut out = String::with_capacity(input.len());
    let mut count = 0usize;
    let mut rest = input;

    // Fenced code blocks, including an unterminated trailing fence.
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        count += 1;
        let after = &rest[start + 3..];
        match after.find("```") {
            Some(end) => rest = &after[end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);

    // Markdown links: keep the label, drop the target.
    let mut linked = String::with_capacity(out.len());
    let mut chars = out.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if character == '['
            && let Some(close) = out[index..].find("](")
            && let Some(end) = out[index + close..].find(')')
        {
            linked.push_str(&out[index + 1..index + close]);
            count += 1;
            // Skip past the whole link.
            let skip_to = index + close + end + 1;
            while let Some((next, _)) = chars.peek() {
                if *next < skip_to {
                    chars.next();
                } else {
                    break;
                }
            }
            continue;
        }
        linked.push(character);
    }

    // HTML tags and remaining inline code / emphasis markers.
    let mut result = String::with_capacity(linked.len());
    let mut in_tag = false;
    for character in linked.chars() {
        match character {
            '<' => {
                in_tag = true;
                count += 1;
            }
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '`' | '*' | '_' | '#' => count += 1,
            other => result.push(other),
        }
    }
    (result, count)
}

/// Reduce a raw provider reply to plain, non-acting text.
#[must_use]
pub fn redact(input: &str) -> Redacted {
    let (markup_stripped, markup_count) = strip_markup(input);

    let mut control = 0usize;
    let mut bidi = 0usize;
    let mut cleaned = String::with_capacity(markup_stripped.len());
    for character in markup_stripped.chars() {
        if is_bidi(character) {
            bidi += 1;
            continue;
        }
        if is_disallowed_control(character) {
            control += 1;
            continue;
        }
        cleaned.push(character);
    }

    let mut secrets = 0usize;
    let mut paths = 0usize;
    let mut out = String::with_capacity(cleaned.len());
    let mut first = true;
    for token in cleaned.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        let trailing = &token[trimmed.len()..];
        if !first {
            // Whitespace is carried by split_inclusive, so nothing to add.
        }
        first = false;
        if !trimmed.is_empty() && looks_like_secret(trimmed) {
            secrets += 1;
            out.push_str(PLACEHOLDER);
            out.push_str(trailing);
            continue;
        }
        if !trimmed.is_empty() && looks_like_path(trimmed) {
            paths += 1;
            out.push_str(PLACEHOLDER);
            out.push_str(trailing);
            continue;
        }
        out.push_str(token);
    }

    // Collapse whitespace runs the removals left behind.
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");

    Redacted {
        text: collapsed,
        counts: vec![
            (RedactionKind::Secret, secrets),
            (RedactionKind::Path, paths),
            (RedactionKind::Control, control),
            (RedactionKind::Bidi, bidi),
            (RedactionKind::Markup, markup_count),
        ],
    }
}
