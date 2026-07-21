//! Project instructions discovery for Build agent system context.
//!
//! Loads AGENTS.md / CLAUDE.md / .grok rules (and similar) under the session cwd
//! so the model sees the same class of project guidance as Grok Build.

use std::path::Path;

use walkdir::WalkDir;

const MAX_TOTAL_CHARS: usize = 24_000;
const MAX_FILE_CHARS: usize = 12_000;

const ROOT_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "Claude.md",
    "AGENT.md",
    ".grok/rules.md",
    ".grok/AGENTS.md",
    ".grokptah/rules.md",
    "docs/AGENTS.md",
    "docs/ARCHITECTURE.md",
];

/// Collect project instruction text + list of loaded relative paths.
pub fn load_project_instructions(cwd: &Path) -> (String, Vec<String>) {
    let mut parts = Vec::new();
    let mut loaded = Vec::new();
    let mut total = 0usize;

    for rel in ROOT_FILES {
        if total >= MAX_TOTAL_CHARS {
            break;
        }
        let path = cwd.join(rel);
        if let Some((chunk, _)) = read_capped(&path, MAX_FILE_CHARS) {
            let room = MAX_TOTAL_CHARS.saturating_sub(total);
            let text = if chunk.len() > room {
                crate::textutil::truncate_with_marker(&chunk, room, "…")
            } else {
                chunk
            };
            total += text.len();
            loaded.push(rel.to_string());
            parts.push(format!("### {rel}\n{text}"));
        }
    }

    // Shallow scan for additional *.md under .grok/ and .grokptah/
    for dir_name in [".grok", ".grokptah"] {
        if total >= MAX_TOTAL_CHARS {
            break;
        }
        let dir = cwd.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if total >= MAX_TOTAL_CHARS {
                break;
            }
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("md") {
                continue;
            }
            let rel = p
                .strip_prefix(cwd)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string());
            if loaded.iter().any(|l| l == &rel) {
                continue;
            }
            if let Some((chunk, _)) = read_capped(p, MAX_FILE_CHARS / 2) {
                let room = MAX_TOTAL_CHARS.saturating_sub(total);
                if room < 200 {
                    break;
                }
                let text = if chunk.len() > room {
                    crate::textutil::truncate_with_marker(&chunk, room, "…")
                } else {
                    chunk
                };
                total += text.len();
                loaded.push(rel.clone());
                parts.push(format!("### {rel}\n{text}"));
            }
        }
    }

    if parts.is_empty() {
        (String::new(), loaded)
    } else {
        (
            format!(
                "Project instructions (from the working tree; follow these):\n\n{}",
                parts.join("\n\n")
            ),
            loaded,
        )
    }
}

fn read_capped(path: &Path, max: usize) -> Option<(String, usize)> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let text = if raw.len() > max {
        crate::textutil::truncate_with_marker(&raw, max, "\n… (truncated)")
    } else {
        raw
    };
    Some((text, path.to_path_buf().components().count()))
}

/// List relative file paths under `cwd` matching a simple glob (`*` and `?` only).
pub fn glob_files(cwd: &Path, pattern: &str, limit: usize) -> Vec<String> {
    let limit = limit.clamp(1, 500);
    let mut out = Vec::new();
    let pat = pattern.trim().trim_start_matches("./");
    for entry in WalkDir::new(cwd)
        .max_depth(12)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        // Skip heavy / noisy dirs
        let rel = match p.strip_prefix(cwd) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if rel_s.split('/').any(|c| {
            matches!(
                c,
                "node_modules" | "target" | ".git" | "dist" | "build" | ".next" | "vendor"
            )
        }) {
            continue;
        }
        if glob_match(pat, &rel_s) {
            out.push(rel_s);
            if out.len() >= limit {
                break;
            }
        }
    }
    out.sort();
    out
}

