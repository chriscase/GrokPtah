//! Structured success oracles for live/offline parity evals.
//!
//! Prefer compile/test exit codes and exact artifact checks over brittle
//! prose substrings (`must_not_contain: "out>"` false-negative class).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// One success specification (JSON `success` object or nested `checks` entry).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuccessSpec {
    /// Legacy substring predicates — allowed for offline smokes only.
    FileContains {
        path: String,
        #[serde(default)]
        must_contain: Vec<String>,
        #[serde(default)]
        must_not_contain: Vec<String>,
        #[serde(default)]
        must_not_remove: Vec<String>,
        #[serde(default)]
        extra_checks: Vec<FilePredicates>,
    },
    /// Exact file body after newline normalization (LF, no trailing-only noise strip).
    ExactFile {
        path: String,
        /// Expected body as a string (use `\n` in JSON).
        #[serde(default)]
        expected: Option<String>,
        /// Or load expected body from this path relative to the work root.
        #[serde(default)]
        expected_from: Option<String>,
    },
    /// Run a command under the work root; success when exit code matches.
    Command {
        argv: Vec<String>,
        #[serde(default = "default_exit_zero")]
        exit_code: i32,
        #[serde(default = "default_cmd_timeout")]
        timeout_secs: u64,
    },
    FileExists {
        path: String,
    },
    FileAbsent {
        path: String,
    },
    /// All nested checks must pass (structured composite).
    All {
        checks: Vec<SuccessSpec>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct FilePredicates {
    pub path: String,
    #[serde(default)]
    pub must_contain: Vec<String>,
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    #[serde(default)]
    pub must_not_remove: Vec<String>,
}

fn default_exit_zero() -> i32 {
    0
}

fn default_cmd_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleResult {
    pub ok: bool,
    pub detail: String,
}

/// Evaluate a success oracle against a fixture work directory.
pub fn evaluate(root: &Path, spec: &SuccessSpec) -> OracleResult {
    match spec {
        SuccessSpec::FileContains {
            path,
            must_contain,
            must_not_contain,
            must_not_remove,
            extra_checks,
        } => {
            if let Some(fail) =
                file_predicates_fail(root, path, must_contain, must_not_contain, must_not_remove)
            {
                return OracleResult {
                    ok: false,
                    detail: fail,
                };
            }
            for extra in extra_checks {
                if let Some(fail) = file_predicates_fail(
                    root,
                    &extra.path,
                    &extra.must_contain,
                    &extra.must_not_contain,
                    &extra.must_not_remove,
                ) {
                    return OracleResult {
                        ok: false,
                        detail: format!("extra {}: {fail}", extra.path),
                    };
                }
            }
            OracleResult {
                ok: true,
                detail: "file_contains ok".into(),
            }
        }
        SuccessSpec::ExactFile {
            path,
            expected,
            expected_from,
        } => exact_file(root, path, expected.as_deref(), expected_from.as_deref()),
        SuccessSpec::Command {
            argv,
            exit_code,
            timeout_secs,
        } => run_command(root, argv, *exit_code, *timeout_secs),
        SuccessSpec::FileExists { path } => {
            let p = root.join(path);
            if p.is_file() {
                OracleResult {
                    ok: true,
                    detail: format!("exists {path}"),
                }
            } else {
                OracleResult {
                    ok: false,
                    detail: format!("missing file {path}"),
                }
            }
        }
        SuccessSpec::FileAbsent { path } => {
            let p = root.join(path);
            if p.exists() {
                OracleResult {
                    ok: false,
                    detail: format!("unexpected path {path}"),
                }
            } else {
                OracleResult {
                    ok: true,
                    detail: format!("absent {path}"),
                }
            }
        }
        SuccessSpec::All { checks } => {
            let mut parts = Vec::new();
            for (i, c) in checks.iter().enumerate() {
                let r = evaluate(root, c);
                parts.push(format!("[{i}] {}", r.detail));
                if !r.ok {
                    return OracleResult {
                        ok: false,
                        detail: parts.join("; "),
                    };
                }
            }
            OracleResult {
                ok: true,
                detail: parts.join("; "),
            }
        }
    }
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn exact_file(
    root: &Path,
    path: &str,
    expected: Option<&str>,
    expected_from: Option<&str>,
) -> OracleResult {
    let p = root.join(path);
    let Ok(actual) = fs::read_to_string(&p) else {
        return OracleResult {
            ok: false,
            detail: format!("exact_file: cannot read {path}"),
        };
    };
    let expected_body = if let Some(e) = expected {
        e.to_string()
    } else if let Some(rel) = expected_from {
        match fs::read_to_string(root.join(rel)) {
            Ok(s) => s,
            Err(_) => {
                return OracleResult {
                    ok: false,
                    detail: format!("exact_file: cannot read expected_from {rel}"),
                };
            }
        }
    } else {
        return OracleResult {
            ok: false,
            detail: "exact_file: need expected or expected_from".into(),
        };
    };
    let a = normalize_newlines(&actual);
    let e = normalize_newlines(&expected_body);
    if a == e {
        OracleResult {
            ok: true,
            detail: format!("exact_file {path}"),
        }
    } else {
        OracleResult {
            ok: false,
            detail: format!(
                "exact_file mismatch {path} (actual {} bytes, expected {} bytes)",
                a.len(),
                e.len()
            ),
        }
    }
}

fn file_predicates_fail(
    root: &Path,
    path: &str,
    must_contain: &[String],
    must_not_contain: &[String],
    must_not_remove: &[String],
) -> Option<String> {
    let path_buf = root.join(path);
    let Ok(body) = fs::read_to_string(&path_buf) else {
        return Some(format!("cannot read {path}"));
    };
    for s in must_contain {
        if !body.contains(s) {
            return Some(format!("missing must_contain in {path}: {s:?}"));
        }
    }
    for s in must_not_contain {
        if body.contains(s) {
            return Some(format!("hit must_not_contain in {path}: {s:?}"));
        }
    }
    for s in must_not_remove {
        if !body.contains(s) {
            return Some(format!("missing must_not_remove in {path}: {s:?}"));
        }
    }
    None
}

fn run_command(root: &Path, argv: &[String], want_exit: i32, timeout_secs: u64) -> OracleResult {
    if argv.is_empty() {
        return OracleResult {
            ok: false,
            detail: "command: empty argv".into(),
        };
    }
    use std::sync::mpsc;
    let root = root.to_path_buf();
    let argv_owned = argv.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = Command::new(&argv_owned[0]);
        cmd.args(&argv_owned[1..])
            .current_dir(&root)
            .env("CARGO_TERM_COLOR", "never")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let res = cmd.output();
        let _ = tx.send((argv_owned, res));
    });
    match rx.recv_timeout(Duration::from_secs(timeout_secs.max(1))) {
        Ok((argv_done, Ok(out))) => {
            let code = out.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let tail = format!(
                "exit={code} want={want_exit}; stderr_tail={}",
                tail_str(&stderr, 400)
            );
            if code == want_exit {
                OracleResult {
                    ok: true,
                    detail: format!(
                        "command ok: {argv_done:?}; {tail}; stdout_tail={}",
                        tail_str(&stdout, 200)
                    ),
                }
            } else {
                OracleResult {
                    ok: false,
                    detail: format!("command fail: {argv_done:?}; {tail}"),
                }
            }
        }
        Ok((_argv_done, Err(e))) => OracleResult {
            ok: false,
            detail: format!("command error: {e}"),
        },
        Err(_) => OracleResult {
            ok: false,
            detail: format!("command timeout after {timeout_secs}s: {argv:?}"),
        },
    }
}

