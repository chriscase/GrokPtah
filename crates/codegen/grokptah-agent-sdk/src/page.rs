//! Consumer-safe pagination.
//!
//! The runtime pages its event journal by a monotonic durable sequence and
//! answers a cursor below the retained window with `cursor_expired` plus the
//! range that is still readable. This module carries that behavior, with one
//! addition: the cursor is **opaque**. A consumer stores and returns it; it
//! does not do arithmetic on it, so a host may change its internal cursor
//! encoding without a contract break.

use serde::{Deserialize, Serialize};

use crate::error::{SdkError, SdkErrorCode, SdkResult};

/// Largest page any host may return, whatever it advertises.
pub const MAX_PAGE_LIMIT: u32 = 500;
/// Page size used when a caller does not ask for one.
pub const DEFAULT_PAGE_LIMIT: u32 = 100;

/// Opaque resume token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    /// Adapters mint cursors; consumers only echo them back.
    pub fn from_opaque(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One page request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRequest {
    /// Resume after this cursor. `None` reads from the retained start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Cursor>,
    /// Requested page size. `None` uses [`DEFAULT_PAGE_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl PageRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn after(mut self, cursor: Cursor) -> Self {
        self.after = Some(cursor);
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Validate and resolve the effective page size.
    ///
    /// An out-of-range limit is a typed rejection rather than a silent clamp:
    /// a caller that asked for 5,000 events and received 500 without being
    /// told would build a paging loop on a false premise.
    pub fn resolve_limit(&self, host_max: u32) -> SdkResult<u32> {
        let ceiling = host_max.clamp(1, MAX_PAGE_LIMIT);
        match self.limit {
            None => Ok(DEFAULT_PAGE_LIMIT.min(ceiling)),
            Some(0) => Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "page limit must be at least 1",
            )),
            Some(requested) if requested > ceiling => Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                format!("page limit {requested} exceeds the host maximum {ceiling}"),
            )
            .with_detail("maxPageLimit", ceiling.to_string())),
            Some(requested) => Ok(requested),
        }
    }
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Present only while more items remain. Absence means "caught up", which
    /// is a different fact from an empty page.
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<Cursor>) -> Self {
        Self { items, next_cursor }
    }

    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
        }
    }

    /// `true` when the caller has read everything currently retained.
    pub fn is_caught_up(&self) -> bool {
        self.next_cursor.is_none()
    }
}

/// The readable window of a paged resource, reported on cursor expiry so a
/// consumer can resume without a second round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainedRange {
    pub start: Cursor,
    pub end: Cursor,
}

/// Build the typed failure for a cursor below the retained window.
///
/// The retained range travels on the error itself, matching the runtime's
/// `cursor_expired` + `eventRange` behavior.
pub fn cursor_expired(range: RetainedRange) -> SdkError {
    SdkError::new(
        SdkErrorCode::CursorExpired,
        "cursor is below the retained window; resume from the reported range",
    )
    .with_detail("retainedStart", range.start.as_str())
    .with_detail("retainedEnd", range.end.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limit_respects_a_stricter_host() {
        assert_eq!(PageRequest::new().resolve_limit(500).unwrap(), 100);
        assert_eq!(PageRequest::new().resolve_limit(25).unwrap(), 25);
    }

    #[test]
    fn oversized_and_zero_limits_are_rejected_not_clamped() {
        let err = PageRequest::new()
            .limit(5_000)
            .resolve_limit(500)
            .unwrap_err();
        assert_eq!(err.code, SdkErrorCode::InvalidRequest);
        assert_eq!(err.detail("maxPageLimit"), Some("500"));
        assert_eq!(
            PageRequest::new()
                .limit(0)
                .resolve_limit(500)
                .unwrap_err()
                .code,
            SdkErrorCode::InvalidRequest
        );
    }

    #[test]
    fn host_max_can_never_exceed_the_contract_ceiling() {
        assert_eq!(
            PageRequest::new()
                .limit(MAX_PAGE_LIMIT)
                .resolve_limit(u32::MAX)
                .unwrap(),
            MAX_PAGE_LIMIT
        );
        assert!(PageRequest::new()
            .limit(MAX_PAGE_LIMIT + 1)
            .resolve_limit(u32::MAX)
            .is_err());
    }

    #[test]
    fn expiry_carries_the_retained_range() {
        let err = cursor_expired(RetainedRange {
            start: Cursor::from_opaque("41"),
            end: Cursor::from_opaque("99"),
        });
        assert_eq!(err.code, SdkErrorCode::CursorExpired);
        assert_eq!(err.detail("retainedStart"), Some("41"));
        assert_eq!(err.detail("retainedEnd"), Some("99"));
    }

    #[test]
    fn empty_page_is_caught_up_but_a_full_page_with_cursor_is_not() {
        assert!(Page::<u8>::empty().is_caught_up());
        assert!(!Page::new(vec![1u8], Some(Cursor::from_opaque("1"))).is_caught_up());
    }
}
