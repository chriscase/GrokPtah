//! Lexical containment, Windows path forms, and the no-follow walk.

use std::fs;

use super::support::{Fixture, make_managed_worktree};
use crate::{
    CandidateRoot, PathPolicy, SourceViewError, WindowsPathForm, case_fold, classify_windows_path,
    has_alternate_data_stream, has_illegal_character, has_stripped_tail, is_git_worktree_pointer,
    is_managed_run_worktree, is_reserved_device_name, normalize_request, segments_equal_folded,
};

// ------------------------------------------------------------- normalise

#[test]
fn normalises_a_plain_relative_path() {
    let fixture = Fixture::new();
    let contained =
        normalize_request(&fixture.root, "src/nested/deep.txt", PathPolicy::host()).expect("ok");
    assert_eq!(contained.segments(), ["src", "nested", "deep.txt"]);
    assert_eq!(contained.display(), "src/nested/deep.txt");
    assert_eq!(contained.prefix_display(2), "src/nested");
}

#[test]
fn dot_and_empty_segments_are_inert() {
    let fixture = Fixture::new();
    let contained =
        normalize_request(&fixture.root, "./src//./main.rs", PathPolicy::host()).expect("ok");
    assert_eq!(contained.display(), "src/main.rs");
}

#[test]
fn parent_escape_is_refused_rather_than_collapsed() {
    let fixture = Fixture::new();
    for attempt in [
        "..",
        "../secrets",
        "src/../../secrets",
        "src/nested/../../../etc/passwd",
        "a/b/../../..",
    ] {
        assert_eq!(
            normalize_request(&fixture.root, attempt, PathPolicy::host()).unwrap_err(),
            SourceViewError::ParentEscape,
            "`{attempt}` must be refused before any filesystem call",
        );
    }
}

#[test]
fn the_outer_request_is_trimmed_but_interior_segments_are_not() {
    let fixture = Fixture::new();
    // Whitespace around a pasted path is chrome; whitespace inside a segment
    // is part of a name and is judged by policy.
    assert_eq!(
        normalize_request(&fixture.root, "  src/main.rs  ", PathPolicy::host())
            .expect("ok")
            .display(),
        "src/main.rs",
    );
    assert_eq!(
        normalize_request(&fixture.root, "src/a b.txt", PathPolicy::strict())
            .expect("ok")
            .display(),
        "src/a b.txt",
    );
}

#[test]
fn empty_and_nul_requests_are_refused() {
    let fixture = Fixture::new();
    for (attempt, expected) in [
        ("", SourceViewError::EmptyPath),
        ("   ", SourceViewError::EmptyPath),
        ("./.", SourceViewError::EmptyPath),
        ("src/ma\0in.rs", SourceViewError::NulByte),
    ] {
        assert_eq!(
            normalize_request(&fixture.root, attempt, PathPolicy::host()).unwrap_err(),
            expected,
        );
    }
}

#[test]
fn an_absolute_request_inside_the_root_is_accepted() {
    let fixture = Fixture::new();
    let absolute = fixture.path("src/main.rs");
    let contained = normalize_request(
        &fixture.root,
        &absolute.to_string_lossy(),
        PathPolicy::host(),
    )
    .expect("ok");
    assert_eq!(contained.display(), "src/main.rs");
}

