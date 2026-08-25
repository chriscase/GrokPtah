//! Lexical containment.
//!
//! This pass never touches the filesystem. It turns a request string into an
//! ordered list of validated segments, or refuses it. `..` is refused outright
//! rather than collapsed: collapsing would let `a/../../b` look contained
//! while naming something outside, and a link swapped in mid-walk would make
//! any collapsed answer wrong anyway.
//!
//! Windows naming rules are enforced by policy rather than by `cfg`, so the
//! whole table is exercised on every platform's CI rather than only on the
//! platform that would suffer from it.

use std::path::{Component, Path, PathBuf};

use crate::error::SourceViewError;
use crate::winpath::{self, WindowsPathForm};

/// Which naming rules to enforce for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathPolicy {
    /// Refuse reserved device names, alternate data streams, trailing dots or
    /// spaces, and characters Windows rejects.
    pub enforce_windows_names: bool,
    /// Compare an absolute request against the root case-insensitively.
    pub case_insensitive_root: bool,
}

impl PathPolicy {
    /// The policy for the running platform.
    pub fn host() -> Self {
        Self {
            enforce_windows_names: cfg!(windows),
            case_insensitive_root: cfg!(windows),
        }
    }

    /// The strictest policy, used by tests and by any caller that wants a
    /// request to behave identically wherever it is evaluated.
    pub fn strict() -> Self {
        Self {
            enforce_windows_names: true,
            case_insensitive_root: true,
        }
    }
}

/// A request that survived lexical containment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainedPath {
    segments: Vec<String>,
}

impl ContainedPath {
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Forward-slash display form. This is what the wire and the UI show; it
    /// is root-relative, so it never carries the host's directory layout.
    pub fn display(&self) -> String {
        self.segments.join("/")
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.segments.iter().collect()
    }

    /// The first `count` segments, for naming the exact component a walk
    /// refused without naming anything above the root.
    pub fn prefix_display(&self, count: usize) -> String {
        self.segments[..count.min(self.segments.len())].join("/")
    }
}

/// Normalise `requested` into root-relative segments, or refuse it.
pub fn normalize_request(
    root: &Path,
    requested: &str,
    policy: PathPolicy,
) -> Result<ContainedPath, SourceViewError> {
    if requested.contains('\0') {
        return Err(SourceViewError::NulByte);
    }
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err(SourceViewError::EmptyPath);
    }

    let relative_text = match winpath::classify(trimmed) {
        // Forms that reach something other than the file they appear to name.
        WindowsPathForm::VerbatimDevice
        | WindowsPathForm::LocalDevice
        | WindowsPathForm::Unc
        | WindowsPathForm::DriveRelative => return Err(SourceViewError::UnsupportedPathForm),
        WindowsPathForm::DriveAbsolute => strip_root_prefix(root, Path::new(trimmed), policy)
            .ok_or(SourceViewError::AbsolutePathOutsideRoot)?,
        WindowsPathForm::RootRelative => {
            // On Unix this is an ordinary absolute path; a backslash-rooted
            // spelling is not, and is refused.
            if cfg!(unix) && trimmed.starts_with('/') {
                strip_root_prefix(root, Path::new(trimmed), policy)
                    .ok_or(SourceViewError::AbsolutePathOutsideRoot)?
            } else {
                return Err(SourceViewError::UnsupportedPathForm);
            }
        }
        WindowsPathForm::Relative => trimmed.to_string(),
    };

    let mut segments = Vec::new();
    for raw in split_segments(&relative_text) {
        match raw {
            "" | "." => continue,
            ".." => return Err(SourceViewError::ParentEscape),
            segment => segments.push(validate_segment(segment, policy)?),
        }
    }

    if segments.is_empty() {
        return Err(SourceViewError::EmptyPath);
    }
    Ok(ContainedPath { segments })
}

fn validate_segment(segment: &str, policy: PathPolicy) -> Result<String, SourceViewError> {
    // A segment must be exactly one ordinary name to the platform's parser.
    let parsed = Path::new(segment);
    let mut components = parsed.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => {}
        _ => {
            return Err(SourceViewError::InvalidComponent {
                segment: segment.to_string(),
            });
        }
    }

    if policy.enforce_windows_names {
        if winpath::is_reserved_device_name(segment) {
            return Err(SourceViewError::ReservedDeviceName {
                segment: segment.to_string(),
            });
        }
        if winpath::has_alternate_data_stream(segment) {
            return Err(SourceViewError::AlternateDataStream {
                segment: segment.to_string(),
            });
        }
        if winpath::has_stripped_tail(segment) || winpath::has_illegal_character(segment) {
            return Err(SourceViewError::InvalidComponent {
                segment: segment.to_string(),
            });
        }
    }

    Ok(segment.to_string())
}

#[cfg(windows)]
fn split_segments(input: &str) -> impl Iterator<Item = &str> {
    input.split(['/', '\\'])
}

#[cfg(not(windows))]
fn split_segments(input: &str) -> impl Iterator<Item = &str> {
    // A backslash is a legal filename byte on Unix, so it is never a separator.
    input.split('/')
}

/// Strip an approved root prefix from an absolute request.
///
/// Comparison is component-wise rather than textual, so a sibling directory
/// that merely shares a string prefix (`/work-secrets` against `/work`) is not
/// mistaken for containment.
fn strip_root_prefix(root: &Path, candidate: &Path, policy: PathPolicy) -> Option<String> {
    let mut root_parts = root.components();
    let mut candidate_parts = candidate.components();
    loop {
        match (root_parts.next(), candidate_parts.clone().next()) {
            (None, _) => break,
            (Some(_), None) => return None,
            (Some(expected), Some(actual)) => {
                let expected_text = expected.as_os_str().to_string_lossy();
                let actual_text = actual.as_os_str().to_string_lossy();
                let matches = if policy.case_insensitive_root {
                    winpath::segments_equal_folded(&expected_text, &actual_text)
                } else {
                    expected_text == actual_text
                };
                if !matches {
                    return None;
                }
                candidate_parts.next();
            }
        }
    }
    let rest: Vec<String> = candidate_parts
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if rest.is_empty() {
        return None;
    }
    Some(rest.join("/"))
}
