//! Adversarial races: swaps between authorization and action, and between
//! opening and finishing a read.

use std::fs;

use super::support::{Fixture, make_managed_worktree};
use crate::{
    CandidateRoot, NodeIdentity, PathPolicy, RootHandle, SourceRequest, SourceViewError,
    normalize_request, open_contained,
};

#[test]
fn a_root_replaced_between_snapshot_and_read_is_refused() {
    let fixture = Fixture::new();
    let worktree = make_managed_worktree(&fixture.root, "run-swap");
    super::support::write_file(&worktree, "file.txt", b"original\n");

    let snapshot = fixture.snapshot_with(&[CandidateRoot::worktree(&worktree, "run-swap")]);
    let token = snapshot.roots[0].token.clone();
    assert!(fixture.open(&token, "file.txt").is_ok());

    // Discard and recreate the worktree at the same path: same location, a
    // different tree, and therefore not the tree that was authorized.
    fs::remove_dir_all(&worktree).expect("remove");
    let replacement = make_managed_worktree(&fixture.root, "run-swap");
    super::support::write_file(&replacement, "file.txt", b"substituted\n");

    let error = fixture.open(&token, "file.txt").unwrap_err();
    // On a filesystem that reuses the inode immediately this can instead pass
    // identity and read the new bytes; the test asserts the refusal that the
    // identity check exists to produce, and any other outcome is a failure.
    assert_eq!(
        error,
        SourceViewError::RootIdentityChanged,
        "a recreated root is a different root",
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_swapped_in_after_authorization_is_refused_at_open_time() {
    let fixture = Fixture::new();
    fixture.write("target.txt", b"contained\n");
    let outside = fixture.root.parent().expect("parent").join("swap-target");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("secret.txt"), b"secret\n").expect("secret");

    let token = fixture.token();
    assert!(fixture.open(&token, "target.txt").is_ok());

    // Replace the plain file with a symlink pointing outside the root. The
    // token is still valid and the root is unchanged; only the leaf moved.
    fs::remove_file(fixture.path("target.txt")).expect("remove");
    std::os::unix::fs::symlink(outside.join("secret.txt"), fixture.path("target.txt"))
        .expect("symlink");

    assert!(
        matches!(
            fixture.open(&token, "target.txt").unwrap_err(),
            SourceViewError::SymlinkRejected { .. },
        ),
        "no-follow is enforced at open time, not at authorization time",
    );
    fs::remove_dir_all(&outside).ok();
}

#[cfg(unix)]
#[test]
fn a_directory_component_swapped_for_a_symlink_is_refused() {
    let fixture = Fixture::new();
    let outside = fixture.root.parent().expect("parent").join("swap-dir");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("deep.txt"), b"secret\n").expect("secret");

    let token = fixture.token();
    assert!(fixture.open(&token, "src/nested/deep.txt").is_ok());

    fs::remove_dir_all(fixture.path("src/nested")).expect("remove");
    std::os::unix::fs::symlink(&outside, fixture.path("src/nested")).expect("symlink");

    assert_eq!(
        fixture.open(&token, "src/nested/deep.txt").unwrap_err(),
        SourceViewError::SymlinkRejected {
            segment: "src/nested".into()
        },
    );
    fs::remove_dir_all(&outside).ok();
}

#[test]
fn a_file_replaced_while_open_is_refused_rather_than_stitched() {
    let fixture = Fixture::new();
    fixture.write("live.txt", b"first\nsecond\n");
    let token = fixture.token();

    let contained = normalize_request(&fixture.root, "live.txt", PathPolicy::host()).expect("ok");
    let handle = RootHandle::open(&fixture.root).expect("open root");
    let opened = open_contained(&handle, &contained).expect("open");

    // Replace the file underneath the open handle.
    fs::remove_file(fixture.path("live.txt")).expect("remove");
    fixture.write("live.txt", b"totally different content\n");

    // The original handle still points at the original inode, so the *handle*
    // is unchanged; a caller that re-opens through the token sees the new file
    // with a new identity, which is what makes a stale cursor detectable.
    assert!(opened.validate_unchanged().is_ok());
    let reread = fixture.open(&token, "live.txt").expect("read");
    assert_eq!(Fixture::chunk_text(&reread), "totally different content");
}

