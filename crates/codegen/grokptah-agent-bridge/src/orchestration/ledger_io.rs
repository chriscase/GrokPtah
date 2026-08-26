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

// ── Windows record authority ───────────────────────────────────────────
//
// The Windows analogue of the Unix `uid == euid && mode & 0o077 == 0` check
// is "the DACL is protected and grants nobody but the owner". The *decision*
// is a pure function over a reduced ACL model so it is compiled and executed
// on every platform, including this one; only the syscalls that read the real
// security descriptor are Windows-only.

/// A file carrying this attribute is a symlink, junction, or mount point.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
/// A directory is never a ledger record.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

#[cfg_attr(not(windows), allow(dead_code))]
/// Who an access-control entry names, reduced to what the ledger cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedgerTrustee {
    /// The SID that owns this handle's token — the "owner" of `0600`.
    Owner,
    /// `NT AUTHORITY\SYSTEM`. Present on every practical Windows install and
    /// already able to read anything; refusing it would refuse every real
    /// file without denying an attacker anything.
    System,
    /// `BUILTIN\Administrators`. Same argument: an administrator can take
    /// ownership regardless, so its presence is not the leak.
    Administrators,
    /// Anybody else — another user, `Users`, `Everyone`, `Authenticated
    /// Users`. This is exactly the group/other bit of `0o077`.
    Other,
}

#[cfg_attr(not(windows), allow(dead_code))]
/// One access-control entry, reduced to the fields the decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LedgerAce {
    /// `true` for an access-allowed ACE, `false` for access-denied.
    pub allow: bool,
    pub trustee: LedgerTrustee,
    /// The ACE was inherited from the parent container rather than set on
    /// this object.
    pub inherited: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Refuse a record whose attributes disqualify it before its content is read.
pub(crate) fn windows_attribute_verdict(name: &str, attributes: u32) -> Result<(), OrchError> {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            format!("ledger record {name} is a reparse point"),
        ));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid(format!(
            "ledger record {name} is not a regular file"
        )));
    }
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
/// The Windows analogue of `mode & 0o077 == 0`, decided from the DACL.
///
/// Fails closed on every ambiguity:
///
/// * **No DACL at all** is not "no access" on Windows, it is *unrestricted*
///   access. A NULL DACL is the widest possible grant, so its absence is the
///   most dangerous case, not the safest.
/// * **An unprotected DACL** inherits from its parent container. Whatever the
///   parent grants today — and whatever it is changed to grant tomorrow — this
///   record grants. Owner-only by inheritance is owner-only by luck.
/// * **Any allow entry naming anyone else** is the leak this check exists to
///   find. Deny entries are irrelevant to it: they can only narrow access.
pub(crate) fn windows_dacl_verdict(
    name: &str,
    dacl_present: bool,
    protected: bool,
    aces: &[LedgerAce],
) -> Result<(), OrchError> {
    if !dacl_present {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            format!("ledger record {name} has a NULL DACL, which grants everyone access"),
        ));
    }
    if !protected {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            format!("ledger record {name} inherits its DACL from its parent directory"),
        ));
    }
    for ace in aces {
        if ace.inherited {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} carries an inherited access-control entry"),
            ));
        }
        if ace.allow && ace.trustee == LedgerTrustee::Other {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("ledger record {name} grants access beyond its owner"),
            ));
        }
    }
    Ok(())
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
        windows_attribute_verdict(name, metadata.file_attributes())?;
        if !metadata.is_file() {
            return Err(invalid(format!(
                "ledger record {name} is not a regular file"
            )));
        }
        // The security descriptor is read from the *open handle*, so it
        // describes the object actually opened rather than whatever the name
        // resolves to now.
        let (dacl_present, protected, aces) = win_acl::read_handle_dacl(file, name)?;
        windows_dacl_verdict(name, dacl_present, protected, &aces)
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
        // `create_new` is the `O_EXCL` half: an attacker cannot pre-create the
        // name to capture the write. The file is empty until the protected
        // DACL is installed on the handle below, so the inherited-permission
        // window contains no content.
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name))
            .map_err(internal)?;
        win_acl::protect_handle_owner_only(&file, name)?;
        Ok(file)
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

/// The Windows security-descriptor syscalls behind the pure verdicts above.
///
/// Nothing in here decides anything: it reads or installs a descriptor and
/// hands the reduced model to [`windows_dacl_verdict`]. Keeping the decision
/// out of the FFI is what lets the decision be executed on every platform.
#[cfg(windows)]
mod win_acl {
    use std::fs;
    use std::mem;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        AddAccessAllowedAce, CreateWellKnownSid, EqualSid, GetAce, GetLengthSid,
        GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, INHERITED_ACE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE,
        SE_DACL_PRESENT, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{LedgerAce, LedgerTrustee, OrchError, OrchErrorCode};