/// Minimal glob: `*` any run of chars except `/` unless ** used; `**` any path; `?` one char.
fn glob_match(pattern: &str, path: &str) -> bool {
    // Normalize **.rs style
    let pattern = pattern.trim_start_matches('/');
    let path = path.trim_start_matches('/');
    if pattern.contains("**") {
        // Convert **/*.ext → suffix match on segments
        if let Some(rest) = pattern.strip_prefix("**/") {
            return path_matches_simple(rest, path)
                || path.rsplit('/').next().is_some_and(|name| {
                    path_matches_simple(rest.trim_start_matches("*/"), name)
                        || path_matches_simple(rest, name)
                });
        }
    }
    // Also allow bare *.rs matching any depth basename
    if pattern.starts_with("*.") && !pattern.contains('/') {
        return path
            .rsplit('/')
            .next()
            .is_some_and(|name| path_matches_simple(pattern, name));
    }
    path_matches_simple(pattern, path)
}

fn path_matches_simple(pattern: &str, path: &str) -> bool {
    let pb: Vec<char> = pattern.chars().collect();
    let sb: Vec<char> = path.chars().collect();
    match_rec(&pb, 0, &sb, 0)
}

fn match_rec(pat: &[char], pi: usize, s: &[char], si: usize) -> bool {
    if pi == pat.len() {
        return si == s.len();
    }
    match pat[pi] {
        '*' => {
            // * does not cross '/'
            let mut i = si;
            loop {
                if match_rec(pat, pi + 1, s, i) {
                    return true;
                }
                if i >= s.len() || s[i] == '/' {
                    break;
                }
                i += 1;
            }
            // also zero-length
            match_rec(pat, pi + 1, s, si)
        }
        '?' => {
            if si < s.len() && s[si] != '/' {
                match_rec(pat, pi + 1, s, si + 1)
            } else {
                false
            }
        }
        c => {
            if si < s.len() && s[si] == c {
                match_rec(pat, pi + 1, s, si + 1)
            } else {
                false
            }
        }
    }
}

/// Apply a unified-diff-like patch with simple `*** Begin Patch` / `*** Update File:` blocks
/// or a JSON-style search/replace payload.
pub fn apply_patch(cwd: &Path, patch: &str) -> anyhow::Result<String> {
    // Format A: search/replace markers
    // *** Update File: path
    // <<<<<<< SEARCH
    // old
    // =======
    // new
    // >>>>>>> REPLACE
    let mut report = Vec::new();
    let mut rest = patch.trim();
    if rest.is_empty() {
        anyhow::bail!("empty patch");
    }

    // Also accept: path\n*** SEARCH\n...\n*** REPLACE\n...
    while let Some(idx) = rest.find("*** Update File:") {
        rest = rest[idx + "*** Update File:".len()..].trim_start();
        let (path_line, after_path) = rest
            .split_once('\n')
            .map(|(a, b)| (a.trim(), b))
            .unwrap_or((rest.trim(), ""));
        if path_line.is_empty() {
            anyhow::bail!("missing path after *** Update File:");
        }
        rest = after_path;
        let search_tag = if rest.contains("<<<<<<< SEARCH") {
            "<<<<<<< SEARCH"
        } else if rest.contains("*** SEARCH") {
            "*** SEARCH"
        } else {
            anyhow::bail!("patch for {path_line}: missing SEARCH block");
        };
        let replace_tag = if rest.contains(">>>>>>> REPLACE") {
            ">>>>>>> REPLACE"
        } else if rest.contains("*** REPLACE") {
            "*** REPLACE"
        } else {
            anyhow::bail!("patch for {path_line}: missing REPLACE marker");
        };
        let mid = if rest.contains("=======") {
            "======="
        } else if rest.contains("*** REPLACE") {
            "*** REPLACE"
        } else {
            anyhow::bail!("patch for {path_line}: missing separator");
        };

        let after_search = rest
            .split_once(search_tag)
            .map(|(_, b)| b.trim_start_matches(['\r', '\n']))
            .ok_or_else(|| anyhow::anyhow!("SEARCH"))?;
        let (old, after_old) = after_search
            .split_once(mid)
            .ok_or_else(|| anyhow::anyhow!("separator after SEARCH"))?;
        let after_old = after_old.trim_start_matches(['\r', '\n']);
        let (new, after_new) = after_old
            .split_once(replace_tag)
            .ok_or_else(|| anyhow::anyhow!("REPLACE end"))?;
        rest = after_new;

        let old = strip_trailing_nl(old);
        let new = strip_trailing_nl(new);
        let full = super::local_tools::resolve_under_cwd(cwd, path_line)?;
        let original = std::fs::read_to_string(&full)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", full.display()))?;
        if !original.contains(&old) {
            anyhow::bail!(
                "SEARCH block not found in {path_line} ({} chars search)",
                old.len()
            );
        }
        let updated = original.replacen(&old, &new, 1);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&full, &updated)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", full.display()))?;
        report.push(format!(
            "updated {path_line} ({} → {} bytes)",
            original.len(),
            updated.len()
        ));
    }

    if report.is_empty() {
        // Format B: single JSON { "path", "old_string", "new_string" }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(patch) {
            let path = v
                .get("path")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("json patch needs path"))?;
            let old = v
                .get("old_string")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("json patch needs old_string"))?;
            let new = v
                .get("new_string")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("json patch needs new_string"))?;
            let full = super::local_tools::resolve_under_cwd(cwd, path)?;
            let original = std::fs::read_to_string(&full)?;
            if !original.contains(old) {
                anyhow::bail!("old_string not found in {path}");
            }
            let updated = original.replacen(old, new, 1);
            std::fs::write(&full, &updated)?;
            return Ok(format!(
                "updated {path} ({} → {} bytes)",
                original.len(),
                updated.len()
            ));
        }
        anyhow::bail!(
            "unrecognized patch format; use *** Update File: path with <<<<<<< SEARCH / ======= / >>>>>>> REPLACE"
        );
    }
    Ok(report.join("\n"))
}

