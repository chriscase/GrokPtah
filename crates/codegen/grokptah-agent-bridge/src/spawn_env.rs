//! Scrub control-plane secrets from every child process environment (#196).

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

/// Environment variable names that must never reach agent-run children.
pub const CONTROL_SECRET_ENV_KEYS: &[&str] = &[
    "GROKPTAH_CONTROL_TOKEN",
    "GROKPTAH_CONTROL_PORT",
    "GROKPTAH_CONTROL_WORKSPACES",
    "GROKPTAH_CONTROL_SECRET",
    "GROKPTAH_MCP_CONTROL_TOKEN",
];

fn is_scrubbed_key(key: &OsStr) -> bool {
    let Some(s) = key.to_str() else {
        return false;
    };
    let upper = s.to_ascii_uppercase();
    CONTROL_SECRET_ENV_KEYS
        .iter()
        .any(|k| upper == *k || upper.starts_with("GROKPTAH_CONTROL_"))
}

/// Apply scrub list to a tokio `Command` (clears then rebuilds without secrets).
pub fn scrub_tokio_command(cmd: &mut TokioCommand) {
    // Start from current env and drop control secrets.
    let mut keys: HashSet<OsString> = std::env::vars_os().map(|(k, _)| k).collect();
    for k in CONTROL_SECRET_ENV_KEYS {
        keys.insert(OsString::from(*k));
    }
    // Also drop any GROKPTAH_CONTROL_* present now or later via prefix scan.
    for (k, _) in std::env::vars_os() {
        if is_scrubbed_key(&k) {
            cmd.env_remove(k);
        }
    }
    for k in CONTROL_SECRET_ENV_KEYS {
        cmd.env_remove(k);
    }
}

/// Apply scrub list to a std `Command`.
pub fn scrub_std_command(cmd: &mut StdCommand) {
    for (k, _) in std::env::vars_os() {
        if is_scrubbed_key(&k) {
            cmd.env_remove(k);
        }
    }
    for k in CONTROL_SECRET_ENV_KEYS {
        cmd.env_remove(k);
    }
}

/// Snapshot of current process env with control secrets removed (for tests).
#[allow(dead_code)]
pub fn scrubbed_env_pairs() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| !is_scrubbed_key(OsStr::new(k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_keys_are_recognized() {
        assert!(is_scrubbed_key(OsStr::new("GROKPTAH_CONTROL_TOKEN")));
        assert!(is_scrubbed_key(OsStr::new("grokptah_control_port")));
        assert!(is_scrubbed_key(OsStr::new("GROKPTAH_CONTROL_EXTRA")));
        assert!(!is_scrubbed_key(OsStr::new("PATH")));
        assert!(!is_scrubbed_key(OsStr::new("GROKPTAH_AGENT_OFFLINE")));
    }
}
