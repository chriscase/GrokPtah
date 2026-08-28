//! Atomic durable writes with owner/mode/link defenses.
//!
//! Writes are old-or-new: a crash during write/fsync/rename/dir-fsync never
//! publishes a partial file. Callers must treat a failed write as "previous
//! bytes still authoritative."

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use uuid::Uuid;

#[cfg(test)]
use std::cell::Cell;

use super::orchestration::{OrchError, OrchErrorCode};

const MIGRATION_LABEL: &str = "canonical-authority.v1";

#[cfg(test)]
thread_local! {
    static FAULT: Cell<u8> = const { Cell::new(0) };
}

#[cfg(test)]
pub fn inject_fault(point: Option<&str>) {
    let value = match point {
        None => 0,
        Some("write") => 1,
        Some("file_sync") => 2,
        Some("rename") => 3,
        Some("dir_sync") => 4,
        Some(other) => panic!("unknown durable_fs fault point: {other}"),
    };
    FAULT.with(|fault| fault.set(value));
}

fn fault(point: &str) -> Result<(), OrchError> {
    #[cfg(test)]
    {
        let value = match point {
            "write" => 1,
            "file_sync" => 2,
            "rename" => 3,
            "dir_sync" => 4,
            _ => 0,
        };
        if value != 0
            && FAULT.with(|fault| {
                if fault.get() == value {
                    fault.set(0);
                    true
                } else {
                    false
                }
            })
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                format!("injected durable write failure at {point}"),
            ));
        }
    }
    let _ = point;
    Ok(())
}

pub fn migration_label() -> &'static str {
    MIGRATION_LABEL
}

pub fn ensure_secure_dir(path: &Path) -> Result<(), OrchError> {
    ensure_dir(path, true)
}

/// Durable record directories may start at umask 0o755. If we own them, tighten
/// to 0o700. Symlinks, foreign owners, and remaining group/other bits fail closed.
pub fn ensure_durable_dir(path: &Path) -> Result<(), OrchError> {
    ensure_dir(path, false)
}

fn ensure_dir(path: &Path, strict: bool) -> Result<(), OrchError> {
    if path.exists() {
        let meta = fs::symlink_metadata(path).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("inspect {}: {error}", path.display()),
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "authority directory must not be a symlink",
            ));
        }
        if !meta.is_dir() {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "authority path exists and is not a directory",
            ));
        }
        if !strict {
            restrict_mode(path, true)?;
        }
        let meta = fs::symlink_metadata(path).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("inspect {}: {error}", path.display()),
            )
        })?;
        reject_insecure_metadata(path, &meta)?;
    } else {
        fs::create_dir_all(path).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("create {}: {error}", path.display()),
            )
        })?;
        restrict_mode(path, true)?;
        reject_insecure_path(path)?;
    }
    Ok(())
}

pub fn reject_insecure_path(path: &Path) -> Result<(), OrchError> {
    let meta = fs::symlink_metadata(path).map_err(|error| {
        OrchError::new(
            OrchErrorCode::Internal,
            format!("inspect {}: {error}", path.display()),
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "authority file must not be a symlink",
        ));
    }
    reject_insecure_metadata(path, &meta)
}

fn reject_insecure_metadata(path: &Path, meta: &fs::Metadata) -> Result<(), OrchError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        if meta.uid() != uid {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                format!("{} is not owned by the host process", path.display()),
            ));
        }
        let mode = meta.mode() & 0o777;
        let allowed = if meta.is_dir() { 0o700 } else { 0o600 };
        if mode & 0o077 != 0 || mode != allowed && mode & 0o022 != 0 {
            // Group/other bits are never acceptable. Owner-write-only files
            // created before chmod are still rejected if other/group can write.
            if mode & 0o077 != 0 {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    format!("{} mode {:o} is not private", path.display(), mode),
                ));
            }
        }
        if meta.file_type().is_file() && meta.nlink() != 1 {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                format!("{} must not be a hard link", path.display()),
            ));
        }
        let _ = allowed;
    }
    let _ = path;
    let _ = meta;
    Ok(())
}