fn strip_trailing_nl(s: &str) -> String {
    s.trim_end_matches('\n').trim_end_matches('\r').to_string()
}

/// Soft total budget for *unmatched* skill bodies in the default catalog inject.
const MAX_SKILLS_CATALOG_CHARS: usize = 12_000;
/// Matched skills inject full Markdown (Build 0.2.109 parity intent) (#154).
const MAX_MATCHED_SKILL_BODY: usize = 200_000;
const MAX_MATCHED_SKILLS: usize = 8;
const MAX_MATCHED_TOTAL: usize = 400_000;

/// Git status for Build system context: branch + unstaged + untracked (#158).
/// Capped so huge dirty trees do not explode the prompt.
pub fn git_status_context(cwd: &Path) -> String {
    // Confirm this is a git work tree (empty repo still counts).
    let is_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output();
    match is_repo {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout)
                .trim()
                .to_ascii_lowercase();
            if v != "true" {
                return String::new();
            }
        }
        _ => return String::new(),
    }

    let mut out = String::from("## Git status (startup)\n");
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output();
    if let Ok(o) = branch {
        if o.status.success() {
            let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !b.is_empty() {
                out.push_str(&format!("Branch: {b}\n"));
            }
        } else {
            out.push_str("Branch: (unborn)\n");
        }
    }
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(cwd)
        .output();
    let Ok(o) = status else {
        return out;
    };
    if !o.status.success() {
        return out;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    if text.trim().is_empty() {
        out.push_str("Working tree clean.\n");
        return out;
    }
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut staged = Vec::new();
    for line in text.lines().take(200) {
        if line.len() < 3 {
            continue;
        }
        let x = line.as_bytes()[0] as char;
        let y = line.as_bytes()[1] as char;
        let path = line[3..].trim();
        if x == '?' && y == '?' {
            untracked.push(path.to_string());
        } else {
            if x != ' ' && x != '?' {
                staged.push(format!("{x} {path}"));
            }
            if y != ' ' && y != '?' {
                unstaged.push(format!("{y} {path}"));
            }
        }
    }
    if !staged.is_empty() {
        out.push_str("Staged:\n");
        for p in staged.into_iter().take(80) {
            out.push_str(&format!("  {p}\n"));
        }
    }
    if !unstaged.is_empty() {
        out.push_str("Unstaged:\n");
        for p in unstaged.into_iter().take(80) {
            out.push_str(&format!("  {p}\n"));
        }
    }
    if !untracked.is_empty() {
        out.push_str("Untracked:\n");
        for p in untracked.into_iter().take(80) {
            out.push_str(&format!("  {p}\n"));
        }
    }
    if out.len() > 6_000 {
        crate::textutil::truncate_with_marker(&out, 6_000, "\n… (git status truncated)")
    } else {
        out
    }
}

