use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::completion::CompletionEvidence;
use crate::orchestration::RunExecutionMode;

/// Whether a session's persisted working directory is safe to use.
///
/// Keeping this in the durable session summary lets the desktop and control
/// plane show a recovery state without guessing from a path string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    #[default]
    Ready,
    Missing,
    Inaccessible,
    NotDirectory,
}

impl WorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Inaccessible => "inaccessible",
            Self::NotDirectory => "not_directory",
        }
    }
}

pub fn workspace_status(path: &std::path::Path) -> WorkspaceStatus {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => WorkspaceStatus::Ready,
        Ok(_) => WorkspaceStatus::NotDirectory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => WorkspaceStatus::Missing,
        Err(_) => WorkspaceStatus::Inaccessible,
    }
}

/// Workspace mode: coding agent builds vs plain Grok conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// Coding-agent / build session (tools, project cwd, effort).
    #[default]
    Build,
    /// Regular Grok chat — conversational, no tool loop by default.
    Chat,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Chat => "chat",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "chat" | "grok" | "conversation" => Self::Chat,
            _ => Self::Build,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: Uuid,
    /// Durable Build-agent identity, when this session has entered the
    /// persistent-agent lifecycle. Chat sessions and legacy shells may omit it.
    #[serde(default)]
    pub agent_id: Option<String>,
    pub title: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub forked_from: Option<Uuid>,
    /// Virtual folder label (e.g. "NexaDeck", "experiments"). None = Inbox.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub archived: bool,
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub kind: SessionKind,
    /// Execution policy for future Build turns in this session.
    #[serde(default)]
    pub execution_mode: RunExecutionMode,
    #[serde(default)]
    pub workspace_status: WorkspaceStatus,
}

/// Durable evidence for one completed or interrupted model turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCompletion {
    pub turn_id: Uuid,
    pub completed_at: DateTime<Utc>,
    pub evidence: CompletionEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// `user` | `assistant` | `system` | `tool` | `thought`
    pub role: String,
    /// Primary display text (user/assistant body; tool title line).
    pub text: String,
    /// Tool call id when `role == "tool"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
}