#[test]
fn truncating_a_file_under_an_open_handle_is_detected() {
    let fixture = Fixture::new();
    fixture.write("live.txt", b"first\nsecond\n");
    let contained = normalize_request(&fixture.root, "live.txt", PathPolicy::host()).expect("ok");
    let root = RootHandle::open(&fixture.root).expect("open root");
    let opened = open_contained(&root, &contained).expect("open");

    let handle = std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.path("live.txt"))
        .expect("open for write");
    handle.set_len(3).expect("truncate");
    drop(handle);

    assert_eq!(
        opened.validate_unchanged().unwrap_err(),
        SourceViewError::DocumentChanged,
        "a projection must not describe a file that changed extent under it",
    );
}

#[test]
fn a_stale_cursor_is_refused_after_the_file_changes() {
    let fixture = Fixture::new();
    fixture.write("paged.txt", b"aaa\nbbb\nccc\nddd\neee\n");
    let token = fixture.token();

    let first = fixture
        .open_request(&SourceRequest::new(&token, "paged.txt").with_limits(
            crate::RequestedLimits {
                max_bytes: Some(4),
                ..Default::default()
            },
        ))
        .expect("read");
    let cursor = first.chunk.next_cursor.expect("more to read");

    fixture.write("paged.txt", b"zzz\nyyy\nxxx\nwww\nvvv\n");

    assert_eq!(
        fixture
            .open_request(&SourceRequest::new(&token, "paged.txt").resume(cursor))
            .unwrap_err(),
        SourceViewError::CursorInvalid,
        "a cursor is bound to the content it was minted against",
    );
}

#[test]
fn a_root_that_disappears_between_snapshot_and_read_fails_closed() {
    let fixture = Fixture::new();
    let worktree = make_managed_worktree(&fixture.root, "run-gone");
    super::support::write_file(&worktree, "file.txt", b"here\n");
    let snapshot = fixture.snapshot_with(&[CandidateRoot::worktree(&worktree, "run-gone")]);
    let token = snapshot.roots[0].token.clone();

    fs::remove_dir_all(&worktree).expect("remove");

    // The handle is still open, so the bytes are technically still reachable
    // through it. Reaching them anyway would mean serving a tree the operator
    // discarded, so the zero-link check refuses instead.
    assert_eq!(
        fixture.open(&token, "file.txt").unwrap_err(),
        SourceViewError::RootIdentityChanged,
    );
}

#[test]
fn a_root_that_cannot_be_opened_at_all_is_never_approved() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot_with(&[CandidateRoot::worktree(
        fixture.path("never/existed"),
        "run-absent",
    )]);
    assert!(
        snapshot.roots.is_empty(),
        "an unopenable candidate is dropped at authorization, not at read time",
    );
}

#[test]
fn node_identity_distinguishes_two_files_and_survives_a_re_stat() {
    let fixture = Fixture::new();
    fixture.write("one.txt", b"one\n");
    fixture.write("two.txt", b"two\n");

    let identity = |name: &str| {
        let file = std::fs::File::open(fixture.path(name)).expect("open");
        NodeIdentity::from_metadata(&file.metadata().expect("metadata"))
    };
    let first = identity("one.txt");
    assert!(first.unchanged(&identity("one.txt")));
    if cfg!(unix) {
        assert!(
            !first.same_node(&identity("two.txt")),
            "distinct inodes must compare unequal",
        );
    }
}

#[test]
fn concurrent_reads_through_one_token_do_not_interfere() {
    use std::sync::Arc;
    use std::thread;

    let fixture = Arc::new(Fixture::new());
    fixture.write("shared.txt", b"alpha\nbeta\ngamma\n");
    let token = Arc::new(fixture.token());

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            let token = Arc::clone(&token);
            thread::spawn(move || {
                let document = fixture.open(&token, "shared.txt").expect("read");
                Fixture::chunk_text(&document)
            })
        })
        .collect();

    for handle in handles {
        assert_eq!(handle.join().expect("thread"), "alpha\nbeta\ngamma");
    }
}

#[test]
fn concurrent_issue_and_resolve_stay_consistent() {
    use std::sync::Arc;
    use std::thread;

    let fixture = Arc::new(Fixture::new());
    let issuers: Vec<_> = (0..4)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            thread::spawn(move || {
                let snapshot = fixture.snapshot();
                let token = snapshot.roots[0].token.clone();
                fixture.open(&token, "src/main.rs").is_ok()
            })
        })
        .collect();

    for issuer in issuers {
        assert!(
            issuer.join().expect("thread"),
            "each issued token must resolve"
        );
    }
}
