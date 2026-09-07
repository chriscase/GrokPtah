//! Main-checkout digest/mtime fence for host sentinel baselines.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::error::{HarnessError, HarnessResult};
use crate::sentinel::MainCheckoutFence;

const HEAD_RELATIVE: &str = ".git/HEAD";

/// Collect a digest/mtime fence for the main checkout (not a disposable worktree).
pub fn collect_main_checkout_fence(
    checkout_path: impl AsRef<Path>,
) -> HarnessResult<MainCheckoutFence> {
    let checkout_path = checkout_path.as_ref();
    let checkout_path_str = checkout_path.to_string_lossy().into_owned();
    if !checkout_path.exists() {
        return Err(HarnessError::backend_unavailable(format!(
            "main checkout path does not exist: {checkout_path_str}"
        )));
    }

    let metadata = fs::metadata(checkout_path).map_err(|err| {
        HarnessError::backend_unavailable(format!(
            "main checkout metadata unavailable at {checkout_path_str}: {err}"
        ))
    })?;
    let mtime_fence_ns = metadata.modified().ok().and_then(system_time_to_ns);

    let content_digest = digest_main_checkout(checkout_path)?;

    Ok(MainCheckoutFence {
        checkout_path: checkout_path_str,
        content_digest,
        mtime_fence_ns,
    })
}

fn digest_main_checkout(checkout_path: &Path) -> HarnessResult<String> {
    let head_path = checkout_path.join(HEAD_RELATIVE);
    if head_path.is_file() {
        let mut head = String::new();
        fs::File::open(&head_path)
            .and_then(|mut file| file.read_to_string(&mut head))
            .map_err(|err| {
                HarnessError::backend_unavailable(format!(
                    "main checkout HEAD read failed at {}: {err}",
                    head_path.display()
                ))
            })?;
        return Ok(format!("sha256:{}", hash_bytes(head.trim().as_bytes())));
    }

    let mut paths = Vec::new();
    collect_digest_paths(checkout_path, 2, &mut paths)?;
    paths.sort();
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        let meta = fs::metadata(&path).map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "main checkout digest metadata failed at {}: {err}",
                path.display()
            ))
        })?;
        hasher.update(meta.len().to_le_bytes());
        if let Some(modified) = meta.modified().ok().and_then(system_time_to_ns) {
            hasher.update(modified.to_le_bytes());
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn collect_digest_paths(
    current: &Path,
    depth_remaining: u32,
    out: &mut Vec<PathBuf>,
) -> HarnessResult<()> {
    if depth_remaining == 0 {
        return Ok(());
    }
    let entries = fs::read_dir(current).map_err(|err| {
        HarnessError::backend_unavailable(format!(
            "main checkout digest walk failed at {}: {err}",
            current.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "main checkout digest walk failed at {}: {err}",
                current.display()
            ))
        })?;
        let file_type = entry.file_type().map_err(|err| {
            HarnessError::backend_unavailable(format!(
                "main checkout digest walk failed at {}: {err}",
                entry.path().display()
            ))
        })?;
        let path = entry.path();
        if file_type.is_dir() {
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            collect_digest_paths(&path, depth_remaining - 1, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn system_time_to_ns(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn collect_fence_from_temp_checkout() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("marker.txt"), b"baseline").expect("write");
        let fence = collect_main_checkout_fence(dir.path()).expect("collect");
        assert!(fence.content_digest.starts_with("sha256:"));
        assert_eq!(fence.checkout_path, dir.path().to_string_lossy());
        assert!(fence.mtime_fence_ns.is_some());
    }
}
