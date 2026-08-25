//! Shared synthetic fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use crate::{
    AuthorizationContext, CandidateRoot, PathPolicy, PolicyInputs, Principal, RootSnapshot,
    SnapshotStore, SourceDocument, SourceRequest, SourceViewError, TestClock,
};

pub const START_MS: u64 = 1_700_000_000_000;
pub const TEST_KEY: [u8; 32] = [7u8; 32];

/// A synthetic workspace, an authorization context, and a live store.
pub struct Fixture {
    _dir: TempDir,
    pub root: PathBuf,
    pub store: SnapshotStore,
    pub clock: Arc<TestClock>,
    pub context: AuthorizationContext,
}

pub fn principal(session: &str) -> Principal {
    Principal::new("user-1", "tenant-a", "project-x", session)
}

pub fn policy(workspace_marker: &str) -> PolicyInputs {
    let mut inputs = PolicyInputs::new();
    inputs.push("workspace", workspace_marker);
    inputs.push("permission_mode", "review");
    inputs
}

pub fn context(session: &str, workspace_marker: &str) -> AuthorizationContext {
    AuthorizationContext::new(principal(session), policy(workspace_marker))
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(TestClock::new(START_MS)))
    }

    pub fn with_clock(clock: Arc<TestClock>) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let root = dunce::canonicalize(dir.path()).expect("canonical temp dir");
        fs::create_dir_all(root.join("src/nested")).expect("nested dir");
        write_file(&root, "src/main.rs", b"fn main() {}\n");
        write_file(&root, "src/nested/deep.txt", b"alpha\nbeta\ngamma\n");
        let store = SnapshotStore::new(TEST_KEY, clock.clone());
        Self {
            _dir: dir,
            root,
            store,
            clock,
            context: context("session-1", "primary"),
        }
    }

    /// A store with a bounded registry, for eviction tests.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut fixture = Self::new();
        fixture.store = SnapshotStore::new(TEST_KEY, fixture.clock.clone()).with_capacity(capacity);
        fixture
    }

    pub fn with_ttl(ttl_ms: u64) -> Self {
        let mut fixture = Self::new();
        fixture.store = SnapshotStore::new(TEST_KEY, fixture.clock.clone()).with_ttl_ms(ttl_ms);
        fixture
    }

    pub fn write(&self, relative: &str, bytes: &[u8]) {
        write_file(&self.root, relative, bytes);
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Issue a snapshot over the workspace root alone.
    pub fn snapshot(&self) -> RootSnapshot {
        self.store
            .issue(&self.context, &[CandidateRoot::workspace(&self.root)])
    }

    pub fn snapshot_with(&self, candidates: &[CandidateRoot]) -> RootSnapshot {
        self.store.issue(&self.context, candidates)
    }

    /// The token for the first (and usually only) approved root.
    pub fn token(&self) -> String {
        self.snapshot().roots[0].token.clone()
    }

    pub fn open(&self, token: &str, path: &str) -> Result<SourceDocument, SourceViewError> {
        crate::open_document(
            &self.store,
            &self.context,
            &SourceRequest::new(token, path),
            PathPolicy::host(),
        )
    }

    pub fn open_request(&self, request: &SourceRequest) -> Result<SourceDocument, SourceViewError> {
        crate::open_document(&self.store, &self.context, request, PathPolicy::host())
    }

    pub fn open_as(
        &self,
        acting: &AuthorizationContext,
        token: &str,
        path: &str,
    ) -> Result<SourceDocument, SourceViewError> {
        crate::open_document(
            &self.store,
            acting,
            &SourceRequest::new(token, path),
            PathPolicy::host(),
        )
    }

    /// Text of the returned chunk, joined with newlines.
    pub fn chunk_text(document: &SourceDocument) -> String {
        document
            .chunk
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Page a whole file through the cursor contract, reassembling lines.
    pub fn read_all(&self, token: &str, path: &str, max_bytes: u64) -> Vec<crate::SourceLine> {
        let limits = crate::RequestedLimits {
            max_bytes: Some(max_bytes),
            ..Default::default()
        };
        let mut assembler = crate::LineAssembler::new();
        let mut request = SourceRequest::new(token, path).with_limits(limits);
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 2_000, "paging must terminate");
            let document = self.open_request(&request).expect("read");
            assembler.push_chunk(&document.chunk);
            match document.chunk.next_cursor {
                Some(cursor) => {
                    request = SourceRequest::new(token, path)
                        .with_limits(limits)
                        .resume(cursor)
                }
                None => break,
            }
        }
        assembler.finish()
    }
}

pub fn write_file(root: &Path, relative: &str, bytes: &[u8]) {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("parent dir");
    }
    fs::write(target, bytes).expect("write fixture");
}

/// Build a synthetic managed run worktree that satisfies the promotion test.
pub fn make_managed_worktree(source: &Path, run_id: &str) -> PathBuf {
    let worktree = source
        .join(".grokptah")
        .join("worktrees")
        .join("runs")
        .join(run_id);
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}/.git/worktrees/{run_id}\n", source.display()),
    )
    .expect("worktree pointer");
    worktree
}
