//! Opaque identifiers. Workspace tokens are never displayed as filesystem paths.

use std::fmt;

use serde::Serialize;

/// Build-session identity as returned by `ptah_list_sessions`.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize)]
pub struct SessionId(String);

impl SessionId {
    /// Wrap a host session id. Callers should treat this as an opaque token.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Durable run identity as returned by `ptah_list_runs` / `ptah_get_run`.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize)]
pub struct RunId(String);

impl RunId {
    /// Wrap a host run id. Callers should treat this as an opaque token.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque workspace handle recovered from a Build session listing.
///
/// The host MCP tools still require the original workspace token on the wire.
/// This type never exposes that token through `Debug`, `Display`, or `Serialize`.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct WorkspaceRef(String);

impl WorkspaceRef {
    pub(crate) fn from_host(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub(crate) fn host_token(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WorkspaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkspaceRef")
    }
}

impl Serialize for WorkspaceRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_unit_struct("WorkspaceRef")
    }
}
