//! External read-only SDK for current GrokPtah MCP observatory tools.
//!
//! The consumer supplies authentication, TLS, MCP initialize/session headers,
//! and JSON-RPC framing through [`McpTransport`]. This crate never opens a
//! socket, never calls mutation tools, and never depends on
//! `grokptah-agent-bridge`.
//!
//! Wired host tools: `tools/list`, `ptah_list_sessions`, `ptah_list_runs`,
//! `ptah_get_run`, `ptah_get_progress`, `ptah_get_handoff`, `ptah_get_events`,
//! `ptah_get_capacity`. A missing tool is [`SdkError::Unsupported`], never
//! empty data.
//!
//! Additive `grokptah.public-run.v1` methods (`list_public_runs`,
//! `observe_public_run`, `observe_public_progress`, `observe_public_handoff`)
//! parse only the allowlisted document from `ptah_list_runs` / `ptah_get_run`
//! / `ptah_get_progress` / `ptah_get_handoff`. Additive
//! `grokptah.public-event.v1` `stream_public_events` parses only that
//! document from `ptah_get_events`. Legacy `list_runs` / `observe_run` /
//! `stream_events` are unsupported shims: they do not call those public tools
//! and do not deserialize the DTO as `RunRecord` / `JournalPage`.
//!
//! [`grok_build`] is an advisory, provider-neutral manager contract. It does
//! not launch runs, call providers, or confer authority.

mod capability;
mod dto;
mod error;
mod grok_build;
mod ids;
mod observe;
mod page;
mod service;
mod transport;
mod version;

pub use capability::{Capabilities, CapabilityState};
pub use dto::{
    EventPage, EventRange, HostCapacity, HostHealth, PublicEvent, PublicEventKind,
    PublicEventKindV1, PublicEventPageV1, PublicEventV1, PublicRunHandoffV1, PublicRunListV1,
    PublicRunProgressV1, PublicRunState, PublicRunV1, RunBoundsView, RunView, SessionView,
    UsageView, parse_public_event_page_v1, parse_public_event_v1, parse_public_run_handoff_v1,
    parse_public_run_list_v1, parse_public_run_progress_v1, parse_public_run_v1,
};
pub use error::SdkError;
pub use grok_build::{
    GROK_BUILD_CONTRACT_VERSION, GrokBuildCleanupState, GrokBuildContractError,
    GrokBuildGitIdentity, GrokBuildIsolationReceipt, GrokBuildLaunchRequest, GrokBuildMutationMode,
    GrokBuildNonclaim, GrokBuildPolicyState, GrokBuildResult, GrokBuildRunState, GrokBuildVerdict,
    INDEPENDENT_QUALIFICATION_EVIDENCE,
};
pub use ids::{RunId, SessionId, WorkspaceRef};
pub use observe::{EventQuery, RunSelector, SessionScope};
pub use page::{Cursor, RetainedRange};
pub use service::ReadObservatory;
pub use transport::{McpTool, McpTransport, TransportError};
pub use version::{
    CONTRACT_VERSION, EVENT_PAGE_LIMIT_DEFAULT, EVENT_PAGE_LIMIT_MAX, EVENT_PAGE_LIMIT_MIN,
    PUBLIC_EVENT_SCHEMA_VERSION, PUBLIC_RUN_SCHEMA_VERSION, contract_version,
};
