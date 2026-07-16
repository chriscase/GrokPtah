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
                format!("{}…", &chunk[..room])
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
        for entry in WalkDir::new(&dir).max_depth(2).into_iter().filter_map(|e| e.ok()) {
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
                    format!("{}…", &chunk[..room])
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
        format!("{}\n… (truncated)", &raw[..max])
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
                || path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| path_matches_simple(rest.trim_start_matches("*/"), name)
                        || path_matches_simple(rest, name));
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
        assert!(hits.iter().any(|h| h.ends_with("src/b.rs") || h == "src/b.rs"));
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
    fn loads_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Always use tabs.").unwrap();
        let (text, loaded) = load_project_instructions(dir.path());
        assert!(loaded.iter().any(|l| l == "AGENTS.md"));
        assert!(text.contains("Always use tabs"));
    }
}
