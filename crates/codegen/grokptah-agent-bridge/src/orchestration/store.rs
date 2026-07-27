//! Durable run records, idempotency receipts, audit log (#196).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;

use super::types::{AuditEntry, IdempotencyReceipt, RunRecord, RunState};

#[derive(Clone)]
pub struct OrchStore {
    inner: Arc<OrchStoreInner>,
}

struct OrchStoreInner {
    root: PathBuf,
    lock: Mutex<()>,
}

impl OrchStore {
    /// Open store and convert unfinished runs to `interrupted` (crash recovery).
    /// Call once per process boot — not when cloning a handle for background work.
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("runs"))?;
        fs::create_dir_all(root.join("idempotency"))?;
        fs::create_dir_all(root.join("audit"))?;
        let store = Self {
            inner: Arc::new(OrchStoreInner {
                root,
                lock: Mutex::new(()),
            }),
        };
        store.mark_unfinished_interrupted()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn run_path(&self, run_id: &str) -> PathBuf {
        self.inner.root.join("runs").join(format!("{run_id}.json"))
    }

    fn idemp_path(&self, request_id: &str) -> PathBuf {
        // request ids may contain path-unfriendly chars
        let safe: String = request_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.inner
            .root
            .join("idempotency")
            .join(format!("{safe}.json"))
    }

    pub fn save_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        let path = self.run_path(&run.run_id);
        atomic_write_json(&path, run)
    }

    pub fn load_run(&self, run_id: &str) -> anyhow::Result<Option<RunRecord>> {
        let path = self.run_path(run_id);
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    pub fn list_runs(&self) -> anyhow::Result<Vec<RunRecord>> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("runs");
        if !dir.is_dir() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(e.path()) {
                if let Ok(r) = serde_json::from_str::<RunRecord>(&text) {
                    out.push(r);
                }
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    pub fn save_idempotency(&self, receipt: &IdempotencyReceipt) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        atomic_write_json(&self.idemp_path(&receipt.request_id), receipt)
    }

    pub fn load_idempotency(&self, request_id: &str) -> anyhow::Result<Option<IdempotencyReceipt>> {
        let path = self.idemp_path(request_id);
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    pub fn append_audit(&self, entry: &AuditEntry) -> anyhow::Result<()> {
        use std::io::Write;
        let _g = self.inner.lock.lock();
        let path = self.inner.root.join("audit").join("audit.jsonl");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{}", serde_json::to_string(entry)?)?;
        Ok(())
    }

    /// Unfinished runs become `interrupted` and never auto-resume.
    pub fn mark_unfinished_interrupted(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        for mut run in self.list_runs()? {
            if matches!(run.state, RunState::Queued | RunState::Running) {
                run.state = RunState::Interrupted;
                run.updated_at = Utc::now();
                run.terminal_result = Some("interrupted".into());
                run.error_code = Some("interrupted".into());
                self.save_run(&run)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::RunBounds;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn restart_marks_running_interrupted() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let run = RunRecord {
            run_id: "r1".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: "req1".into(),
            client_id: None,
            state: RunState::Running,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
        };
        store.save_run(&run).unwrap();
        // reopen = crash recovery
        let store2 = OrchStore::open(d.path()).unwrap();
        let loaded = store2.load_run("r1").unwrap().unwrap();
        assert_eq!(loaded.state, RunState::Interrupted);
        assert!(!matches!(loaded.state, RunState::Running));
    }

    #[test]
    fn clone_does_not_interrupt_running() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let run = RunRecord {
            run_id: "r2".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: "req2".into(),
            client_id: None,
            state: RunState::Running,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
        };
        store.save_run(&run).unwrap();
        let clone = store.clone();
        let loaded = clone.load_run("r2").unwrap().unwrap();
        assert_eq!(loaded.state, RunState::Running);
    }

    #[test]
    fn idempotency_roundtrip() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let r = IdempotencyReceipt {
            request_id: "abc/def".into(),
            payload_hash: "h".into(),
            run_id: Some("r".into()),
            tool: "ptah_submit_task".into(),
            response: serde_json::json!({"ok": true}),
            created_at: Utc::now(),
        };
        store.save_idempotency(&r).unwrap();
        let loaded = store.load_idempotency("abc/def").unwrap().unwrap();
        assert_eq!(loaded.payload_hash, "h");
    }
}
