//! Handle-relative opening with no-follow semantics.
//!
//! The containment question is not "does this path string look contained?" but
//! "is the handle I now hold contained?". Those differ whenever anything can
//! change the filesystem between the two, which on a machine running an agent
//! is always.
//!
//! On Unix the walk is genuinely handle-relative: the root directory is opened
//! once and every component is resolved with `openat` under `O_NOFOLLOW`, so a
//! symlink swapped in mid-walk is refused by the kernel rather than raced past.
//! No path string is ever re-resolved.
//!
//! On Windows the standard library exposes no handle-relative open, so the
//! walk opens each component by the path built so far with
//! `FILE_FLAG_OPEN_REPARSE_POINT` and refuses any reparse point. That is
//! check-then-use and is documented as such: it stops a link that is present,
//! not one introduced between two consecutive opens.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::SourceViewError;
use crate::identity::NodeIdentity;
use crate::path::ContainedPath;

/// A file opened inside a verified root.
#[derive(Debug)]
pub struct OpenedDocument {
    pub file: File,
    /// Identity observed through the handle at open time.
    pub identity: NodeIdentity,
}

impl OpenedDocument {
    /// Re-observe identity through the same handle.
    ///
    /// Called after a read completes: if the file was replaced or truncated
    /// underneath the read, the projection describes neither version and is
    /// refused rather than returned.
    pub fn validate_unchanged(&self) -> Result<(), SourceViewError> {
        let now = self
            .file
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        if NodeIdentity::from_metadata(&now).unchanged(&self.identity) {
            Ok(())
        } else {
            Err(SourceViewError::DocumentChanged)
        }
    }
}

/// A root directory held open for the lifetime of its authorization.
///
/// Holding the directory open is what makes a swap unwinnable. Reads are
/// resolved from this handle, not from the path, so replacing the directory at
/// that path cannot redirect them — and because a removed directory drops to
/// zero links, the replacement is *detected* rather than merely bypassed. An
/// identity comparison alone would not do this: a filesystem is free to reuse
/// the inode of a directory that was just removed, and in practice does.
#[derive(Debug, Clone)]
pub struct RootHandle {
    path: PathBuf,
    identity: NodeIdentity,
    handle: Arc<File>,
}

impl RootHandle {
    /// Open a root and record its identity. Used once, at authorization.
    pub fn open(path: &Path) -> Result<Self, SourceViewError> {
        let handle = open_directory_nofollow(path)?;
        let metadata = handle
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        if !metadata.is_dir() {
            return Err(SourceViewError::RootUnavailable);
        }
        Ok(Self {
            path: path.to_path_buf(),
            identity: NodeIdentity::from_metadata(&metadata),
            handle: Arc::new(handle),
        })
    }

    /// The canonical path this handle was opened from. Never sent over a
    /// boundary; used for digests and for the platforms that cannot resolve
    /// relative to a handle.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    pub(crate) fn dir(&self) -> &File {
        &self.handle
    }

    /// Confirm the held directory is still a live, unchanged directory.
    ///
    /// Called at action time, before every read.
    pub fn verify(&self) -> Result<(), SourceViewError> {
        let metadata = self
            .handle
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        if !metadata.is_dir() {
            return Err(SourceViewError::RootIdentityChanged);
        }
        if !NodeIdentity::from_metadata(&metadata).same_node(&self.identity) {
            return Err(SourceViewError::RootIdentityChanged);
        }
        if is_unlinked(&metadata) {
            // The directory was removed. Anything now at its path is a
            // different tree, whatever inode it happens to have been given.
            return Err(SourceViewError::RootIdentityChanged);
        }
        Ok(())
    }
}

/// Whether a directory has been removed while this handle held it open.
///
/// Unix reports the link count exactly. Windows exposes no stable equivalent
/// through the standard library, so this returns false there and detection
/// falls back to the identity comparison above — documented as weaker.
#[cfg(unix)]
fn is_unlinked(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() == 0
}

#[cfg(not(unix))]
fn is_unlinked(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Observe a root's identity for the first time, at authorization.
pub fn observe_root(root: &Path) -> Result<NodeIdentity, SourceViewError> {
    Ok(*RootHandle::open(root)?.identity())
}

// ---------------------------------------------------------------- unix

#[cfg(unix)]
fn open_directory_nofollow(path: &Path) -> Result<File, SourceViewError> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| SourceViewError::RootUnavailable)
}

#[cfg(unix)]
pub fn open_contained(
    root: &RootHandle,
    relative: &ContainedPath,
) -> Result<OpenedDocument, SourceViewError> {
    use rustix::fs::{Mode, OFlags};

    root.verify()?;
    let segments = relative.segments();
    let last = segments.len() - 1;
    // Every component is resolved from the held root directory. No path string
    // is re-resolved, so there is no window in which one could be swapped.
    let mut dir: File = root
        .dir()
        .try_clone()
        .map_err(|error| SourceViewError::io(&error))?;

    for (index, segment) in segments.iter().enumerate() {
        let at = |count: usize| relative.prefix_display(count + 1);
        if index < last {
            let opened = rustix::fs::openat(
                &dir,
                segment.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|errno| classify_failure(&dir, segment, errno, at(index)))?;
            dir = File::from(opened);
            continue;
        }

        let opened = rustix::fs::openat(
            &dir,
            segment.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| classify_failure(&dir, segment, errno, at(index)))?;
        let file = File::from(opened);
        let metadata = file
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        if !metadata.is_file() {
            return Err(SourceViewError::NotAFile { segment: at(index) });
        }
        return Ok(OpenedDocument {
            identity: NodeIdentity::from_metadata(&metadata),
            file,
        });
    }

    // `ContainedPath` is non-empty by construction, so the loop always returns.
    Err(SourceViewError::EmptyPath)
}