impl TranscriptEntry {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            text: text.into(),
            tool_call_id: None,
            tool_title: None,
            tool_status: None,
            tool_output: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            text: text.into(),
            tool_call_id: None,
            tool_title: None,
            tool_status: None,
            tool_output: None,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            text: text.into(),
            tool_call_id: None,
            tool_title: None,
            tool_status: None,
            tool_output: None,
        }
    }

    /// Model reasoning / chain-of-thought (hydrated as thought bubbles on reload).
    pub fn thought(text: impl Into<String>) -> Self {
        Self {
            role: "thought".into(),
            text: text.into(),
            tool_call_id: None,
            tool_title: None,
            tool_status: None,
            tool_output: None,
        }
    }

    pub fn tool(
        call_id: impl Into<String>,
        title: impl Into<String>,
        status: impl Into<String>,
        output: Option<String>,
    ) -> Self {
        let title = title.into();
        let status = status.into();
        Self {
            role: "tool".into(),
            text: format!("{title} · {status}"),
            tool_call_id: Some(call_id.into()),
            tool_title: Some(title),
            tool_status: Some(status),
            tool_output: output,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub title: String,
    pub cwd: std::path::PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub transcript: Vec<TranscriptEntry>,
    pub forked_from: Option<Uuid>,
    pub model: String,
    pub effort: crate::types::EffortLevel,
    #[serde(default)]
    pub plan_mode: bool,
    #[serde(default)]
    pub plan_steps: Vec<String>,
    /// proposed | accepted | executing | done | rejected | ""
    #[serde(default)]
    pub plan_status: String,
    /// Original user goal that the plan addresses.
    #[serde(default)]
    pub plan_goal: Option<String>,
    /// Server-facing summary of transcript *before* [`api_context_start`].
    /// Local `transcript` is never truncated by compact — this only shrinks
    /// what is re-sent to the model on the next turn.
    #[serde(default)]
    pub compacted_summary: Option<String>,
    /// Index into `transcript` where the API context window begins.
    /// Entries `[0..api_context_start)` stay on disk forever for search/UI
    /// but are omitted from wire history (replaced by `compacted_summary`).
    #[serde(default)]
    pub api_context_start: usize,
    /// Virtual folder (UI org only — not a filesystem path).
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub kind: SessionKind,
    /// Execution policy for future Build turns in this session.
    #[serde(default)]
    pub execution_mode: RunExecutionMode,
    /// Whether [`Session::execution_mode`] was chosen by the operator.
    ///
    /// A mode this host defaulted follows the workspace: rebinding a session
    /// to a repository that cannot back a worktree must not leave it pinned to
    /// isolation it can never prepare. A mode the operator chose is theirs and
    /// is never recomputed. Sessions written before this field existed decode
    /// as "defaulted", which is what they were.
    #[serde(default)]
    pub execution_mode_explicit: bool,
    /// Bounded per-turn completion evidence, restored independently of the transcript.
    #[serde(default)]
    pub completion_history: Vec<SessionCompletion>,
    /// True once `transcript.jsonl` has been read into `transcript`.
    #[serde(skip)]
    pub transcript_loaded: bool,
    /// How many prefix entries are already durable on disk (append cursor).
    #[serde(skip)]
    pub persisted_len: usize,
    /// In-session agent todo list (not durable across restarts by design).
    #[serde(skip)]
    pub todos: crate::todo_list::TodoList,
}

impl Session {
    pub fn new(cwd: std::path::PathBuf, model: String, effort: crate::types::EffortLevel) -> Self {
        Self::new_with_kind(cwd, model, effort, SessionKind::Build)
    }

    /// Create a session, defaulting a Build session on a clean Git workspace
    /// to an isolated worktree.
    ///
    /// Isolation is the safe default because it is the only mode whose changes
    /// can be reviewed before they touch the operator's checkout. It is chosen
    /// only when [`crate::run_promotion::isolation_readiness`] says the
    /// workspace can actually back one: on a dirty or non-Git workspace an
    /// isolated run would fail on its first turn, so those fall back to shared
    /// execution rather than producing a session that cannot run.
    pub fn new_with_kind(
        cwd: std::path::PathBuf,
        model: String,
        effort: crate::types::EffortLevel,
        kind: SessionKind,
    ) -> Self {
        let execution_mode = if kind == SessionKind::Build
            && crate::run_promotion::isolation_readiness(&cwd).permits_default_isolation()
        {
            RunExecutionMode::IsolatedWorktree
        } else {
            RunExecutionMode::Shared
        };
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            agent_id: None,
            title: match kind {
                SessionKind::Chat => "New chat".into(),
                SessionKind::Build => "New session".into(),
            },
            cwd,
            created_at: now,
            updated_at: now,
            transcript: Vec::new(),
            forked_from: None,
            model,
            effort,
            plan_mode: false,
            plan_steps: Vec::new(),
            plan_status: String::new(),
            plan_goal: None,
            compacted_summary: None,
            api_context_start: 0,
            folder: None,
            tags: Vec::new(),
            archived: false,
            archived_at: None,
            kind,
            execution_mode,
            execution_mode_explicit: false,
            completion_history: Vec::new(),
            transcript_loaded: true,
            persisted_len: 0,
            todos: crate::todo_list::TodoList::default(),
        }
    }

    pub fn summary(&self) -> SessionSummary {
        // Prefer in-memory length when loaded; else disk cursor from meta load.
        let message_count = if self.transcript_loaded {
            self.transcript.len()
        } else {
            self.persisted_len
        };
        SessionSummary {
            id: self.id,
            agent_id: self.agent_id.clone(),
            title: self.title.clone(),
            cwd: self.cwd.display().to_string(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count,
            forked_from: self.forked_from,
            folder: self.folder.clone(),
            tags: self.tags.clone(),
            archived: self.archived,
            archived_at: self.archived_at,
            kind: self.kind,
            execution_mode: self.execution_mode,
            workspace_status: workspace_status(&self.cwd),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EffortLevel;
    use std::fs;
    use std::process::Command;

    fn git(root: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "GrokPtah tests")
            .env("GIT_AUTHOR_EMAIL", "tests@grokptah.invalid")
            .env("GIT_COMMITTER_NAME", "GrokPtah tests")
            .env("GIT_COMMITTER_EMAIL", "tests@grokptah.invalid")
            .output()
            .expect("start git");
        assert!(output.status.success(), "git {args:?} failed");
    }

    fn clean_repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("README.md"), "base\n").unwrap();
        git(dir.path(), &["add", "README.md"]);
        git(dir.path(), &["commit", "-qm", "base"]);
        dir
    }

    fn session(cwd: &std::path::Path, kind: SessionKind) -> Session {
        Session::new_with_kind(cwd.to_path_buf(), "grok-4".into(), EffortLevel::None, kind)
    }

    /// Isolation is the safe default because it is the only mode whose changes
    /// can be reviewed before they touch the operator's checkout.
    #[test]
    fn a_new_build_session_on_a_clean_git_workspace_defaults_to_isolation() {
        let repository = clean_repository();
        let session = session(repository.path(), SessionKind::Build);
        assert_eq!(session.execution_mode, RunExecutionMode::IsolatedWorktree);
        assert_eq!(
            session.summary().execution_mode,
            RunExecutionMode::IsolatedWorktree
        );
        assert!(session.execution_mode.rollback_guarantee().is_durable());
    }

    /// Where isolation cannot actually be prepared, defaulting to it would
    /// produce a session whose first turn always fails. Those fall back to
    /// shared execution, which is honest about promising no rollback.
    #[test]
    fn a_build_session_falls_back_to_shared_where_isolation_cannot_be_prepared() {
        let dirty = clean_repository();
        fs::write(dirty.path().join("scratch.txt"), "uncommitted\n").unwrap();
        assert_eq!(
            session(dirty.path(), SessionKind::Build).execution_mode,
            RunExecutionMode::Shared
        );

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            session(plain.path(), SessionKind::Build).execution_mode,
            RunExecutionMode::Shared
        );

        let missing = plain.path().join("does-not-exist");
        assert_eq!(
            session(&missing, SessionKind::Build).execution_mode,
            RunExecutionMode::Shared
        );
    }

    /// Isolated execution is a Build-session concept; a chat has no workspace
    /// changes to isolate, so it must not silently acquire a worktree.
    #[test]
    fn a_chat_session_never_defaults_to_isolation_even_on_a_clean_repository() {
        let repository = clean_repository();
        assert_eq!(
            session(repository.path(), SessionKind::Chat).execution_mode,
            RunExecutionMode::Shared
        );
    }

    /// The guarantee is stated, not inferred, and shared execution states that
    /// it has none. Review and promotion already refuse shared runs, so any
    /// rollback claim here would be one the runtime cannot honour.
    #[test]
    fn shared_execution_claims_no_durable_rollback() {
        use crate::orchestration::RollbackGuarantee;
        assert_eq!(
            RunExecutionMode::Shared.rollback_guarantee(),
            RollbackGuarantee::None
        );
        assert!(!RunExecutionMode::Shared.rollback_guarantee().is_durable());
        assert!(RunExecutionMode::Shared.requires_unsafe_acknowledgement());

        assert_eq!(
            RunExecutionMode::IsolatedWorktree.rollback_guarantee(),
            RollbackGuarantee::ReviewedWorktree
        );
        assert!(RunExecutionMode::IsolatedWorktree
            .rollback_guarantee()
            .is_durable());
        assert!(
            !RunExecutionMode::IsolatedWorktree.requires_unsafe_acknowledgement(),
            "the safe mode must never demand an unsafe acknowledgement"
        );
    }

    /// A persisted session keeps whatever mode it was created with, so an
    /// existing shared session is not silently migrated under the operator.
    #[test]
    fn a_persisted_execution_mode_survives_a_round_trip() {
        let repository = clean_repository();
        let session = session(repository.path(), SessionKind::Build);
        let encoded = serde_json::to_value(&session).expect("session serializes");
        assert_eq!(encoded["execution_mode"], "isolated_worktree");
        let decoded: Session = serde_json::from_value(encoded).expect("session round-trips");
        assert_eq!(decoded.execution_mode, RunExecutionMode::IsolatedWorktree);
    }
}
