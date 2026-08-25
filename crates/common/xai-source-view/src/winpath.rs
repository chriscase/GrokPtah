//! Windows path-form classification.
//!
//! Windows resolves several path spellings that look like ordinary relative
//! paths but are not: UNC shares, the two device namespaces, drive-relative
//! paths, reserved device names, alternate data streams, and names whose
//! trailing dots or spaces the filesystem silently strips. Each is a way for
//! one request string to reach something other than the file it appears to
//! name, so each is refused.
//!
//! Everything here is pure string analysis with no filesystem access, so the
//! full table is exercised on every platform. The *enforcement* points differ
//! by platform and are documented at each call site.

/// What form a path or segment takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsPathForm {
    /// An ordinary relative path.
    Relative,
    /// `C:\dir\file` — rooted on a specific volume.
    DriveAbsolute,
    /// `C:file` — relative to that drive's *current directory*, which this
    /// process does not control and must never depend on.
    DriveRelative,
    /// `\dir\file` — rooted on the current drive.
    RootRelative,
    /// `\\server\share\...`
    Unc,
    /// `\\?\...` — the extended-length namespace, which bypasses normalisation.
    VerbatimDevice,
    /// `\\.\...` — the device namespace (`\\.\PhysicalDrive0`, `\\.\pipe\…`).
    LocalDevice,
}

impl WindowsPathForm {
    /// Whether this form may appear in a request to read inside a root.
    pub fn is_readable_request(self) -> bool {
        matches!(self, Self::Relative)
    }
}

/// Classify a whole request string.
pub fn classify(path: &str) -> WindowsPathForm {
    let unified: Vec<char> = path
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    let head: String = unified.iter().take(4).collect();
    if head.starts_with("\\\\?\\") {
        return WindowsPathForm::VerbatimDevice;
    }
    if head.starts_with("\\\\.\\") {
        return WindowsPathForm::LocalDevice;
    }
    if unified.len() >= 2 && unified[0] == '\\' && unified[1] == '\\' {
        return WindowsPathForm::Unc;
    }
    if unified.len() >= 2 && unified[0].is_ascii_alphabetic() && unified[1] == ':' {
        return if unified.get(2) == Some(&'\\') {
            WindowsPathForm::DriveAbsolute
        } else {
            WindowsPathForm::DriveRelative
        };
    }
    if unified.first() == Some(&'\\') {
        return WindowsPathForm::RootRelative;
    }
    WindowsPathForm::Relative
}

/// Reserved DOS device names. Reserved with *any* extension and regardless of
/// case, so `nul.txt` and `CON` both reach the device rather than a file.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT0",
    "CONIN$", "CONOUT$",
];

/// Whether one path segment names a reserved device.
pub fn is_reserved_device_name(segment: &str) -> bool {
    let trimmed = segment.trim_end_matches([' ', '.']);
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    let folded = case_fold(stem.trim_end());
    RESERVED.contains(&folded.as_str())
}

/// Whether one segment names an alternate data stream (`file.txt:hidden`).
///
/// A bare drive prefix (`C:`) is classified by [`classify`] instead, so any
/// remaining colon inside a segment is a stream specifier.
pub fn has_alternate_data_stream(segment: &str) -> bool {
    segment.contains(':')
}

/// Whether a segment ends in characters Windows silently strips.
///
/// `report.` and `report` open the same file, so accepting both would let one
/// document have two identities.
pub fn has_stripped_tail(segment: &str) -> bool {
    matches!(segment.chars().last(), Some('.') | Some(' '))
}

/// Characters Windows refuses in a filename.
pub fn has_illegal_character(segment: &str) -> bool {
    segment
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 0x20)
}

/// ASCII case folding, matching how this crate compares path text.
///
/// Windows folds using the OS uppercase table, which also folds many
/// non-ASCII characters. This function deliberately folds ASCII only: over-
/// folding would make two genuinely distinct names compare equal, and the
/// comparison is used to *refuse*, so under-folding fails closed.
pub fn case_fold(text: &str) -> String {
    text.to_ascii_uppercase()
}

/// Case-insensitive segment comparison under the same conservative rule.
pub fn segments_equal_folded(left: &str, right: &str) -> bool {
    case_fold(left) == case_fold(right)
}