/// Turn a failed `openat` into the refusal that describes what was actually
/// there.
///
/// `O_NOFOLLOW` reports a symlink as `ELOOP` for a plain open but as `ENOTDIR`
/// when `O_DIRECTORY` is also set, and `ENOTDIR` is equally what a plain file
/// in a directory position produces. The open has already failed closed, so a
/// no-follow `statat` is safe here and is used only to pick the right code —
/// never to decide whether to proceed.
#[cfg(unix)]
fn classify_failure(
    dir: &File,
    segment: &str,
    errno: rustix::io::Errno,
    at: String,
) -> SourceViewError {
    use rustix::fs::{AtFlags, FileType};
    use rustix::io::Errno;
    match errno {
        Errno::LOOP => SourceViewError::SymlinkRejected { segment: at },
        Errno::NOTDIR => {
            let is_link = rustix::fs::statat(dir, segment, AtFlags::SYMLINK_NOFOLLOW)
                .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Symlink)
                .unwrap_or(false);
            if is_link {
                SourceViewError::SymlinkRejected { segment: at }
            } else {
                SourceViewError::NotAFile { segment: at }
            }
        }
        Errno::NOENT => SourceViewError::NotFound { segment: at },
        Errno::NAMETOOLONG => SourceViewError::InvalidComponent { segment: at },
        Errno::ACCESS | Errno::PERM => SourceViewError::Io {
            detail: "permission_denied".into(),
        },
        _ => SourceViewError::Io {
            detail: "unavailable".into(),
        },
    }
}

// ------------------------------------------------------------- windows

#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

#[cfg(windows)]
fn open_directory_nofollow(path: &Path) -> Result<File, SourceViewError> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| SourceViewError::RootUnavailable)
}

#[cfg(windows)]
pub fn open_contained(
    root: &RootHandle,
    relative: &ContainedPath,
) -> Result<OpenedDocument, SourceViewError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    root.verify()?;
    let segments = relative.segments();
    let last = segments.len() - 1;
    let mut walked = root.path().to_path_buf();

    for (index, segment) in segments.iter().enumerate() {
        walked.push(segment);
        let at = relative.prefix_display(index + 1);
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&walked)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => SourceViewError::NotFound {
                    segment: at.clone(),
                },
                _ => SourceViewError::io(&error),
            })?;
        let metadata = handle
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SourceViewError::ReparsePointRejected { segment: at });
        }
        if index < last {
            if !metadata.is_dir() {
                return Err(SourceViewError::NotAFile { segment: at });
            }
            continue;
        }
        if !metadata.is_file() {
            return Err(SourceViewError::NotAFile { segment: at });
        }
        return Ok(OpenedDocument {
            identity: NodeIdentity::from_metadata(&metadata),
            file: handle,
        });
    }

    Err(SourceViewError::EmptyPath)
}

// ------------------------------------------------------- other targets

#[cfg(not(any(unix, windows)))]
fn open_directory_nofollow(path: &Path) -> Result<File, SourceViewError> {
    File::open(path).map_err(|_| SourceViewError::RootUnavailable)
}

#[cfg(not(any(unix, windows)))]
pub fn open_contained(
    root: &RootHandle,
    relative: &ContainedPath,
) -> Result<OpenedDocument, SourceViewError> {
    root.verify()?;
    let mut walked = root.path().to_path_buf();
    let segments = relative.segments();
    let last = segments.len() - 1;
    for (index, segment) in segments.iter().enumerate() {
        walked.push(segment);
        let at = relative.prefix_display(index + 1);
        let meta = std::fs::symlink_metadata(&walked).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => SourceViewError::NotFound {
                segment: at.clone(),
            },
            _ => SourceViewError::io(&error),
        })?;
        if meta.file_type().is_symlink() {
            return Err(SourceViewError::SymlinkRejected { segment: at });
        }
        if index < last {
            if !meta.is_dir() {
                return Err(SourceViewError::NotAFile { segment: at });
            }
            continue;
        }
        if !meta.is_file() {
            return Err(SourceViewError::NotAFile { segment: at });
        }
        let file = File::open(&walked).map_err(|error| SourceViewError::io(&error))?;
        let metadata = file
            .metadata()
            .map_err(|error| SourceViewError::io(&error))?;
        return Ok(OpenedDocument {
            identity: NodeIdentity::from_metadata(&metadata),
            file,
        });
    }
    Err(SourceViewError::EmptyPath)
}