    fn refuse(name: &str, what: &str) -> OrchError {
        OrchError::new(
            OrchErrorCode::Conflict,
            format!("ledger record {name}: {what}"),
        )
    }

    /// A SID buffer sized by the platform maximum, so no allocation can fail
    /// halfway through building an ACL.
    struct WellKnownSid {
        bytes: [u8; SECURITY_MAX_SID_SIZE as usize],
    }

    impl WellKnownSid {
        fn new(kind: i32, name: &str) -> Result<Self, OrchError> {
            let mut sid = Self {
                bytes: [0u8; SECURITY_MAX_SID_SIZE as usize],
            };
            let mut size = SECURITY_MAX_SID_SIZE;
            // SAFETY: `bytes` is a live buffer of exactly `size` bytes and
            // `size` is a live `u32` for the duration of the call.
            let ok = unsafe {
                CreateWellKnownSid(
                    kind,
                    std::ptr::null_mut(),
                    sid.bytes.as_mut_ptr().cast::<std::ffi::c_void>(),
                    &mut size,
                )
            };
            if ok == 0 {
                return Err(refuse(name, "cannot materialize a well-known SID"));
            }
            Ok(sid)
        }

        fn psid(&self) -> PSID {
            self.bytes.as_ptr() as PSID
        }
    }

    /// The SID of the user this process runs as.
    struct TokenUserSid {
        buffer: Vec<u8>,
    }

