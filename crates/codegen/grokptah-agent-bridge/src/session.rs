use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub forked_from: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub title: String,
    pub cwd: std::path::PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub transcript: Vec<TranscriptEntry>,
    pub forked_from: Option<Uuid>,
    pub model: String,
    pub effort: crate::types::EffortLevel,
    pub plan_mode: bool,
    pub plan_steps: Vec<String>,
    /// Compact drops early transcript into a summary blob.
    pub compacted_summary: Option<String>,
}

impl Session {
    pub fn new(cwd: std::path::PathBuf, model: String, effort: crate::types::EffortLevel) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: "New session".into(),
            cwd,
            created_at: now,
            updated_at: now,
            transcript: Vec::new(),
            forked_from: None,
            model,
            effort,
            plan_mode: false,
            plan_steps: Vec::new(),
            compacted_summary: None,
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id,
            title: self.title.clone(),
            cwd: self.cwd.display().to_string(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count: self.transcript.len(),
            forked_from: self.forked_from,
        }
    }
}
