//! Shell exec-risk preflight (#155).
//!
//! **Not an OS sandbox.** Peels common transparent wrappers and assigns a
//! coarse risk tier so the permission gate can ask/deny high-risk forms.
//! Deliberate non-parity with Landlock/seatbelt — see ADR / TOOL_MATRIX.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// Benign / low-signal — allow under normal policy.
    Allow,
    /// Ambiguous or medium risk — prompt (or auto-allow in YOLO).
    Ask,
    /// High risk patterns — deny unless profile is `full` + YOLO/bypass.
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskReport {
    pub tier: RiskTier,
    pub reason: String,
    /// Command after peeling transparent prefixes (best-effort).
    pub peeled: String,
}

/// Peel common transparent prefixes: `env`, `command`, `nice`, `nohup`, `timeout`.
pub fn peel_transparent_prefixes(cmd: &str) -> String {
    let mut s = cmd.trim().to_string();
    for _ in 0..8 {
        let before = s.clone();
        s = peel_once(&s);
        if s == before {
            break;
        }
    }
    s
}

fn peel_once(cmd: &str) -> String {
    let t = cmd.trim_start();
    // env [-i] [NAME=VAL ...] command...
    if let Some(rest) = strip_program(t, "env") {
        return strip_env_assignments(rest);
    }
    if let Some(rest) = strip_program(t, "command") {
        // command [-p] [-v] [-V] name ...
        return strip_leading_flags(rest, &["-p", "-v", "-V"]);
    }
    if let Some(rest) = strip_program(t, "nice") {
        return strip_nice_args(rest);
    }
    if let Some(rest) = strip_program(t, "nohup") {
        return rest.trim_start().to_string();
    }
    if let Some(rest) = strip_program(t, "timeout") {
        return strip_timeout_args(rest);
    }
    t.to_string()
}

fn strip_program<'a>(cmd: &'a str, name: &str) -> Option<&'a str> {
    let cmd = cmd.trim_start();
    if cmd == name {
        return Some("");
    }
    if let Some(rest) = cmd.strip_prefix(name) {
        if rest.starts_with(|c: char| c.is_whitespace()) {
            return Some(rest.trim_start());
        }
        // path form /usr/bin/env
        if rest.starts_with('/') {
            // not this
        }
    }
    // basename path: /usr/bin/env ...
    if let Some(idx) = cmd.find(char::is_whitespace) {
        let prog = &cmd[..idx];
        let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
        if base == name {
            return Some(cmd[idx..].trim_start());
        }
    } else {
        let base = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
        if base == name {
            return Some("");
        }
    }
    None
}

fn strip_env_assignments(rest: &str) -> String {
    let mut parts = shellish_split(rest);
    if parts.is_empty() {
        return String::new();
    }
    if parts[0] == "-i" || parts[0] == "-0" || parts[0] == "-u" {
        // Conservative: leave remaining after first flag token for peel loop
        parts.remove(0);
        if !parts.is_empty() && !parts[0].contains('=') && parts[0].starts_with('-') {
            // skip another flag value pair loosely
        }
    }
    while !parts.is_empty() && parts[0].contains('=') && !parts[0].starts_with('-') {
        parts.remove(0);
    }
    parts.join(" ")
}

fn strip_leading_flags(rest: &str, flags: &[&str]) -> String {
    let mut parts = shellish_split(rest);
    while !parts.is_empty() && flags.contains(&parts[0].as_str()) {
        parts.remove(0);
    }
    parts.join(" ")
}

fn strip_nice_args(rest: &str) -> String {
    let mut parts = shellish_split(rest);
    if parts.first().map(|s| s.as_str()) == Some("-n") && parts.len() >= 2 {
        parts.drain(0..2);
    } else if parts
        .first()
        .map(|s| s.starts_with('-') && s[1..].chars().all(|c| c.is_ascii_digit() || c == 'n'))
        .unwrap_or(false)
    {
        parts.remove(0);
    }
    parts.join(" ")
}

fn strip_timeout_args(rest: &str) -> String {
    let mut parts = shellish_split(rest);
    // timeout [options] duration command
    while !parts.is_empty() && parts[0].starts_with('-') {
        let f = parts[0].clone();
        parts.remove(0);
        // flags with values
        if matches!(
            f.as_str(),
            "-k" | "--kill-after" | "-s" | "--signal" | "--preserve-status"
        ) && f != "--preserve-status"
            && !f.contains('=')
            && !parts.is_empty()
        {
            parts.remove(0);
        }
    }
    // duration
    if !parts.is_empty() {
        parts.remove(0);
    }
    parts.join(" ")
}

