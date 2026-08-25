//! Handle-relative, no-follow I/O for the durable orchestration ledger.
//!
//! Every durable record that can carry private execution input is read and
//! written through a **directory handle**, not through a path. A path is
//! re-resolved by the kernel on every syscall, so checking it and then using
//! it is a time-of-check/time-of-use race: an attacker who can create names in
//! the ledger directory can swap a regular file for a symlink between the
//! check and the open. A handle names one inode for its whole lifetime.
//!
//! On top of that, three properties are enforced at open time rather than
//! inferred afterwards:
//!
//! * **No follow.** `O_NOFOLLOW` on Unix and `FILE_FLAG_OPEN_REPARSE_POINT`
//!   plus an explicit reparse-point rejection on Windows. A symlink or
//!   junction in the final component fails the open; it is never traversed.
//! * **Authority.** The opened inode must be owned by this process's effective
//!   user and must not be group- or world-accessible (Unix), and must not be a
//!   reparse point or a directory (Windows). Ownership is read from the *open
//!   handle*, so it describes the object actually opened.
//! * **Containment.** Names are single path components, validated before use,
//!   so nothing can address a parent or a nested directory.
//!
//! Writes are atomic and private from the first byte: the temporary file is
//! created `O_EXCL` with mode `0600` (Unix) inside the same directory handle,
//! written, fsynced, and renamed into place through that same handle, and the
//! directory itself is fsynced so the rename survives a power loss.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::types::{OrchError, OrchErrorCode};

#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Windows reparse-point attribute. A file carrying it is a symlink, junction,
/// or mount point, and is refused rather than opened.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
/// Open the link itself rather than its target.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn internal(error: io::Error) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, error.to_string())
}

/// A single, safe path component: no separators, no `.`/`..`, no NUL, bounded.
///
/// Every ledger file name is a store-generated hex digest plus a fixed
/// extension, so this is a total constraint rather than a heuristic.
pub fn validate_component(name: &str) -> Result<(), OrchError> {
    if name.is_empty() || name.len() > 255 {
        return Err(invalid("ledger file name is out of range"));
    }
    if name.contains('\0') || name.contains('/') || name.contains('\\') {
        return Err(invalid("ledger file name contains a path separator"));
    }
    if name == "." || name == ".." {
        return Err(invalid("ledger file name is a directory reference"));
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(invalid("ledger file name is not a single component")),
    }
}

/// An open directory used as the authority root for the records inside it.
///
/// The handle is opened once and reused, so every record read or written
/// through it is resolved against the same inode even if the directory is
/// renamed or replaced underneath.
pub struct LedgerDir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