pub fn restrict_mode(path: &Path, dir: bool) -> Result<(), OrchError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if dir { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("chmod {}: {error}", path.display()),
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        let _ = dir;
    }
    Ok(())
}

pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), OrchError> {
    atomic_write_bytes_in(path, bytes, true)
}

fn atomic_write_bytes_in(path: &Path, bytes: &[u8], strict_parent: bool) -> Result<(), OrchError> {
    if let Some(parent) = path.parent() {
        if strict_parent {
            ensure_secure_dir(parent)?;
        } else {
            ensure_durable_dir(parent)?;
        }
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("auth"),
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        fault("write")?;
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)
                .map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("open {}: {error}", tmp.display()),
                    )
                })?;
            file.write_all(bytes).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("write {}: {error}", tmp.display()),
                )
            })?;
            fault("file_sync")?;
            file.sync_all().map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("fsync {}: {error}", tmp.display()),
                )
            })?;
        }
        restrict_mode(&tmp, false)?;
        if let Some(parent) = path.parent() {
            let dir = File::open(parent).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("open {}: {error}", parent.display()),
                )
            })?;
            fault("dir_sync")?;
            dir.sync_all().map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("dir fsync {}: {error}", parent.display()),
                )
            })?;
        }
        fault("rename")?;
        fs::rename(&tmp, path).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("rename {}: {error}", path.display()),
            )
        })?;
        restrict_mode(path, false)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), OrchError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        OrchError::new(
            OrchErrorCode::Internal,
            format!("serialize {}: {error}", path.display()),
        )
    })?;
    atomic_write_bytes_in(path, &bytes, false)
}

pub fn quarantine(path: &Path, reason: &str) -> Result<PathBuf, OrchError> {
    let parent = path
        .parent()
        .ok_or_else(|| OrchError::new(OrchErrorCode::Internal, "quarantine path has no parent"))?;
    let dest_dir = parent.join("quarantine");
    ensure_secure_dir(&dest_dir)?;
    let name = format!(
        "{}-{}-{reason}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
        Uuid::new_v4().simple()
    );
    let dest = dest_dir.join(sanitize_name(&name));
    fs::rename(path, &dest).map_err(|error| {
        OrchError::new(
            OrchErrorCode::Internal,
            format!("quarantine {}: {error}", path.display()),
        )
    })?;
    Ok(dest)
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn interrupted_writes_keep_previous_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("secure");
        ensure_secure_dir(&root).unwrap();
        let path = root.join("record.json");
        atomic_write_bytes(&path, br#"{"v":1}"#).unwrap();
        let original = std::fs::read(&path).unwrap();
        for point in ["write", "file_sync", "rename", "dir_sync"] {
            inject_fault(Some(point));
            let err = atomic_write_bytes(&path, br#"{"v":2}"#).unwrap_err();
            assert_eq!(err.code, OrchErrorCode::Internal);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        atomic_write_bytes(&path, br#"{"v":2}"#).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"v":2}"#);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_world_writable_and_hardlink_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("secure");
        ensure_secure_dir(&root).unwrap();
        let file = root.join("authority.json");
        atomic_write_bytes(&file, b"{}").unwrap();

        let link = root.join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(
            reject_insecure_path(&link).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );

        let hard = root.join("hard.json");
        std::fs::hard_link(&file, &hard).unwrap();
        assert_eq!(
            reject_insecure_path(&hard).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );
        assert_eq!(
            reject_insecure_path(&file).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );

        let loose = root.join("loose.json");
        {
            let mut f = std::fs::File::create(&loose).unwrap();
            f.write_all(b"x").unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            reject_insecure_path(&loose).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );

        let linked_dir = root.join("as-dir");
        std::os::unix::fs::symlink(&root, &linked_dir).unwrap();
        assert_eq!(
            ensure_secure_dir(&linked_dir).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );
    }
}
