//! Private-file and commit-point durability helpers (#443).
//!
//! The commit discipline mirrors `event_bus::atomic_write_bytes`: write a
//! temporary file, `sync_all`, rename onto the target, then fsync the parent
//! directory so the rename itself is durable. `orchestration::store`'s legacy
//! audit rotation skipped that parent fsync, which is one of the reasons a
//! crash there could not be told apart from "never audited".

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{AuditError, AuditResult};

/// Temporary sibling used by [`atomic_write`].
///
/// The suffix is appended to the *whole* file name (`manifest.json.tmp`, not
/// `manifest.tmp`) because readers must be able to recognise the temporary of a
/// specific document and refuse to promote it.
pub(crate) fn tmp_path(path: &Path) -> AuditResult<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AuditError::Io("audit path has no file name".into()))?;
    Ok(path.with_file_name(format!("{name}.tmp")))
}

pub(crate) fn create_private_dir_all(path: &Path) -> AuditResult<()> {
    fs::create_dir_all(path).map_err(|error| AuditError::Io(format!("create dir: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| AuditError::Io(format!("chmod dir: {error}")))?;
    }
    Ok(())
}

pub(crate) fn create_private_dir_new(path: &Path) -> AuditResult<()> {
    fs::create_dir(path).map_err(|error| AuditError::Io(format!("create dir: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| AuditError::Io(format!("chmod dir: {error}")))?;
    }
    Ok(())
}

pub(crate) fn create_private_file_new(path: &Path) -> AuditResult<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| AuditError::Io(format!("create file: {error}")))
}

pub(crate) fn fsync_dir(path: &Path) -> AuditResult<()> {
    #[cfg(unix)]
    {
        let dir = fs::File::open(path)
            .map_err(|error| AuditError::Io(format!("open dir for fsync: {error}")))?;
        dir.sync_all()
            .map_err(|error| AuditError::Io(format!("fsync dir: {error}")))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// tmp -> write -> sync_all -> rename -> fsync parent. The rename is the commit.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> AuditResult<()> {
    let tmp = tmp_path(path)?;
    if tmp.exists() {
        fs::remove_file(&tmp)
            .map_err(|error| AuditError::Io(format!("clear stale tmp: {error}")))?;
    }
    {
        let mut file = create_private_file_new(&tmp)?;
        file.write_all(bytes)
            .map_err(|error| AuditError::Io(format!("write tmp: {error}")))?;
        file.sync_all()
            .map_err(|error| AuditError::Io(format!("sync tmp: {error}")))?;
    }
    fs::rename(&tmp, path).map_err(|error| AuditError::Io(format!("commit rename: {error}")))?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Append one already-terminated line and make both data and length durable.
///
/// `sync_all` rather than `sync_data`: the file length must survive a crash, or
/// a reader cannot tell a short file from a torn tail.
pub(crate) fn append_line(path: &Path, line: &str) -> AuditResult<u64> {
    let mut options = fs::OpenOptions::new();
    options.append(true).create(false);
    let mut file = options
        .open(path)
        .map_err(|error| AuditError::Io(format!("open journal: {error}")))?;
    let bytes = line.as_bytes();
    file.write_all(bytes)
        .map_err(|error| AuditError::Io(format!("append journal: {error}")))?;
    file.sync_all()
        .map_err(|error| AuditError::Io(format!("sync journal: {error}")))?;
    Ok(bytes.len() as u64)
}

pub(crate) fn read_bytes(path: &Path) -> AuditResult<Vec<u8>> {
    fs::read(path).map_err(|error| AuditError::Io(format!("read {}: {error}", path.display())))
}

/// Reject a path whose final component is a symlink.
pub(crate) fn reject_symlink(path: &Path) -> AuditResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AuditError::Io(format!("symlink metadata: {error}")))?;
    if metadata.file_type().is_symlink() {
        return Err(AuditError::Poisoned(super::PoisonReason::SymlinkedPath));
    }
    Ok(())
}

/// Reject a symlink anywhere in an existing path.  Checking only the final
/// component is insufficient for export destinations and generation paths:
/// an attacker can replace an ancestor between validation and open.
pub(crate) fn reject_symlink_components(path: &Path) -> AuditResult<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err(AuditError::Poisoned(super::PoisonReason::SymlinkedPath));
        }
    }
    Ok(())
}