fn tail_str(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.len() <= n {
        t.to_string()
    } else {
        t[t.len() - n..].to_string()
    }
}

/// Load tasks.json as raw JSON values (for offline oracle self-tests).
pub fn load_tasks_json(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Parse a single task's success field from JSON.
pub fn parse_success(value: &serde_json::Value) -> Result<SuccessSpec, String> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}

/// Absolute path helper for tests that locate repo `evals/`.
pub fn find_evals_root() -> Option<PathBuf> {
    // examples run from bridge crate; tests from bridge crate
    let candidates = [
        PathBuf::from("../../../evals"),
        PathBuf::from("../../../../evals"),
        PathBuf::from("evals"),
        PathBuf::from("../evals"),
    ];
    for c in candidates {
        if c.join("tasks.json").is_file() {
            return Some(dunce::canonicalize(&c).unwrap_or(c));
        }
    }
    // Walk up from CARGO_MANIFEST_DIR
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut p = PathBuf::from(manifest);
        for _ in 0..6 {
            let try_e = p.join("evals/tasks.json");
            if try_e.is_file() {
                return Some(p.join("evals"));
            }
            if !p.pop() {
                break;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[test]
    fn exact_file_true_positive_and_negative() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "out.txt", "alpha\n");
        write_file(dir.path(), "oracle/expected.txt", "alpha\n");
        let ok = SuccessSpec::ExactFile {
            path: "out.txt".into(),
            expected: None,
            expected_from: Some("oracle/expected.txt".into()),
        };
        assert!(evaluate(dir.path(), &ok).ok);
        let bad = SuccessSpec::ExactFile {
            path: "out.txt".into(),
            expected: Some("beta\n".into()),
            expected_from: None,
        };
        assert!(!evaluate(dir.path(), &bad).ok);
    }

    #[test]
    fn file_contains_must_not_false_negative_class() {
        // Substring "out>" matching a comment would fail a naive check; we still
        // support file_contains for smokes but hard tasks should use command.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "src/emitter.rs",
            "// old bug was out>\npub fn emit() { format!(\"OUT:{x}\") }\n",
        );
        let brittle = SuccessSpec::FileContains {
            path: "src/emitter.rs".into(),
            must_contain: vec!["OUT:".into()],
            must_not_contain: vec!["out>".into()],
            must_not_remove: vec![],
            extra_checks: vec![],
        };
        // This documents the false-negative: comment contains out>
        assert!(
            !evaluate(dir.path(), &brittle).ok,
            "brittle substring wrongly fails on comments — hard tasks must not use this alone"
        );
        // Structured alternative: exact format! line via must_not on precise token
        let precise = SuccessSpec::FileContains {
            path: "src/emitter.rs".into(),
            must_contain: vec!["format!(\"OUT:".into()],
            must_not_contain: vec!["format!(\"out>".into()],
            must_not_remove: vec![],
            extra_checks: vec![],
        };
        assert!(evaluate(dir.path(), &precise).ok);
    }

    #[test]
    fn command_oracle_pass_and_fail() {
        let dir = tempfile::tempdir().unwrap();
        let pass = SuccessSpec::Command {
            argv: vec!["true".into()],
            exit_code: 0,
            timeout_secs: 10,
        };
        assert!(evaluate(dir.path(), &pass).ok);
        let fail = SuccessSpec::Command {
            argv: vec!["false".into()],
            exit_code: 0,
            timeout_secs: 10,
        };
        assert!(!evaluate(dir.path(), &fail).ok);
    }

    #[test]
    fn all_composite_short_circuits() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", "ok\n");
        let spec = SuccessSpec::All {
            checks: vec![
                SuccessSpec::FileExists {
                    path: "a.txt".into(),
                },
                SuccessSpec::FileAbsent {
                    path: "b.txt".into(),
                },
                SuccessSpec::ExactFile {
                    path: "a.txt".into(),
                    expected: Some("ok\n".into()),
                    expected_from: None,
                },
            ],
        };
        assert!(evaluate(dir.path(), &spec).ok);
        write_file(dir.path(), "b.txt", "x");
        assert!(!evaluate(dir.path(), &spec).ok);
    }

    #[test]
    fn cargo_test_oracle_on_mini_crate() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Cargo.toml",
            r#"[package]
