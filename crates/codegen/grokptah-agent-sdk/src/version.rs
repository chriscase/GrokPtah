//! Contract version for this read-only observatory seam.

/// Published SDK contract version. Bump only with a documented projection change.
pub const CONTRACT_VERSION: &str = "1.0";

/// Host `ptah_get_events` minimum `limit` (`mcp_control` schema / dispatch).
pub const EVENT_PAGE_LIMIT_MIN: u32 = 1;

/// Host `ptah_get_events` maximum `limit` (`mcp_control` schema / dispatch).
pub const EVENT_PAGE_LIMIT_MAX: u32 = 500;

/// Host default `ptah_get_events` `limit` when the argument is omitted.
pub const EVENT_PAGE_LIMIT_DEFAULT: u32 = 50;

/// Current contract version string (`"1.0"`).
pub fn contract_version() -> &'static str {
    CONTRACT_VERSION
}
