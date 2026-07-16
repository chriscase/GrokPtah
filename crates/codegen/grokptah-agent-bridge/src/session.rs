use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
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

    pub fn new_with_kind(
        cwd: std::path::PathBuf,
        model: String,
        effort: crate::types::EffortLevel,
        kind: SessionKind,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
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
        }
    }
}