#[test]
fn a_sibling_sharing_a_string_prefix_is_not_containment() {
    let fixture = Fixture::new();
    let sibling = fixture.root.with_file_name(format!(
        "{}-secrets",
        fixture.root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&sibling).expect("sibling");
    let leak = sibling.join("leak.txt");
    fs::write(&leak, b"nope").expect("leak");

    assert_eq!(
        normalize_request(&fixture.root, &leak.to_string_lossy(), PathPolicy::host()).unwrap_err(),
        SourceViewError::AbsolutePathOutsideRoot,
    );
    fs::remove_dir_all(&sibling).ok();
}

#[cfg(not(windows))]
#[test]
fn a_unix_absolute_path_outside_the_root_is_refused_as_such() {
    let fixture = Fixture::new();
    assert_eq!(
        normalize_request(&fixture.root, "/etc/passwd", PathPolicy::host()).unwrap_err(),
        SourceViewError::AbsolutePathOutsideRoot,
    );
}

// --------------------------------------------------------- windows forms

#[test]
fn windows_path_forms_are_classified_on_every_platform() {
    for (input, expected) in [
        ("src/main.rs", WindowsPathForm::Relative),
        ("src\\main.rs", WindowsPathForm::Relative),
        ("C:\\repo\\main.rs", WindowsPathForm::DriveAbsolute),
        ("c:/repo/main.rs", WindowsPathForm::DriveAbsolute),
        ("C:main.rs", WindowsPathForm::DriveRelative),
        ("\\repo\\main.rs", WindowsPathForm::RootRelative),
        ("/repo/main.rs", WindowsPathForm::RootRelative),
        ("\\\\server\\share\\f.txt", WindowsPathForm::Unc),
        ("\\\\?\\C:\\repo\\f.txt", WindowsPathForm::VerbatimDevice),
        ("\\\\.\\PhysicalDrive0", WindowsPathForm::LocalDevice),
    ] {
        assert_eq!(
            classify_windows_path(input),
            expected,
            "classifying `{input}`"
        );
    }
    assert!(WindowsPathForm::Relative.is_readable_request());
    assert!(!WindowsPathForm::Unc.is_readable_request());
    assert!(!WindowsPathForm::VerbatimDevice.is_readable_request());
}

#[test]
fn unsupported_path_forms_are_refused_on_every_platform() {
    let fixture = Fixture::new();
    for attempt in [
        "\\\\server\\share\\secret.txt",
        "\\\\?\\C:\\repo\\secret.txt",
        "\\\\.\\PhysicalDrive0",
        "C:secret.txt",
    ] {
        assert_eq!(
            normalize_request(&fixture.root, attempt, PathPolicy::host()).unwrap_err(),
            SourceViewError::UnsupportedPathForm,
            "`{attempt}` reaches something other than the file it names",
        );
    }
}

#[test]
fn reserved_device_names_are_recognised_with_any_extension_or_case() {
    for reserved in [
        "CON", "con", "NUL", "nul.txt", "AUX", "PRN.log", "COM1", "com9.txt", "LPT1", "CONIN$",
        "nul.", "NUL ",
    ] {
        assert!(
            is_reserved_device_name(reserved),
            "`{reserved}` names a device, not a file",
        );
    }
    for ordinary in [
        "console.ts",
        "nullable.rs",
        "com.rs",
        "lptx",
        "auxiliary.md",
        "connect",
    ] {
        assert!(
            !is_reserved_device_name(ordinary),
            "`{ordinary}` is an ordinary name"
        );
    }
}

#[test]
fn windows_naming_rules_are_enforced_under_the_strict_policy_everywhere() {
    let fixture = Fixture::new();
    for (attempt, expected) in [
        (
            "src/NUL",
            SourceViewError::ReservedDeviceName {
                segment: "NUL".into(),
            },
        ),
        (
            "src/notes.txt:hidden",
            SourceViewError::AlternateDataStream {
                segment: "notes.txt:hidden".into(),
            },
        ),
        (
            "src/report.",
            SourceViewError::InvalidComponent {
                segment: "report.".into(),
            },
        ),
        (
            "src/trailing /f.txt",
            SourceViewError::InvalidComponent {
                segment: "trailing ".into(),
            },
        ),
        (
            "src/we*rd",
            SourceViewError::InvalidComponent {
                segment: "we*rd".into(),
            },
        ),
    ] {
        assert_eq!(
            normalize_request(&fixture.root, attempt, PathPolicy::strict()).unwrap_err(),
            expected,
            "strict policy must refuse `{attempt}`",
        );
    }
}

#[cfg(not(windows))]
#[test]
fn the_host_policy_on_unix_permits_names_windows_would_refuse() {
    let fixture = Fixture::new();
    // These are ordinary, legal Unix filenames; refusing them on Unix would be
    // a false positive rather than containment.
    for attempt in ["src/we*rd", "src/report.", "src/a:b"] {
        assert!(
            normalize_request(&fixture.root, attempt, PathPolicy::host()).is_ok(),
            "`{attempt}` is a legal Unix name",
        );
    }
}

#[test]
fn stream_tail_and_illegal_character_predicates_are_exact() {
    assert!(has_alternate_data_stream("notes.txt:hidden"));
    assert!(!has_alternate_data_stream("notes.txt"));
    assert!(has_stripped_tail("report."));
    assert!(has_stripped_tail("report "));
    assert!(!has_stripped_tail("report.txt"));
    assert!(has_illegal_character("a<b"));
    assert!(has_illegal_character("a\u{1}b"));
    assert!(!has_illegal_character("a-b_c.d"));
}

#[test]
fn case_folding_is_ascii_only_so_it_fails_closed() {
    assert_eq!(case_fold("Cargo.TOML"), "CARGO.TOML");
    assert!(segments_equal_folded("Src", "sRC"));
    assert!(
        !segments_equal_folded("straße", "STRASSE"),
        "over-folding would make distinct names compare equal",
    );
}

// ------------------------------------------------------------- no-follow

#[cfg(unix)]
#[test]
fn a_symlinked_file_inside_the_root_is_refused() {
    let fixture = Fixture::new();
    fixture.write("real.txt", b"contained\n");
    std::os::unix::fs::symlink(fixture.path("real.txt"), fixture.path("link.txt"))
        .expect("symlink");
    let token = fixture.token();

    assert!(matches!(
        fixture.open(&token, "link.txt").unwrap_err(),
        SourceViewError::SymlinkRejected { .. },
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_component_is_refused_and_named() {
    let fixture = Fixture::new();
    let outside = fixture
        .root
        .parent()
        .expect("parent")
        .join("outside-target");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("secret.txt"), b"secret\n").expect("secret");
    std::os::unix::fs::symlink(&outside, fixture.path("escape")).expect("dir symlink");
    let token = fixture.token();

    assert_eq!(
        fixture.open(&token, "escape/secret.txt").unwrap_err(),
        SourceViewError::SymlinkRejected {
            segment: "escape".into()
        },
        "the first linked component is named, and nothing above the root is",
    );
    fs::remove_dir_all(&outside).ok();
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_refused_as_a_link_not_as_missing() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(fixture.path("nowhere.txt"), fixture.path("dangling.txt"))
        .expect("symlink");
    let token = fixture.token();
    assert!(matches!(
        fixture.open(&token, "dangling.txt").unwrap_err(),
        SourceViewError::SymlinkRejected { .. },
    ));
}

#[cfg(unix)]
#[test]
fn an_absolute_symlink_to_a_directory_outside_cannot_be_walked_through() {
    let fixture = Fixture::new();
    let outside = fixture.root.parent().expect("parent").join("outside-tree");
    fs::create_dir_all(outside.join("inner")).expect("outside tree");
    fs::write(outside.join("inner/leak.txt"), b"leak\n").expect("leak");
    std::os::unix::fs::symlink(&outside, fixture.path("src/hop")).expect("symlink");
    let token = fixture.token();

    assert!(matches!(
        fixture.open(&token, "src/hop/inner/leak.txt").unwrap_err(),
        SourceViewError::SymlinkRejected { .. },
    ));
    fs::remove_dir_all(&outside).ok();
}

#[test]
fn a_directory_is_not_a_file() {
    let fixture = Fixture::new();
    let token = fixture.token();
    assert_eq!(
        fixture.open(&token, "src").unwrap_err(),
        SourceViewError::NotAFile {
            segment: "src".into()
        },
    );
}

#[test]
fn a_missing_file_is_reported_relative_to_the_root() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let error = fixture.open(&token, "src/absent.rs").unwrap_err();
    assert_eq!(
        error,
        SourceViewError::NotFound {
            segment: "src/absent.rs".into()
        },
    );
    assert!(
        !error
            .to_string()
            .contains(&fixture.root.display().to_string()),
        "an error must never name an absolute host path",
    );
}

#[test]
fn descending_through_a_file_is_refused() {
    let fixture = Fixture::new();
    let token = fixture.token();
    assert_eq!(
        fixture.open(&token, "src/main.rs/inner.txt").unwrap_err(),
        SourceViewError::NotAFile {
            segment: "src/main.rs".into()
        },
    );
}

// ---------------------------------------------------- non-UTF-8 names

#[cfg(unix)]
#[test]
fn a_root_whose_path_contains_non_utf8_bytes_still_works_end_to_end() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new();
    let odd = fixture.root.join(OsString::from_vec(vec![
        b'w', b'e', b'i', b'r', b'd', 0xff, 0xfe,
    ]));
    fs::create_dir_all(&odd).expect("odd dir");
    fs::write(odd.join("inside.txt"), b"reachable\n").expect("inside");

    let snapshot = fixture.snapshot_with(&[CandidateRoot::workspace(&odd)]);
    assert_eq!(
        snapshot.roots.len(),
        1,
        "a non-UTF-8 root is still approvable"
    );
    let document = fixture
        .open(&snapshot.roots[0].token, "inside.txt")
        .expect("read");
    assert_eq!(Fixture::chunk_text(&document), "reachable");
    // The digest is over the real bytes, so two roots differing only in an
    // unpaired byte do not collide.
    assert_eq!(snapshot.roots[0].path_digest.len(), 64);
}

#[cfg(unix)]
#[test]
fn a_file_whose_name_is_not_utf8_is_simply_unreachable() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new();
    let name = OsString::from_vec(vec![b'b', b'a', b'd', 0x80, b'.', b't', b'x', b't']);
    fs::write(fixture.root.join(&name), b"unreachable\n").expect("odd file");
    let token = fixture.token();

    // Every request arrives as UTF-8 text, so this name cannot be spelled. It
    // is refused as missing rather than approximated with replacements.
    assert!(matches!(
        fixture.open(&token, "bad\u{FFFD}.txt").unwrap_err(),
        SourceViewError::NotFound { .. },
    ));
}