/// Load discovered skills (catalog + bodies) for Build agent system context.
///
/// `SkillInfo.id` is the filesystem path to the skill markdown.
/// When `task` is set, skills matching the task get **full** bodies (no 2.4k cap).
#[allow(dead_code)] // public helper; production path uses `load_skills_context_for_task`
pub fn load_skills_context(project: Option<&Path>) -> String {
    load_skills_context_for_task(project, None)
}

/// Like [`load_skills_context`] but prefers full bodies for skills matching `task` (#154).
pub fn load_skills_context_for_task(project: Option<&Path>, task: Option<&str>) -> String {
    let skills = crate::discover::discover_skills(project);
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Skills available for this session (follow a skill body when the user task matches):\n\n",
    );

    // Compact catalog always
    for s in &skills {
        let line = format!("- **{}**: {}\n", s.name, s.description);
        if out.len() + line.len() > MAX_SKILLS_CATALOG_CHARS / 2 {
            break;
        }
        out.push_str(&line);
    }
    out.push('\n');

    let task_l = task.map(|t| t.to_ascii_lowercase()).unwrap_or_default();
    let mut matched: Vec<&crate::types::SkillInfo> = Vec::new();
    let mut rest: Vec<&crate::types::SkillInfo> = Vec::new();
    for s in &skills {
        if !task_l.is_empty() && skill_matches_task(s, &task_l) {
            matched.push(s);
        } else {
            rest.push(s);
        }
    }

    let mut used_bodies = 0usize;
    let mut body_count = 0usize;

    // Full bodies for matched skills (#154 — no aggressive 2400-char truncate)
    for s in matched.into_iter().take(MAX_MATCHED_SKILLS) {
        if body_count >= MAX_MATCHED_SKILLS || used_bodies >= MAX_MATCHED_TOTAL {
            break;
        }
        let Ok(body) = std::fs::read_to_string(&s.id) else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let text = if body.len() > MAX_MATCHED_SKILL_BODY {
            crate::textutil::truncate_with_marker(body, MAX_MATCHED_SKILL_BODY, "…\n(truncated)")
        } else {
            body.to_string()
        };
        let block = format!("### Skill: {} (matched — full body)\n{}\n\n", s.name, text);
        out.push_str(&block);
        used_bodies += block.len();
        body_count += 1;
    }

    // Unmatched: short previews only (catalog already listed names)
    let preview_budget = MAX_SKILLS_CATALOG_CHARS;
    let mut preview_used = 0usize;
    for s in rest.into_iter().take(4) {
        if preview_used >= preview_budget {
            break;
        }
        let Ok(body) = std::fs::read_to_string(&s.id) else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let preview = crate::textutil::truncate_with_marker(
            body,
            400,
            "…\n(preview; open skill file for full text)",
        );
        let block = format!("### Skill: {} (preview)\n{}\n\n", s.name, preview);
        out.push_str(&block);
        preview_used += block.len();
    }
    out
}

fn skill_matches_task(skill: &crate::types::SkillInfo, task_lower: &str) -> bool {
    if task_lower.is_empty() {
        return false;
    }
    let name = skill.name.to_ascii_lowercase();
    let desc = skill.description.to_ascii_lowercase();
    if task_lower.contains(&name)
        || name
            .split(['-', '_', ' '])
            .any(|p| p.len() > 2 && task_lower.contains(p))
    {
        return true;
    }
    // token overlap on description words
    for w in desc.split_whitespace() {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        if w.len() >= 5 && task_lower.contains(w) {
            return true;
        }
    }
    false
}