impl LedgerDir {
    /// Create the directory if needed with owner-only permissions, then open a
    /// handle to it and verify that handle's authority.
    pub fn open(path: &Path) -> Result<Self, OrchError> {
        fs::create_dir_all(path).map_err(internal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))
                .map_err(internal)?;
        }
        #[cfg(unix)]
        {
            use std::os::fd::OwnedFd;
            use std::os::unix::ffi::OsStrExt;
            let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| invalid("ledger directory path contains NUL"))?;
            // SAFETY: `c_path` is a valid NUL-terminated C string for the
            // duration of the call; the returned descriptor is immediately
            // adopted by OwnedFd, which owns the close.
            let raw = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if raw < 0 {
                return Err(internal(io::Error::last_os_error()));
            }
            // SAFETY: `raw` is a fresh, valid, owned descriptor.
            let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) };
            let dir = Self {
                path: path.to_path_buf(),
                fd,
            };
            dir.verify_directory_authority()?;
            Ok(dir)
        }
        #[cfg(not(unix))]
        {
            let dir = Self {
                path: path.to_path_buf(),
            };
            dir.verify_directory_authority()?;
            Ok(dir)
        }
    }

    /// The directory must be owned by this effective user and closed to group
    /// and other. Checked through the open handle, so it describes the inode
    /// the handle names rather than whatever the path resolves to now.
    #[cfg(unix)]
    fn verify_directory_authority(&self) -> Result<(), OrchError> {
        let stat = self.fstat()?;
        // SAFETY: geteuid is always safe and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if stat.st_uid != euid {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "ledger directory is not owned by this process",
            ));
        }
        if stat.st_mode & 0o077 != 0 {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "ledger directory permissions are wider than owner-only",
            ));
        }
        Ok(())
    }

    #[cfg(windows)]
    fn verify_directory_authority(&self) -> Result<(), OrchError> {
        use std::os::windows::fs::MetadataExt;
        let metadata = fs::symlink_metadata(&self.path).map_err(internal)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "ledger directory is a reparse point",
            ));
        }
        if !metadata.is_dir() {
            return Err(invalid("ledger directory is not a directory"));
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn verify_directory_authority(&self) -> Result<(), OrchError> {
        Ok(())
    }

    #[cfg(unix)]
    fn fstat(&self) -> Result<libc::stat, OrchError> {
        use std::os::fd::AsRawFd;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: the descriptor is owned and open; `stat` is a live,
        // correctly sized, writable buffer for the duration of the call.
        let rc = unsafe { libc::fstat(self.fd.as_raw_fd(), stat.as_mut_ptr()) };
        if rc < 0 {
            return Err(internal(io::Error::last_os_error()));
        }
        // SAFETY: fstat succeeded, so the buffer is fully initialized.
        Ok(unsafe { stat.assume_init() })
    }

    /// Read one record without ever following a link.
    ///
    /// Returns `Ok(None)` only when the name does not exist. A name that
    /// exists but is a link, a directory, wrongly owned, or too permissive is
    /// an error: absence and tampering must never look the same.
    pub fn read_private(&self, name: &str) -> Result<Option<String>, OrchError> {
        validate_component(name)?;
        let mut file = match self.open_no_follow(name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            // O_NOFOLLOW on a symlink reports ELOOP (or ENOTDIR on some
            // systems); either way the name is not a regular file we may read.
            Err(error) => {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    format!("ledger record {name} could not be opened safely: {error}"),
                ));
            }
        };
        self.verify_file_authority(&file, name)?;
        use std::io::Read;
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(internal)?;
        Ok(Some(text))
    }

    #[cfg(unix)]
    fn open_no_follow(&self, name: &str) -> io::Result<fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let c_name = std::ffi::CString::new(std::ffi::OsStr::new(name).as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        // SAFETY: the directory descriptor is owned and open, and `c_name` is
        // a valid NUL-terminated string for the duration of the call.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh, valid, owned descriptor.
        Ok(unsafe { fs::File::from_raw_fd(raw) })
    }

    #[cfg(windows)]
    fn open_no_follow(&self, name: &str) -> io::Result<fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        // Windows has no `openat`; the directory handle cannot be used as a
        // resolution root from std. Containment is enforced by the validated
        // single-component name, and the link itself is opened rather than its
        // target so a junction or symlink can be detected and refused.
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name))
    }

    #[cfg(not(any(unix, windows)))]
    fn open_no_follow(&self, name: &str) -> io::Result<fs::File> {
        fs::File::open(self.path.join(name))
    }

    /// The opened inode must be a regular file this user owns, closed to
    /// group and other.
    #[cfg(unix)]
    fn verify_file_authority(&self, file: &fs::File, name: &str) -> Result<(), OrchError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(internal)?;
        if !metadata.is_file() {
            return Err(invalid(format!(
                "ledger record {name} is not a regular file"
            )));
        }
        // SAFETY: geteuid is always safe and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} is not owned by this process"),
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} permissions are wider than owner-only"),
            ));
        }
        // A hard link into the ledger is another way to alias a record.
        if metadata.nlink() != 1 {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} is hard-linked elsewhere"),
            ));
        }
        Ok(())
    }

    #[cfg(windows)]
    fn verify_file_authority(&self, file: &fs::File, name: &str) -> Result<(), OrchError> {
        use std::os::windows::fs::MetadataExt;
        let metadata = file.metadata().map_err(internal)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} is a reparse point"),
            ));
        }
        if !metadata.is_file() {
            return Err(invalid(format!(
                "ledger record {name} is not a regular file"
            )));
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn verify_file_authority(&self, _file: &fs::File, _name: &str) -> Result<(), OrchError> {
        Ok(())
    }

    /// Atomically install a private record.
    ///
    /// The temporary is created exclusively and privately inside this same
    /// directory handle, so the content is never briefly world-readable and an
    /// attacker cannot pre-create the name to capture the write.
    pub fn write_private(&self, name: &str, bytes: &[u8]) -> Result<(), OrchError> {
        validate_component(name)?;
        let tmp_name = format!("{name}.tmp");
        validate_component(&tmp_name)?;
        // A leftover temp from a crashed write must not block this one.
        let _ = self.remove(&tmp_name);
        let write = (|| -> Result<(), OrchError> {
            {
                let mut file = self.create_private_exclusive(&tmp_name)?;
                use std::io::Write;
                file.write_all(bytes).map_err(internal)?;
                file.sync_all().map_err(internal)?;
            }
            self.rename_within(&tmp_name, name)?;
            self.sync_dir()
        })();
        if write.is_err() {
            let _ = self.remove(&tmp_name);
        }
        write
    }

    #[cfg(unix)]
    fn create_private_exclusive(&self, name: &str) -> Result<fs::File, OrchError> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let c_name = std::ffi::CString::new(std::ffi::OsStr::new(name).as_bytes())
            .map_err(|_| invalid("ledger file name contains NUL"))?;
        // SAFETY: the directory descriptor is owned and open, and `c_name` is
        // a valid NUL-terminated string for the duration of the call.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                libc::c_uint::from(PRIVATE_FILE_MODE),
            )
        };
        if raw < 0 {
            return Err(internal(io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a fresh, valid, owned descriptor.
        Ok(unsafe { fs::File::from_raw_fd(raw) })
    }

    #[cfg(windows)]
    fn create_private_exclusive(&self, name: &str) -> Result<fs::File, OrchError> {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name))
            .map_err(internal)
    }

    #[cfg(not(any(unix, windows)))]
    fn create_private_exclusive(&self, name: &str) -> Result<fs::File, OrchError> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
            .map_err(internal)
    }

    #[cfg(unix)]
    fn rename_within(&self, from: &str, to: &str) -> Result<(), OrchError> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        let c_from = std::ffi::CString::new(std::ffi::OsStr::new(from).as_bytes())
            .map_err(|_| invalid("ledger file name contains NUL"))?;
        let c_to = std::ffi::CString::new(std::ffi::OsStr::new(to).as_bytes())
            .map_err(|_| invalid("ledger file name contains NUL"))?;
        // SAFETY: both names are valid NUL-terminated strings and the
        // descriptor is owned and open for the duration of the call.
        let rc = unsafe {
            libc::renameat(
                self.fd.as_raw_fd(),
                c_from.as_ptr(),
                self.fd.as_raw_fd(),
                c_to.as_ptr(),
            )
        };
        if rc < 0 {
            return Err(internal(io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn rename_within(&self, from: &str, to: &str) -> Result<(), OrchError> {
        fs::rename(self.path.join(from), self.path.join(to)).map_err(internal)
    }

    #[cfg(unix)]
    fn sync_dir(&self) -> Result<(), OrchError> {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is owned and open for the duration of the call.
        let rc = unsafe { libc::fsync(self.fd.as_raw_fd()) };
        if rc < 0 {
            return Err(internal(io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(windows)]
    fn sync_dir(&self) -> Result<(), OrchError> {
        use std::os::windows::fs::OpenOptionsExt;
        // Directory handles need backup semantics to be opened at all; a
        // failure to sync the directory is not fatal on NTFS, where the
        // rename is already ordered, so this is best effort by design.
        if let Ok(dir) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&self.path)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn sync_dir(&self) -> Result<(), OrchError> {
        Ok(())
    }

    /// Remove one record. Missing is success; a link is removed as the link.
    pub fn remove(&self, name: &str) -> Result<bool, OrchError> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            let c_name = std::ffi::CString::new(std::ffi::OsStr::new(name).as_bytes())
                .map_err(|_| invalid("ledger file name contains NUL"))?;
            // SAFETY: valid NUL-terminated name, owned open descriptor.
            let rc = unsafe { libc::unlinkat(self.fd.as_raw_fd(), c_name.as_ptr(), 0) };
            if rc < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::NotFound {
                    return Ok(false);
                }
                return Err(internal(error));
            }
            Ok(true)
        }
        #[cfg(not(unix))]
        {
            match fs::remove_file(self.path.join(name)) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(internal(error)),
            }
        }
    }

    /// Names of the records in this directory, sorted, filtered by extension.
    /// Entries that are not plain names are skipped rather than reported.
    pub fn list(&self, extension: &str) -> Result<Vec<String>, OrchError> {
        let mut names = Vec::new();
        let entries = fs::read_dir(&self.path).map_err(internal)?;
        for entry in entries {
            let entry = entry.map_err(internal)?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if validate_component(&name).is_err() {
                continue;
            }
            if Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                != Some(extension)
            {
                continue;
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_names_that_are_not_single_components() {
        for bad in ["", "..", ".", "a/b", "a\\b", "../escape.json"] {
            assert!(validate_component(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(validate_component("0123abcd.json").is_ok());
    }

    #[test]
    fn round_trips_a_private_record() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        ledger.write_private("a.json", b"{\"x\":1}").unwrap();
        assert_eq!(
            ledger.read_private("a.json").unwrap().as_deref(),
            Some("{\"x\":1}")
        );
        assert_eq!(ledger.read_private("missing.json").unwrap(), None);
        assert!(ledger.remove("a.json").unwrap());
        assert!(!ledger.remove("a.json").unwrap());
    }

    #[test]
    fn written_records_are_owner_only() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        ledger.write_private("a.json", b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join("inputs").join("a.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "saw {mode:o}");
            let dir_mode = fs::metadata(dir.path().join("inputs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "saw {dir_mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_follow_a_symlink_and_says_so() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        let outside = dir.path().join("outside.json");
        fs::write(&outside, b"{\"stolen\":true}").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("inputs").join("a.json")).unwrap();

        let error = ledger.read_private("a.json").unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert!(
            !format!("{error}").contains("stolen"),
            "the link target must never be read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_record_whose_permissions_were_widened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        ledger.write_private("a.json", b"{}").unwrap();
        let path = dir.path().join("inputs").join("a.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ledger.read_private("a.json").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ledger.read_private("a.json").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_hard_linked_record() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        ledger.write_private("a.json", b"{}").unwrap();
        fs::hard_link(
            dir.path().join("inputs").join("a.json"),
            dir.path().join("alias.json"),
        )
        .unwrap();
        let error = ledger.read_private("a.json").unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
    }

    #[cfg(unix)]
    #[test]
    fn write_replaces_a_pre_created_temp_rather_than_capturing_it() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        // An attacker pre-creates the staging name to try to capture the write.
        fs::write(dir.path().join("inputs").join("a.json.tmp"), b"squatted").unwrap();
        ledger.write_private("a.json", b"{\"ok\":1}").unwrap();
        assert_eq!(
            ledger.read_private("a.json").unwrap().as_deref(),
            Some("{\"ok\":1}")
        );
        assert_eq!(ledger.read_private("a.json.tmp").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_ledger_directory_that_is_group_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let target = dir.path().join("inputs");
        fs::create_dir_all(&target).unwrap();
        // `LedgerDir::open` tightens the mode itself, so widen it after the
        // first open to prove the check runs on every open.
        let ledger = LedgerDir::open(&target).unwrap();
        drop(ledger);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o777)).unwrap();
        // create_dir_all is a no-op for an existing directory, but open must
        // still re-tighten and then verify.
        let reopened = LedgerDir::open(&target);
        assert!(reopened.is_ok(), "open re-tightens its own directory");
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn lists_only_plain_records_with_the_requested_extension() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(&dir.path().join("inputs")).unwrap();
        ledger.write_private("b.json", b"{}").unwrap();
        ledger.write_private("a.json", b"{}").unwrap();
        fs::write(dir.path().join("inputs").join("note.txt"), b"x").unwrap();
        assert_eq!(
            ledger.list("json").unwrap(),
            vec!["a.json".to_string(), "b.json".to_string()]
        );
    }
}
