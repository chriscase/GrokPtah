//! Contract version for this read-only observatory seam.

/// Published SDK contract version. Bump only with a documented projection change.
/// `RunView` remains the legacy `RunRecord` projection type; `list_runs` /
/// `observe_run` no longer fetch it from public MCP.
pub const CONTRACT_VERSION: &str = "1.0";

/// Explicit public-run document version. Unknown values fail closed.
/// Public MCP `ptah_list_runs` / `ptah_get_run` emit this document.
pub const PUBLIC_RUN_SCHEMA_VERSION: &str = "grokptah.public-run.v1";

/// Explicit public-event document version. Unknown values fail closed.
/// Public MCP `ptah_get_events` emits this document.
pub const PUBLIC_EVENT_SCHEMA_VERSION: &str = "grokptah.public-event.v1";

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