/// Best-effort unified diff for a path after an edit (git, else summary).
pub fn diff_for_path(cwd: &Path, path: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["diff", "--", path])
        .current_dir(cwd)
        .output();
    if let Ok(o) = output {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        if !s.trim().is_empty() {
            return s.chars().take(12_000).collect();
        }
        // untracked new file
        let staged = std::process::Command::new("git")
            .args(["diff", "--no-index", "--", "/dev/null", path])
            .current_dir(cwd)
            .output();
        if let Ok(o) = staged {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            if !s.trim().is_empty() {
                return s.chars().take(12_000).collect();
            }
        }
    }
    format!("(updated {path})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn glob_basename_rs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "a").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/b.rs"), "b").unwrap();
        fs::write(dir.path().join("c.txt"), "c").unwrap();
        let hits = glob_files(dir.path(), "*.rs", 50);
        assert!(hits.iter().any(|h| h.ends_with("a.rs")));
        assert!(hits
            .iter()
            .any(|h| h.ends_with("src/b.rs") || h == "src/b.rs"));
        assert!(!hits.iter().any(|h| h.ends_with("c.txt")));
    }

    #[test]
    fn apply_json_patch() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        fs::write(&p, "hello world").unwrap();
        let report = apply_patch(
            dir.path(),
            r#"{"path":"f.txt","old_string":"world","new_string":"ptah"}"#,
        )
        .unwrap();
        assert!(report.contains("updated"));
        assert_eq!(fs::read_to_string(p).unwrap(), "hello ptah");
    }

    #[test]
    fn apply_multi_hunk_update_file_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("multi.txt");
        fs::write(&p, "line one\nline two\nline three\n").unwrap();
        let patch = r#"*** Update File: multi.txt
<<<<<<< SEARCH
line one
=======
LINE ONE
>>>>>>> REPLACE
*** Update File: multi.txt
<<<<<<< SEARCH
line three
=======
LINE THREE
>>>>>>> REPLACE
"#;
        let report = apply_patch(dir.path(), patch).unwrap();
        assert!(report.contains("updated"), "{report}");
        let body = fs::read_to_string(p).unwrap();
        assert!(body.contains("LINE ONE"), "{body}");
        assert!(body.contains("line two"), "{body}");
        assert!(body.contains("LINE THREE"), "{body}");
    }

    #[test]
    fn loads_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Always use tabs.").unwrap();
        let (text, loaded) = load_project_instructions(dir.path());
        assert!(loaded.iter().any(|l| l == "AGENTS.md"));
        assert!(text.contains("Always use tabs"));
    }

    #[test]
    fn loads_skills_into_context() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        fs::write(
            skills.join("review.md"),
            "# Review\n\nAlways mention edge cases.\n",
        )
        .unwrap();
        let ctx = load_skills_context(Some(dir.path()));
        assert!(
            ctx.contains("edge cases") || ctx.contains("Review") || ctx.contains("review"),
            "skills context: {ctx}"
        );
    }

    #[test]
    fn matched_skill_injects_full_body_over_2400_chars() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        let marker = "UNIQUE_SKILL_MARKER_END_OF_BODY";
        let body = format!("# bigskill\n\n{}\n{marker}\n", "x".repeat(5000));
        fs::write(skills.join("bigskill.md"), &body).unwrap();
        // discover looks under project skill dirs (`skills/`, `.grok/skills`, …)
        let ctx =
            load_skills_context_for_task(Some(dir.path()), Some("please run bigskill carefully"));
        assert!(
            ctx.contains(marker),
            "expected full body including end marker; ctx len {} tail {:?}",
            ctx.len(),
            ctx.chars().rev().take(80).collect::<String>()
        );
        assert!(
            !ctx.contains("(truncated)") || ctx.contains(marker),
            "matched skill should not lose end of body"
        );
    }

    #[test]
    fn git_status_includes_untracked_when_repo() {
        let dir = tempfile::tempdir().unwrap();
        let _ = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir.path())
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir.path())
            .output();
        fs::write(dir.path().join("orphan.txt"), "hi").unwrap();
        let ctx = git_status_context(dir.path());
        assert!(
            ctx.contains("orphan.txt") || ctx.contains("Untracked") || ctx.contains("Branch"),
            "git ctx: {ctx}"
        );
    }
}