// -------------------------------------------------- managed worktrees

#[test]
fn a_managed_worktree_matches_the_promotion_test_exactly() {
    let fixture = Fixture::new();
    let worktree = make_managed_worktree(&fixture.root, "run-1");
    assert!(is_managed_run_worktree(&fixture.root, &worktree));
    assert!(is_git_worktree_pointer(&worktree));
}

#[test]
fn a_directory_git_marker_is_a_clone_not_a_worktree() {
    let fixture = Fixture::new();
    let worktree = fixture.root.join(".grokptah/worktrees/runs/run-clone");
    fs::create_dir_all(worktree.join(".git")).expect("git dir");
    assert!(
        !is_managed_run_worktree(&fixture.root, &worktree),
        "promotion requires a pointer file; a `.git` directory is a clone",
    );
}

#[test]
fn a_git_pointer_that_does_not_reference_worktrees_is_refused() {
    let fixture = Fixture::new();
    let worktree = fixture.root.join(".grokptah/worktrees/runs/run-odd");
    fs::create_dir_all(&worktree).expect("dir");
    fs::write(worktree.join(".git"), b"gitdir: /elsewhere/.git\n").expect("pointer");
    assert!(!is_managed_run_worktree(&fixture.root, &worktree));
}

#[test]
fn the_managed_root_itself_is_never_a_worktree() {
    let fixture = Fixture::new();
    let managed = fixture.root.join(".grokptah/worktrees/runs");
    fs::create_dir_all(&managed).expect("managed");
    fs::write(managed.join(".git"), b"gitdir: x/worktrees/y\n").expect("pointer");
    assert!(!is_managed_run_worktree(&fixture.root, &managed));
}

#[test]
fn a_worktree_outside_the_managed_directory_is_refused() {
    let fixture = Fixture::new();
    let stray = fixture.root.join("stray");
    fs::create_dir_all(fixture.root.join(".grokptah/worktrees/runs")).expect("managed");
    fs::create_dir_all(&stray).expect("stray");
    fs::write(stray.join(".git"), b"gitdir: x/worktrees/y\n").expect("pointer");
    assert!(!is_managed_run_worktree(&fixture.root, &stray));
}

#[cfg(unix)]
#[test]
fn a_symlink_into_the_managed_directory_does_not_qualify() {
    let fixture = Fixture::new();
    let elsewhere = fixture.root.parent().expect("parent").join("elsewhere-wt");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    fs::write(elsewhere.join(".git"), b"gitdir: x/worktrees/y\n").expect("pointer");
    fs::create_dir_all(fixture.root.join(".grokptah/worktrees/runs")).expect("managed");
    let link = fixture.root.join(".grokptah/worktrees/runs/run-link");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");

    assert!(
        !is_managed_run_worktree(&fixture.root, &link),
        "canonicalising the candidate defeats a link into the managed path",
    );
    fs::remove_dir_all(&elsewhere).ok();
}
