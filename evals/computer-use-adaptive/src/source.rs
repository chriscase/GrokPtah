//! Repository-observed source identity for campaign provenance.

use std::path::Path;
use std::process::Command;

use crate::report::SourceGate;
use crate::types::{EvalError, EvalResult};

fn git(repo: &Path, args: &[&str]) -> EvalResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| EvalError::Host(format!("cannot execute git: {err}")))?;
    if !output.status.success() {
        return Err(EvalError::Host(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn observe_source(
    repo: &Path,
    expected_head: Option<&str>,
    expected_base: &str,
) -> EvalResult<SourceGate> {
    let head = git(repo, &["rev-parse", "HEAD"])?;
    if let Some(expected) = expected_head {
        if head != expected {
            return Err(EvalError::Host(format!(
                "observed HEAD {head} != expected candidate {expected}"
            )));
        }
        let dirty = git(repo, &["status", "--porcelain"])?;
        if !dirty.is_empty() {
            return Err(EvalError::Host(
                "candidate worktree is dirty; HEAD/tree would not bind executed sources".into(),
            ));
        }
    }
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"])?;
    let base = git(repo, &["rev-parse", expected_base])?;
    let base_tree = git(repo, &["rev-parse", &format!("{base}^{{tree}}")])?;
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", &base, &head])
        .status()
        .map_err(|err| EvalError::Host(format!("cannot execute git: {err}")))?;
    if !status.success() {
        return Err(EvalError::Host(format!(
            "expected base {base} is not an ancestor of observed HEAD {head}"
        )));
    }
    Ok(SourceGate {
        git_sha: head,
        tree_sha: tree,
        base_git_sha: base,
        base_tree_sha: base_tree,
        base_is_ancestor: true,
        branch_note: "identity observed from git; synthetic campaign only".into(),
    })
}