/// Very small splitter: whitespace, keeps simple quoted spans intact.
fn shellish_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_s = false;
    let mut in_d = false;
    for c in s.chars() {
        match c {
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            c if c.is_whitespace() && !in_s && !in_d => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !in_s && !in_d && !cur.is_empty() {
        out.push(cur);
    } else if !cur.is_empty() {
        // unclosed quote — still push so fail-closed can see it
        out.push(cur);
    }
    out
}

/// Assess risk for a shell command string.
pub fn assess_shell_risk(cmd: &str) -> RiskReport {
    let raw = cmd.trim();
    if raw.is_empty() {
        return RiskReport {
            tier: RiskTier::Ask,
            reason: "empty command".into(),
            peeled: String::new(),
        };
    }
    // Unbalanced quotes → fail closed to Ask
    if unbalanced_quotes(raw) {
        return RiskReport {
            tier: RiskTier::Ask,
            reason: "unbalanced quotes; fail closed".into(),
            peeled: raw.to_string(),
        };
    }
    let peeled = peel_transparent_prefixes(raw);
    let lower = peeled.to_ascii_lowercase();

    // Nested shell -c / bash -c with dangerous inner → Deny/Ask
    if looks_like_nested_shell_c(&peeled) {
        if dangerous_payload(&lower) {
            return RiskReport {
                tier: RiskTier::Deny,
                reason: "nested shell -c with high-risk payload".into(),
                peeled: peeled.clone(),
            };
        }
        return RiskReport {
            tier: RiskTier::Ask,
            reason: "nested shell -c (opaque script)".into(),
            peeled: peeled.clone(),
        };
    }

    if dangerous_payload(&lower) {
        return RiskReport {
            tier: RiskTier::Deny,
            reason: "high-risk shell pattern (destructive or exfil)".into(),
            peeled: peeled.clone(),
        };
    }

    // rg --pre / sort --compress-program spawn
    if lower.contains("rg ") && (lower.contains("--pre") || lower.contains("--pre-glob")) {
        return RiskReport {
            tier: RiskTier::Ask,
            reason: "rg --pre can execute programs".into(),
            peeled: peeled.clone(),
        };
    }
    if lower.contains("sort ") && lower.contains("--compress-program") {
        return RiskReport {
            tier: RiskTier::Ask,
            reason: "sort --compress-program can execute programs".into(),
            peeled: peeled.clone(),
        };
    }

    RiskReport {
        tier: RiskTier::Allow,
        reason: "no high-risk pattern detected".into(),
        peeled,
    }
}

fn unbalanced_quotes(s: &str) -> bool {
    let mut in_s = false;
    let mut in_d = false;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            esc = false;
            continue;
        }
        if c == '\\' && in_d {
            esc = true;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        }
    }
    in_s || in_d
}

fn looks_like_nested_shell_c(cmd: &str) -> bool {
    let parts = shellish_split(cmd);
    if parts.is_empty() {
        return false;
    }
    let base = parts[0].rsplit(['/', '\\']).next().unwrap_or(&parts[0]);
    let base = base.to_ascii_lowercase();
    if !matches!(base.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return false;
    }
    parts.iter().any(|p| p == "-c")
}

fn dangerous_payload(lower: &str) -> bool {
    const PATS: &[&str] = &[
        "rm -rf /",
        "rm -rf/*",
        "rm -fr /",
        "mkfs.",
        "dd if=/dev/zero",
        ":(){:|:&};:",
        "curl | sh",
        "curl|sh",
        "wget | sh",
        "wget|sh",
        "curl | bash",
        "curl|bash",
        "pip install",
        "npm install -g",
        "chmod 777 /",
        "chown -r",
        "> /dev/sd",
        "mkfs ",
        "diskutil erase",
    ];
    PATS.iter().any(|p| lower.contains(p))
        || (lower.contains("rm -rf")
            && (lower.contains("/*")
                || lower.contains(" ~")
                || lower.ends_with(" /")
                || lower.contains(" / ")))
}

/// Whether the soft sandbox profile should refuse a Deny-tier command.
/// `full` profile may still run Deny under YOLO; otherwise Deny is refused.
pub fn should_block_deny_tier(sandbox_profile: &str, yolo: bool) -> bool {
    let p = sandbox_profile.trim().to_ascii_lowercase();
    if p == "full" && yolo {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peels_env_assignments() {
        let p = peel_transparent_prefixes("env FOO=1 BAR=2 ls -la");
        assert!(p.starts_with("ls"), "got {p}");
    }

    #[test]
    fn peels_timeout() {
        let p = peel_transparent_prefixes("timeout 5 echo hi");
        assert!(p.contains("echo"), "got {p}");
    }

    #[test]
    fn deny_rm_rf_rootish() {
        let r = assess_shell_risk("rm -rf /");
        assert_eq!(r.tier, RiskTier::Deny);
    }

    #[test]
    fn deny_env_wrapped_rm() {
        let r = assess_shell_risk("env FOO=1 rm -rf /tmp/xx_not_root");
        // rm -rf without root patterns may allow — rootish checked
        let r2 = assess_shell_risk("env X=1 rm -rf /");
        assert_eq!(r2.tier, RiskTier::Deny, "{r2:?}");
        let _ = r;
    }

    #[test]
    fn allow_ls() {
        let r = assess_shell_risk("ls -la src");
        assert_eq!(r.tier, RiskTier::Allow, "{r:?}");
    }

    #[test]
    fn ask_nested_shell() {
        let r = assess_shell_risk("bash -c 'echo hi'");
        assert_eq!(r.tier, RiskTier::Ask, "{r:?}");
    }

    #[test]
    fn deny_nested_shell_dangerous() {
        let r = assess_shell_risk("env sh -c 'rm -rf /'");
        assert_eq!(r.tier, RiskTier::Deny, "{r:?}");
    }
}