name = "mini"
version = "0.1.0"
edition = "2021"
"#,
        );
        write_file(
            dir.path(),
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n#[cfg(test)]\nmod tests { #[test] fn t() { assert_eq!(super::add(1,2), 3); } }\n",
        );
        let spec = SuccessSpec::Command {
            argv: vec![
                "cargo".into(),
                "test".into(),
                "--manifest-path".into(),
                "Cargo.toml".into(),
                "--quiet".into(),
            ],
            exit_code: 0,
            timeout_secs: 120,
        };
        let r = evaluate(dir.path(), &spec);
        assert!(r.ok, "detail={}", r.detail);

        // Break the code → oracle fails
        write_file(
            dir.path(),
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a - b }\n#[cfg(test)]\nmod tests { #[test] fn t() { assert_eq!(super::add(1,2), 3); } }\n",
        );
        let r2 = evaluate(dir.path(), &spec);
        assert!(!r2.ok, "broken crate must fail oracle");
    }

    #[test]
    fn offline_selftest_dirs_if_present() {
        let Some(evals) = find_evals_root() else {
            // Not fatal when running outside repo layout.
            return;
        };
        let pass = evals.join("oracle_selftest/pass_exact");
        let fail = evals.join("oracle_selftest/fail_exact");
        if pass.is_dir() {
            let spec = SuccessSpec::ExactFile {
                path: "artifact.txt".into(),
                expected: None,
                expected_from: Some("oracle/expected.txt".into()),
            };
            assert!(
                evaluate(&pass, &spec).ok,
                "pass_exact fixture should satisfy exact_file"
            );
        }
        if fail.is_dir() {
            let spec = SuccessSpec::ExactFile {
                path: "artifact.txt".into(),
                expected: None,
                expected_from: Some("oracle/expected.txt".into()),
            };
            assert!(
                !evaluate(&fail, &spec).ok,
                "fail_exact fixture should fail exact_file"
            );
        }
        let pass_cmd = evals.join("oracle_selftest/pass_cmd");
        if pass_cmd.is_dir() {
            let spec = SuccessSpec::Command {
                argv: vec!["true".into()],
                exit_code: 0,
                timeout_secs: 5,
            };
            assert!(evaluate(&pass_cmd, &spec).ok);
        }
        let fail_cmd = evals.join("oracle_selftest/fail_cmd");
        if fail_cmd.is_dir() {
            let spec = SuccessSpec::Command {
                argv: vec!["false".into()],
                exit_code: 0,
                timeout_secs: 5,
            };
            assert!(!evaluate(&fail_cmd, &spec).ok);
        }
    }

    #[test]
    fn tasks_json_hard_tasks_use_structured_oracles() {
        let Some(evals) = find_evals_root() else {
            return;
        };
        let tasks_path = evals.join("tasks.json");
        if !tasks_path.is_file() {
            return;
        }
        let tasks: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&tasks_path).unwrap()).unwrap();
        let mut hard = 0usize;
        for t in &tasks {
            let id = t["id"].as_str().unwrap_or("");
            let diff = t["difficulty"].as_str().unwrap_or("smoke");
            if diff == "smoke" {
                continue;
            }
            hard += 1;
            let success = &t["success"];
            let ty = success["type"].as_str().unwrap_or("");
            assert!(
                matches!(ty, "command" | "all" | "exact_file"),
                "hard task {id} must not use brittle file_contains-only oracle, got type={ty}"
            );
            // Parse must succeed
            let _ = parse_success(success).expect("parse success");
        }
        // Suite should include hard tasks once authored.
        if hard == 0 {
            // Allow empty during bootstrap of this module alone.
            let _ = writeln!(std::io::stderr(), "note: no hard tasks yet in tasks.json");
        }
    }
}
