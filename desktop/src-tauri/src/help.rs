//! Tauri commands for authorized Help.
//!
//! An IPC adapter and nothing more. Both command bodies are single delegations
//! to `grokptah-help-authority`, whose TypeScript mirror is proven equivalent
//! by a shared fixture set. The desktop and the browser broker therefore reach
//! identical decisions by construction rather than by two implementations
//! agreeing on purpose.
//!
//! The delegation itself — including the served-index comparison and the
//! closed `ServedIndex` contract — is tested in the authority crate, so this
//! file holds no logic that could drift untested.
//!
//! No filesystem, provider, or workspace access is reachable from here.

use grokptah_help_authority::{authorize_for_served, DecisionRequest, DecisionResponse, ServedIndex};

/// Authorize one Help action against the corpus and index this build serves.
///
/// Returns the decision rather than the data: the caller applies it. Keeping
/// retrieval out of the command means the authority boundary is testable on
/// its own and cannot be bypassed by a second path that also reads the corpus.
#[tauri::command]
pub fn help_authorize(request: DecisionRequest, served: ServedIndex) -> DecisionResponse {
    authorize_for_served(&request, &served)
}

/// The JSON Schema for the Help authority contracts.
///
/// Exposed so a consumer validates against exactly the document this build
/// enforces, rather than a copy that may have drifted.
#[tauri::command]
pub fn help_authority_schema() -> serde_json::Value {
    grokptah_help_authority::schema::json_schema()
}