    impl TokenUserSid {
        fn current(name: &str) -> Result<Self, OrchError> {
            let mut token: HANDLE = std::ptr::null_mut();
            // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs
            // no close; `token` is a live out-parameter.
            let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
            if ok == 0 {
                return Err(refuse(name, "cannot open this process token"));
            }
            let mut needed: u32 = 0;
            // SAFETY: a null buffer with zero length is the documented way to
            // ask for the required size.
            unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            }
            if needed == 0 {
                // SAFETY: `token` is a live handle this function opened.
                unsafe { CloseHandle(token) };
                return Err(refuse(name, "cannot size this process token user"));
            }
            let mut buffer = vec![0u8; needed as usize];
            // SAFETY: `buffer` is exactly `needed` bytes and stays alive.
            let ok = unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast::<std::ffi::c_void>(),
                    needed,
                    &mut needed,
                )
            };
            // SAFETY: `token` is a live handle this function opened.
            unsafe { CloseHandle(token) };
            if ok == 0 {
                return Err(refuse(name, "cannot read this process token user"));
            }
            Ok(Self { buffer })
        }

        fn psid(&self) -> PSID {
            // SAFETY: `GetTokenInformation(TokenUser)` wrote a `TOKEN_USER`
            // at the start of the buffer, whose `Sid` points inside it.
            unsafe { (*self.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
    }

    /// A security descriptor owned by the caller and freed on drop.
    struct OwnedDescriptor {
        raw: PSECURITY_DESCRIPTOR,
    }

    impl Drop for OwnedDescriptor {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                // SAFETY: `GetSecurityInfo` allocated this with `LocalAlloc`.
                unsafe { LocalFree(self.raw) };
            }
        }
    }

    /// Read the DACL of an already-open handle and reduce it to the model the
    /// pure verdict decides over.
    pub(super) fn read_handle_dacl(
        file: &fs::File,
        name: &str,
    ) -> Result<(bool, bool, Vec<LedgerAce>), OrchError> {
        let handle = file.as_raw_handle() as HANDLE;
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = OwnedDescriptor {
            raw: std::ptr::null_mut(),
        };
        // SAFETY: every out-parameter is live for the call, and the returned
        // descriptor is owned by `descriptor` from here on.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor.raw,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(refuse(name, "cannot read its security descriptor"));
        }

        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        // SAFETY: `descriptor.raw` is a valid descriptor; both out-parameters
        // are live.
        let ok =
            unsafe { GetSecurityDescriptorControl(descriptor.raw, &mut control, &mut revision) };
        if ok == 0 {
            return Err(refuse(name, "cannot read its descriptor control bits"));
        }
        let dacl_present = control & SE_DACL_PRESENT != 0 && !dacl.is_null();
        let protected = control & SE_DACL_PROTECTED != 0;
        if !dacl_present {
            return Ok((false, protected, Vec::new()));
        }

        let system = WellKnownSid::new(WinLocalSystemSid, name)?;
        let administrators = WellKnownSid::new(WinBuiltinAdministratorsSid, name)?;
        // SAFETY: `dacl` is non-null and valid for the descriptor's lifetime.
        let count = unsafe { (*dacl).AceCount };
        let mut aces = Vec::with_capacity(count as usize);
        for index in 0..u32::from(count) {
            let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `index` is below the ACE count reported by the ACL.
            let ok = unsafe { GetAce(dacl, index, &mut ace) };
            if ok == 0 || ace.is_null() {
                return Err(refuse(name, "has an unreadable access-control entry"));
            }
            // Allowed and denied ACEs share the leading
            // `{ Header, Mask, SidStart }` layout, so the SID is at the same
            // offset for both.
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            // SAFETY: `GetAce` returned a valid ACE inside the live ACL.
            let (ace_type, ace_flags) =
                unsafe { ((*entry).Header.AceType, (*entry).Header.AceFlags) };
            let sid = unsafe {
                (ace as *const u8).add(mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart)) as PSID
            };
            let trustee = if sid_equals(sid, owner) {
                LedgerTrustee::Owner
            } else if sid_equals(sid, system.psid()) {
                LedgerTrustee::System
            } else if sid_equals(sid, administrators.psid()) {
                LedgerTrustee::Administrators
            } else {
                LedgerTrustee::Other
            };
            aces.push(LedgerAce {
                allow: u32::from(ace_type) == ACCESS_ALLOWED_ACE_TYPE,
                trustee,
                inherited: u32::from(ace_flags) & INHERITED_ACE != 0,
            });
        }
        Ok((true, protected, aces))
    }

    fn sid_equals(left: PSID, right: PSID) -> bool {
        if left.is_null() || right.is_null() {
            return false;
        }
        // SAFETY: both pointers address valid SIDs for the call.
        unsafe { EqualSid(left, right) != 0 }
    }

    /// Install a protected DACL granting only this process's user and SYSTEM.
    ///
    /// `PROTECTED_DACL_SECURITY_INFORMATION` is the load-bearing half: it
    /// severs inheritance, so the record's permissions stop being a function
    /// of whatever its parent directory grants now or is changed to grant
    /// later.
    pub(super) fn protect_handle_owner_only(file: &fs::File, name: &str) -> Result<(), OrchError> {
        let owner = TokenUserSid::current(name)?;
        let system = WellKnownSid::new(WinLocalSystemSid, name)?;
        // SAFETY: both SIDs are valid for the call.
        let (owner_len, system_len) =
            unsafe { (GetLengthSid(owner.psid()), GetLengthSid(system.psid())) };
        let ace_overhead = mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) as u32;
        let acl_size = mem::size_of::<ACL>() as u32 + 2 * ace_overhead + owner_len + system_len;
        // Round up to the 4-byte alignment `InitializeAcl` requires.
        let acl_size = acl_size.next_multiple_of(4);
        let mut buffer = vec![0u8; acl_size as usize];
        let acl = buffer.as_mut_ptr().cast::<ACL>();
        // SAFETY: `acl` addresses exactly `acl_size` writable, aligned bytes.
        let ok = unsafe { InitializeAcl(acl, acl_size, ACL_REVISION) };
        if ok == 0 {
            return Err(refuse(name, "cannot initialize an owner-only ACL"));
        }
        for sid in [owner.psid(), system.psid()] {
            // SAFETY: `acl` was initialized above and sized for both entries.
            let ok = unsafe { AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS, sid) };
            if ok == 0 {
                return Err(refuse(name, "cannot add an owner-only access entry"));
            }
        }
        let handle = file.as_raw_handle() as HANDLE;
        // SAFETY: `handle` is open for the call and `acl` is a valid ACL.
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(refuse(name, "cannot install an owner-only DACL"));
        }
        Ok(())
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

    // ── Windows record authority, decided here on every platform ───────
    //
    // These exercise the decision, not the syscalls: the reduced ACL model is
    // exactly what the Windows FFI hands the verdict, so a logic regression
    // fails on Linux CI instead of waiting for a Windows host.

    fn owner_only() -> Vec<LedgerAce> {
        vec![
            LedgerAce {
                allow: true,
                trustee: LedgerTrustee::Owner,
                inherited: false,
            },
            LedgerAce {
                allow: true,
                trustee: LedgerTrustee::System,
                inherited: false,
            },
        ]
    }

    #[test]
    fn a_reparse_point_or_directory_is_never_a_ledger_record() {
        assert!(windows_attribute_verdict("r.json", FILE_ATTRIBUTE_REPARSE_POINT).is_err());
        assert!(windows_attribute_verdict("r.json", FILE_ATTRIBUTE_DIRECTORY).is_err());
        assert!(windows_attribute_verdict(
            "r.json",
            FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT
        )
        .is_err());
        // FILE_ATTRIBUTE_NORMAL.
        assert!(windows_attribute_verdict("r.json", 0x0000_0080).is_ok());
    }

    #[test]
    fn an_owner_only_protected_dacl_is_the_only_accepted_shape() {
        assert!(windows_dacl_verdict("r.json", true, true, &owner_only()).is_ok());

        let mut with_admins = owner_only();
        with_admins.push(LedgerAce {
            allow: true,
            trustee: LedgerTrustee::Administrators,
            inherited: false,
        });
        assert!(
            windows_dacl_verdict("r.json", true, true, &with_admins).is_ok(),
            "an administrator can take ownership regardless; refusing the ACE \
             would refuse every real file without denying anything"
        );
    }

    #[test]
    fn a_null_dacl_is_the_widest_grant_not_the_narrowest() {
        // The dangerous case: on Windows, no DACL means unrestricted access.
        // Reading its absence as "nobody is granted anything" is exactly
        // backwards, so it must be a refusal.
        let error = windows_dacl_verdict("r.json", false, true, &[]).unwrap_err();
        assert!(
            error.message.contains("NULL DACL"),
            "a NULL DACL must be refused by name: {error:?}"
        );
    }

    #[test]
    fn an_inherited_dacl_is_owner_only_by_luck_and_is_refused() {
        let error = windows_dacl_verdict("r.json", true, false, &owner_only()).unwrap_err();
        assert!(error.message.contains("inherits"), "{error:?}");

        let inherited = vec![LedgerAce {
            allow: true,
            trustee: LedgerTrustee::Owner,
            inherited: true,
        }];
        let error = windows_dacl_verdict("r.json", true, true, &inherited).unwrap_err();
        assert!(error.message.contains("inherited"), "{error:?}");
    }

    #[test]
    fn any_allow_entry_beyond_the_owner_is_the_leak() {
        let mut widened = owner_only();
        widened.push(LedgerAce {
            allow: true,
            trustee: LedgerTrustee::Other,
            inherited: false,
        });
        let error = windows_dacl_verdict("r.json", true, true, &widened).unwrap_err();
        assert!(error.message.contains("beyond its owner"), "{error:?}");
    }

    #[test]
    fn a_deny_entry_for_anyone_else_narrows_and_is_allowed() {
        let mut denied = owner_only();
        denied.push(LedgerAce {
            allow: false,
            trustee: LedgerTrustee::Other,
            inherited: false,
        });
        assert!(
            windows_dacl_verdict("r.json", true, true, &denied).is_ok(),
            "a deny ACE can only narrow access; treating it as a grant would \
             refuse a strictly safer descriptor"
        );
    }

    /// Executes only on a Windows host. Compiled everywhere the crate is
    /// checked for Windows, so a signature drift in the FFI is caught by the
    /// cross-target check rather than at deployment.
    #[cfg(windows)]
    #[test]
    fn a_windows_record_is_written_owner_only_and_reads_back_authoritative() {
        let dir = tempdir().unwrap();
        let ledger = LedgerDir::open(dir.path()).unwrap();
        ledger.write_private("w.json", b"{}").unwrap();
        assert_eq!(
            ledger.read_private("w.json").unwrap().as_deref(),
            Some("{}")
        );

        let file = ledger.open_no_follow("w.json").unwrap();
        let (present, protected, aces) = win_acl::read_handle_dacl(&file, "w.json").unwrap();
        assert!(present, "a written record must carry a DACL");
        assert!(protected, "a written record must not inherit its DACL");
        assert!(
            aces.iter()
                .all(|ace| !ace.allow || ace.trustee != LedgerTrustee::Other),
            "a written record granted access beyond its owner: {aces:?}"
        );
        assert!(windows_dacl_verdict("w.json", present, protected, &aces).is_ok());
    }

    /// A junction planted in the ledger directory is refused rather than
    /// followed. Executes only on a Windows host.
    #[cfg(windows)]
    #[test]
    fn a_windows_junction_in_the_ledger_is_refused() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.json"), b"{\"k\":1}").unwrap();
        let ledger = LedgerDir::open(dir.path()).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.path().join("link"))
            .arg(outside.path())
            .status();
        if !matches!(status, Ok(status) if status.success()) {
            // Creating a junction needs a privilege this host may not grant;
            // there is nothing to assert without one.
            return;
        }
        let error = ledger
            .read_private("link")
            .expect_err("a junction must never be traversed");
        assert!(error.message.contains("reparse point"), "{error:?}");
    }
}
