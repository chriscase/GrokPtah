//! External read-only SDK for current GrokPtah MCP observatory tools.
//!
//! The consumer supplies authentication, TLS, MCP initialize/session headers,
//! and JSON-RPC framing through [`McpTransport`]. This crate never opens a
//! socket, never calls mutation tools, and never depends on
//! `grokptah-agent-bridge`.
//!
//! Wired host tools: `tools/list`, `ptah_list_sessions`, `ptah_list_runs`,
//! `ptah_get_run`, `ptah_get_events`, `ptah_get_capacity`. A missing tool is
//! [`SdkError::Unsupported`], never empty data.

mod capability;
mod dto;
mod error;
mod ids;
mod observe;
mod page;
mod service;
mod transport;
mod version;

pub use capability::{Capabilities, CapabilityState};
pub use dto::{
    EventPage, EventRange, HostCapacity, HostHealth, PublicEvent, PublicEventKind, RunBoundsView,
    RunView, SessionView, UsageView,
};
pub use error::SdkError;
pub use ids::{RunId, SessionId, WorkspaceRef};
pub use observe::{EventQuery, RunSelector, SessionScope};
pub use page::{Cursor, RetainedRange};
pub use service::ReadObservatory;
pub use transport::{McpTool, McpTransport, TransportError};
pub use version::{
    CONTRACT_VERSION, EVENT_PAGE_LIMIT_DEFAULT, EVENT_PAGE_LIMIT_MAX, EVENT_PAGE_LIMIT_MIN,
    contract_version,
};
