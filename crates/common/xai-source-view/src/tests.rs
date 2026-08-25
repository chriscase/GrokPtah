//! Containment and bounded-read tests. Every fixture is synthetic and built
//! inside a fresh temporary directory; no test reads repository content.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::{
    Eol, MAX_BYTES_CEILING, SourceLimits, SourceRoot, SourceRootRegistry, SourceViewError,
    TextEncoding, language_for, normalize_relative, open_in_registry, read_document,
    resolve_in_root,
};

/// A synthetic workspace with a couple of files already in it.
struct Fixture {
    _dir: TempDir,
    root: SourceRoot,
    path: PathBuf,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let path = dunce::canonicalize(dir.path()).expect("canonical temp dir");
    fs::create_dir_all(path.join("src/nested")).expect("nested dir");
    fs::write(path.join("src/main.rs"), "fn main() {}\n").expect("main.rs");
    fs::write(path.join("src/nested/deep.txt"), "alpha\nbeta\ngamma\n").expect("deep.txt");
    let root = SourceRoot::workspace(&path, "synthetic").expect("approve root");
    Fixture {
        _dir: dir,
        root,
        path,
    }
}

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).expect("parent dir");
    }
    fs::write(target, bytes).expect("write fixture");
}

// ---------------------------------------------------------------- normalise

#[test]
fn normalises_plain_relative_path() {
    let f = fixture();
    let out = normalize_relative(&f.path, "src/main.rs").expect("normalise");
    assert_eq!(out, PathBuf::from("src").join("main.rs"));
}

#[test]
fn collapses_redundant_separators_and_dot_segments() {
    let f = fixture();
    let out = normalize_relative(&f.path, "./src//./nested/deep.txt").expect("normalise");
    assert_eq!(
        out,
        PathBuf::from("src").join("nested").join("deep.txt"),
        "`.` and empty segments are inert, not an escape",
    );
}

#[test]
fn rejects_parent_escape_anywhere_in_the_path() {
    let f = fixture();
    for attempt in [
        "../secrets",
        "src/../../secrets",
        "src/nested/../../../etc/passwd",
        "..",
    ] {
        assert_eq!(
            normalize_relative(&f.path, attempt),
            Err(SourceViewError::ParentEscape),
            "`{attempt}` must be refused before any filesystem call",
        );
    }
}

#[test]
fn rejects_empty_and_whitespace_only_paths() {
    let f = fixture();
    assert_eq!(
        normalize_relative(&f.path, ""),
        Err(SourceViewError::EmptyPath)
    );
    assert_eq!(
        normalize_relative(&f.path, "   "),
        Err(SourceViewError::EmptyPath)
    );
    assert_eq!(
        normalize_relative(&f.path, "./."),
        Err(SourceViewError::EmptyPath)
    );
}

#[cfg(not(windows))]
#[test]
fn rejects_a_bare_filesystem_root_as_outside_the_boundary() {
    let f = fixture();
    // `/` and `///` parse as absolute, so they are judged by containment
    // rather than emptiness: the filesystem root is above every approved root.
    for attempt in ["/", "///"] {
        assert_eq!(
            normalize_relative(&f.path, attempt),
            Err(SourceViewError::AbsolutePathOutsideRoot),
            "`{attempt}` names the filesystem root, never a contained file",
        );
    }
}

#[test]
fn rejects_interior_nul() {
    let f = fixture();
    assert_eq!(
        normalize_relative(&f.path, "src/ma\0in.rs"),
        Err(SourceViewError::NulByte),
    );
}

#[test]
fn accepts_absolute_path_inside_the_root() {
    let f = fixture();
    let absolute = f.path.join("src/main.rs");
    let out = normalize_relative(&f.path, &absolute.to_string_lossy()).expect("normalise");
    assert_eq!(out, PathBuf::from("src").join("main.rs"));
}

#[test]
fn rejects_absolute_path_outside_the_root() {
    let f = fixture();
    let outside = if cfg!(windows) {
        "C:\\Windows\\win.ini"
    } else {
        "/etc/passwd"
    };
    assert_eq!(
        normalize_relative(&f.path, outside),
        Err(SourceViewError::AbsolutePathOutsideRoot),
    );
}

#[test]
fn rejects_sibling_directory_that_merely_shares_a_prefix() {
    let dir = TempDir::new().expect("temp dir");
    let base = dunce::canonicalize(dir.path()).expect("canonical");
    fs::create_dir_all(base.join("work")).expect("work");
    fs::create_dir_all(base.join("work-secrets")).expect("sibling");
    fs::write(base.join("work-secrets/leak.txt"), b"nope").expect("leak");
    let root = SourceRoot::workspace(base.join("work"), "work").expect("approve");
    let sibling = base.join("work-secrets/leak.txt");
    assert_eq!(
        normalize_relative(&root.path, &sibling.to_string_lossy()),
        Err(SourceViewError::AbsolutePathOutsideRoot),
        "a shared string prefix is not containment",
    );
}

#[cfg(not(windows))]
#[test]
fn treats_backslash_as_a_filename_byte_on_unix() {
    let f = fixture();
    let out = normalize_relative(&f.path, "src/a\\b.rs").expect("normalise");
    assert_eq!(out, PathBuf::from("src").join("a\\b.rs"));
}

// ----------------------------------------------------------------- symlinks

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_file_even_inside_the_root() {
    let f = fixture();
    write(&f.path, "real.txt", b"contained\n");
    std::os::unix::fs::symlink(f.path.join("real.txt"), f.path.join("link.txt")).expect("symlink");
    let error = resolve_in_root(&f.root, "link.txt").expect_err("must refuse");
    assert!(
        matches!(error, SourceViewError::SymlinkRejected { .. }),
        "expected symlink rejection, got {error:?}",
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_directory_component() {
    let dir = TempDir::new().expect("temp dir");
    let base = dunce::canonicalize(dir.path()).expect("canonical");
    fs::create_dir_all(base.join("approved")).expect("approved");
    fs::create_dir_all(base.join("outside")).expect("outside");
    fs::write(base.join("outside/secret.txt"), b"secret\n").expect("secret");
    std::os::unix::fs::symlink(base.join("outside"), base.join("approved/escape"))
        .expect("dir symlink");
    let root = SourceRoot::workspace(base.join("approved"), "approved").expect("approve");
    let error = resolve_in_root(&root, "escape/secret.txt").expect_err("must refuse");
    assert!(
        matches!(&error, SourceViewError::SymlinkRejected { at } if at == "escape"),
        "the *first* linked component must be named, got {error:?}",
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_dangling_symlink_as_a_link_not_as_missing() {
    let f = fixture();
    std::os::unix::fs::symlink(f.path.join("nowhere.txt"), f.path.join("dangling.txt"))
        .expect("symlink");
    let error = resolve_in_root(&f.root, "dangling.txt").expect_err("must refuse");
    assert!(
        matches!(error, SourceViewError::SymlinkRejected { .. }),
        "a dangling link is still a link, got {error:?}",
    );
}

// ----------------------------------------------------------------- resolving

#[test]
fn resolves_a_contained_file() {
    let f = fixture();
    let resolved = resolve_in_root(&f.root, "src/nested/deep.txt").expect("resolve");
    assert_eq!(resolved.relative_path, "src/nested/deep.txt");
    assert!(resolved.absolute_path.starts_with(&f.path));
    assert_eq!(resolved.root.id, f.root.id);
}

#[test]
fn refuses_a_directory() {
    let f = fixture();
    let error = resolve_in_root(&f.root, "src").expect_err("must refuse");
    assert!(
        matches!(error, SourceViewError::NotAFile { .. }),
        "got {error:?}"
    );
}

#[test]
fn reports_a_missing_file_relative_to_the_root() {
    let f = fixture();
    let error = resolve_in_root(&f.root, "src/absent.rs").expect_err("must refuse");
    assert_eq!(
        error,
        SourceViewError::NotFound {
            at: "src/absent.rs".into()
        },
        "the error names the request, never an absolute host path",
    );
}

// ----------------------------------------------------------------- registry

#[test]
fn empty_registry_refuses_every_request() {
    let registry = SourceRootRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(
        registry.resolve("ws-0000000000000000", "any.txt"),
        Err(SourceViewError::NoApprovedRoot),
    );
}

#[test]
fn unknown_root_is_refused_by_id() {
    let f = fixture();
    let mut registry = SourceRootRegistry::new();
    registry.approve(f.root.clone());
    assert_eq!(
        registry.resolve("ws-deadbeefdeadbeef", "src/main.rs"),
        Err(SourceViewError::UnknownRoot {
            root_id: "ws-deadbeefdeadbeef".into()
        }),
    );
}

#[test]
fn root_identity_is_stable_and_path_specific() {
    let f = fixture();
    let again = SourceRoot::workspace(&f.path, "relabelled").expect("approve");
    assert_eq!(f.root.id, again.id, "same directory keeps one identity");

    let other = TempDir::new().expect("temp dir");
    let other_root = SourceRoot::workspace(other.path(), "other").expect("approve");
    assert_ne!(f.root.id, other_root.id);
}

#[test]
fn worktree_and_workspace_identities_never_collide() {
    let f = fixture();
    let worktree =
        SourceRoot::isolated_worktree(&f.path, "run worktree", "run-1").expect("approve");
    assert_ne!(
        f.root.id, worktree.id,
        "kind is part of identity, so the strip cannot mislabel a boundary",
    );
    assert_eq!(worktree.run_id.as_deref(), Some("run-1"));
}

#[test]
fn approving_the_same_root_twice_does_not_duplicate_it() {
    let f = fixture();
    let mut registry = SourceRootRegistry::new();
    registry.approve(f.root.clone());
    registry.approve(SourceRoot::workspace(&f.path, "renamed").expect("approve"));
    assert_eq!(registry.roots().len(), 1);
    assert_eq!(registry.roots()[0].label, "renamed");
}

#[test]
fn refuses_a_root_that_is_not_a_directory() {
    let f = fixture();
    let error = SourceRoot::workspace(f.path.join("src/main.rs"), "file").expect_err("must refuse");
    assert!(
        matches!(error, SourceViewError::RootUnavailable { .. }),
        "got {error:?}"
    );
}

// -------------------------------------------------------------------- limits

#[test]
fn clamps_caller_supplied_limits_into_the_supported_window() {
    let huge = SourceLimits::clamped(Some(u64::MAX), Some(usize::MAX), Some(usize::MAX));
    assert_eq!(huge.max_bytes, MAX_BYTES_CEILING);
    assert_eq!(huge.max_lines, crate::MAX_LINES_CEILING);
    assert_eq!(huge.max_line_chars, crate::MAX_LINE_CHARS_CEILING);

    let zero = SourceLimits::clamped(Some(0), Some(0), Some(0));
    assert_eq!(
        zero,
        SourceLimits::default(),
        "zero means default, never unbounded"
    );
}

#[test]
fn truncates_on_the_byte_ceiling_and_says_so() {
    let f = fixture();
    write(&f.path, "big.txt", "abcdefghij\n".repeat(50).as_bytes());
    let resolved = resolve_in_root(&f.root, "big.txt").expect("resolve");
    let limits = SourceLimits::clamped(Some(25), None, None);
    let doc = read_document(&resolved, limits).expect("read");
    assert!(doc.truncated_bytes);
    assert_eq!(doc.bytes_read, 25);
    assert_eq!(doc.byte_len, 550, "on-disk size is reported independently");
}

#[test]
fn truncates_on_the_line_ceiling_and_keeps_real_line_numbers() {
    let f = fixture();
    let body: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    write(&f.path, "many.txt", body.as_bytes());
    let resolved = resolve_in_root(&f.root, "many.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::clamped(None, Some(5), None)).expect("read");
    assert!(doc.truncated_lines);
    assert_eq!(doc.lines.len(), 5);
    assert_eq!(doc.line_count, 40, "the real total survives truncation");
    assert_eq!(doc.lines[0].number, 1);
    assert_eq!(doc.lines[4].number, 5);
    assert_eq!(doc.lines[4].text, "line 5");
}

#[test]
fn truncates_a_wide_line_on_a_char_boundary() {
    let f = fixture();
    write(&f.path, "wide.txt", "é".repeat(200).as_bytes());
    let resolved = resolve_in_root(&f.root, "wide.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::clamped(None, None, Some(16))).expect("read");
    assert!(doc.lines[0].truncated);
    assert_eq!(doc.lines[0].text.chars().count(), 16);
    assert!(
        doc.lines[0].text.chars().all(|c| c == 'é'),
        "no split code point"
    );
}

#[test]
fn refuses_a_file_above_the_hard_ceiling_without_reading_it() {
    let f = fixture();
    let path = f.path.join("huge.bin");
    let file = fs::File::create(&path).expect("create");
    file.set_len(MAX_BYTES_CEILING + 1).expect("set_len");
    drop(file);
    let resolved = resolve_in_root(&f.root, "huge.bin").expect("resolve");
    let error = read_document(&resolved, SourceLimits::default()).expect_err("must refuse");
    assert_eq!(
        error,
        SourceViewError::TooLarge {
            byte_len: MAX_BYTES_CEILING + 1,
            max_bytes: MAX_BYTES_CEILING,
        },
    );
}

// ------------------------------------------------------------------ decoding

#[test]
fn reads_utf8_with_line_numbers() {
    let f = fixture();
    let resolved = resolve_in_root(&f.root, "src/nested/deep.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.encoding, TextEncoding::Utf8);
    assert_eq!(doc.line_count, 3);
    assert_eq!(
        doc.lines
            .iter()
            .map(|l| (l.number, l.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "alpha"), (2, "beta"), (3, "gamma")],
    );
    assert_eq!(doc.eol, Eol::Lf);
    assert!(!doc.truncated_bytes && !doc.truncated_lines);
}

#[test]
fn classifies_nul_bearing_bytes_as_binary_and_withholds_the_text() {
    let f = fixture();
    write(&f.path, "image.bin", &[0x89, 0x50, 0x00, 0x4e, 0x47]);
    let resolved = resolve_in_root(&f.root, "image.bin").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.encoding, TextEncoding::Binary);
    assert!(
        doc.lines.is_empty(),
        "binary content is never rendered as text"
    );
    assert_eq!(doc.byte_len, 5);
}

#[test]
fn decodes_invalid_utf8_lossily_and_counts_the_replacements() {
    let f = fixture();
    write(
        &f.path,
        "latin.txt",
        &[b'a', 0xff, b'b', b'\n', 0xfe, b'\n'],
    );
    let resolved = resolve_in_root(&f.root, "latin.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.encoding, TextEncoding::Utf8Lossy);
    assert_eq!(doc.lossy_replacements, 2);
    assert_eq!(doc.lines.len(), 2);
    assert!(doc.lines[0].text.contains('\u{FFFD}'));
}

#[test]
fn strips_a_utf8_bom_from_the_first_line() {
    let f = fixture();
    write(&f.path, "bom.txt", "\u{FEFF}first\nsecond\n".as_bytes());
    let resolved = resolve_in_root(&f.root, "bom.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(
        doc.lines[0].text, "first",
        "the BOM is metadata, not column 1"
    );
}

#[test]
fn reports_line_ending_shape() {
    let f = fixture();
    write(&f.path, "crlf.txt", b"a\r\nb\r\n");
    write(&f.path, "mixed.txt", b"a\r\nb\n");
    write(&f.path, "none.txt", b"single line, no newline");
    let read_eol = |name: &str| {
        let resolved = resolve_in_root(&f.root, name).expect("resolve");
        read_document(&resolved, SourceLimits::default())
            .expect("read")
            .eol
    };
    assert_eq!(read_eol("crlf.txt"), Eol::Crlf);
    assert_eq!(read_eol("mixed.txt"), Eol::Mixed);
    assert_eq!(read_eol("none.txt"), Eol::None);
}

#[test]
fn strips_carriage_returns_from_rendered_lines() {
    let f = fixture();
    write(&f.path, "crlf.txt", b"alpha\r\nbeta\r\n");
    let resolved = resolve_in_root(&f.root, "crlf.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.lines[0].text, "alpha");
    assert_eq!(doc.lines[1].text, "beta");
}

#[test]
fn an_empty_file_yields_no_lines_rather_than_one_blank_line() {
    let f = fixture();
    write(&f.path, "empty.txt", b"");
    let resolved = resolve_in_root(&f.root, "empty.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.line_count, 0);
    assert!(doc.lines.is_empty());
}

#[test]
fn a_file_without_a_trailing_newline_keeps_its_last_line() {
    let f = fixture();
    write(&f.path, "tail.txt", b"one\ntwo");
    let resolved = resolve_in_root(&f.root, "tail.txt").expect("resolve");
    let doc = read_document(&resolved, SourceLimits::default()).expect("read");
    assert_eq!(doc.line_count, 2);
    assert_eq!(doc.lines[1].text, "two");
}

#[test]
fn fingerprint_tracks_content_not_path() {
    let f = fixture();
    write(&f.path, "a.txt", b"same bytes\n");
    write(&f.path, "b.txt", b"same bytes\n");
    write(&f.path, "c.txt", b"other bytes\n");
    let print = |name: &str| {
        let resolved = resolve_in_root(&f.root, name).expect("resolve");
        read_document(&resolved, SourceLimits::default())
            .expect("read")
            .content_fingerprint
    };
    assert_eq!(print("a.txt"), print("b.txt"));
    assert_ne!(print("a.txt"), print("c.txt"));
}

// ------------------------------------------------------------------ identity

#[test]
fn document_carries_the_exact_boundary_it_was_read_from() {
    let f = fixture();
    let mut registry = SourceRootRegistry::new();
    registry.approve(f.root.clone());
    let doc = open_in_registry(
        &registry,
        &f.root.id,
        "src/main.rs",
        SourceLimits::default(),
    )
    .expect("open");
    assert_eq!(doc.root_id, f.root.id);
    assert_eq!(doc.root_path, f.path.display().to_string());
    assert_eq!(doc.root_kind, crate::RootKind::Workspace);
    assert_eq!(doc.relative_path, "src/main.rs");
    assert!(doc.absolute_path.starts_with(&doc.root_path));
    assert_eq!(doc.run_id, None);
}

#[test]
fn worktree_documents_name_their_run() {
    let f = fixture();
    let worktree =
        SourceRoot::isolated_worktree(&f.path, "run 42 worktree", "run-42").expect("approve");
    let mut registry = SourceRootRegistry::new();
    registry.approve(worktree.clone());
    let doc = open_in_registry(
        &registry,
        &worktree.id,
        "src/main.rs",
        SourceLimits::default(),
    )
    .expect("open");
    assert_eq!(doc.root_kind, crate::RootKind::IsolatedWorktree);
    assert_eq!(doc.run_id.as_deref(), Some("run-42"));
    assert_eq!(doc.root_label, "run 42 worktree");
}

#[test]
fn escape_attempts_never_reach_the_reader() {
    let f = fixture();
    let mut registry = SourceRootRegistry::new();
    registry.approve(f.root.clone());
    for attempt in ["../../etc/passwd", "src/../../../etc/passwd"] {
        assert_eq!(
            open_in_registry(&registry, &f.root.id, attempt, SourceLimits::default()),
            Err(SourceViewError::ParentEscape),
        );
    }
}

// ------------------------------------------------------------------ language

#[test]
fn infers_language_from_extension_or_well_known_name() {
    assert_eq!(language_for("src/main.rs"), "rust");
    assert_eq!(language_for("src/App.tsx"), "tsx");
    assert_eq!(language_for("a/b/c.d.ts"), "typescript");
    assert_eq!(language_for("Cargo.toml"), "toml");
    assert_eq!(language_for("Cargo.lock"), "toml");
    assert_eq!(language_for("Dockerfile"), "dockerfile");
    assert_eq!(language_for("Makefile"), "makefile");
    assert_eq!(language_for("notes"), "plain");
    assert_eq!(language_for("archive.tar.gz"), "plain");
}

// ------------------------------------------------------------ root approval

#[test]
fn short_label_keeps_the_last_two_segments() {
    use crate::short_root_label;
    assert_eq!(short_root_label("/a/b/c/project"), "c/project");
    assert_eq!(short_root_label("project"), "project");
    assert_eq!(short_root_label("/project"), "project");
    assert_eq!(short_root_label("/a/b/c/project/"), "c/project");
    assert_eq!(short_root_label(""), "");
}

#[test]
fn refusals_lead_with_a_stable_machine_code() {
    use crate::boundary_message;
    assert!(boundary_message(&SourceViewError::ParentEscape).starts_with("parent_escape: "));
    assert!(
        boundary_message(&SourceViewError::SymlinkRejected { at: "link".into() })
            .starts_with("symlink_rejected: ")
    );
    assert!(boundary_message(&SourceViewError::NoApprovedRoot).starts_with("no_approved_root: "));
    assert!(
        boundary_message(&SourceViewError::TooLarge {
            byte_len: 9,
            max_bytes: 4
        })
        .starts_with("too_large: ")
    );
}

#[test]
fn a_worktree_outside_the_managed_run_directory_is_not_approved() {
    use crate::is_managed_run_worktree;
    let dir = TempDir::new().expect("temp dir");
    let source = dir.path().join("source");
    let stray = dir.path().join("stray");
    fs::create_dir_all(source.join(".grokptah/worktrees/runs")).expect("managed root");
    fs::create_dir_all(&stray).expect("stray");
    fs::write(stray.join(".git"), b"gitdir: elsewhere\n").expect(".git file");
    assert!(
        !is_managed_run_worktree(&source.display().to_string(), &stray.display().to_string()),
        "only the managed run directory may be inspected as a worktree",
    );
}

#[test]
fn a_managed_directory_without_git_metadata_is_not_approved() {
    use crate::is_managed_run_worktree;
    let dir = TempDir::new().expect("temp dir");
    let source = dir.path().join("source");
    let worktree = source.join(".grokptah/worktrees/runs/run-1");
    fs::create_dir_all(&worktree).expect("worktree");
    let source_text = source.display().to_string();
    let worktree_text = worktree.display().to_string();
    assert!(
        !is_managed_run_worktree(&source_text, &worktree_text),
        "a bare directory in the managed path is not a run worktree",
    );
    fs::write(worktree.join(".git"), b"gitdir: elsewhere\n").expect(".git file");
    assert!(is_managed_run_worktree(&source_text, &worktree_text));
}

#[test]
fn the_managed_root_itself_is_never_a_worktree() {
    use crate::is_managed_run_worktree;
    let dir = TempDir::new().expect("temp dir");
    let source = dir.path().join("source");
    let managed = source.join(".grokptah/worktrees/runs");
    fs::create_dir_all(&managed).expect("managed root");
    fs::write(managed.join(".git"), b"gitdir: elsewhere\n").expect(".git file");
    assert!(!is_managed_run_worktree(
        &source.display().to_string(),
        &managed.display().to_string()
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_worktree_pointing_out_of_the_managed_path_is_not_approved() {
    use crate::is_managed_run_worktree;
    let dir = TempDir::new().expect("temp dir");
    let source = dir.path().join("source");
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir_all(source.join(".grokptah/worktrees/runs")).expect("managed root");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    fs::write(elsewhere.join(".git"), b"gitdir: elsewhere\n").expect(".git file");
    std::os::unix::fs::symlink(&elsewhere, source.join(".grokptah/worktrees/runs/run-1"))
        .expect("symlink");
    assert!(
        !is_managed_run_worktree(
            &source.display().to_string(),
            &source
                .join(".grokptah/worktrees/runs/run-1")
                .display()
                .to_string()
        ),
        "canonicalising the candidate defeats a link into the managed path",
    );
}

#[test]
fn a_missing_source_or_worktree_is_never_approved() {
    use crate::is_managed_run_worktree;
    let dir = TempDir::new().expect("temp dir");
    let absent = dir.path().join("absent").display().to_string();
    assert!(!is_managed_run_worktree(&absent, &absent));
}
