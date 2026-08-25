//! Private, crash-durable, no-follow JSON storage for external-worker records.
//!
//! The ledger and the authority store both hold records that decide whether a
//! provider action is allowed. A record that another local user can read, that
//! can be redirected through a symlink, or that a crash can leave half-applied
//! is not a safety property. Every write here is:
//!
//! * written to an **unpredictable** temp name, so a second writer or a local
//!   attacker cannot pre-create or guess the path being staged;
//! * created `O_EXCL` and `O_NOFOLLOW` with mode `0600` on Unix, so the final
//!   component cannot be swapped for a symlink and the bytes are private;
//! * `fsync`ed before the rename **and** the parent directory `fsync`ed after,
//!   so the rename itself survives a crash rather than only the contents;
//! * optionally compare-and-swapped against the digest the caller last read,
//!   so a concurrent writer cannot be silently clobbered.
//!
//! Known residual: true handle-relative resolution (`openat`/`renameat` against
//! a pinned directory descriptor) is not available in `std`. `O_NOFOLLOW`
//! closes the final-component swap, which is the vector that matters for these
//! files; a swap of an intermediate directory would need `rustix`/`cap-std`.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Bytes accepted when reading one durable record.
///
/// These records are bounded projections. A file larger than this is not a
/// record this process wrote, so it is refused rather than parsed.
pub(crate) const MAX_DURABLE_RECORD_BYTES: u64 = 1024 * 1024;

/// Unix mode for durable records: owner read/write only.
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
/// Unix mode for durable directories: owner access only.
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Create a directory tree that only the owner can traverse.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tighten every component we own. A pre-existing wider directory is
        // narrowed rather than trusted.
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            if let Ok(metadata) = fs::metadata(&current) {
                if metadata.is_dir() && metadata.permissions().mode() & 0o077 != 0 {
                    let _ =
                        fs::set_permissions(&current, fs::Permissions::from_mode(PRIVATE_DIR_MODE));
                }
            }
        }
    }
    Ok(())
}

/// Open a file for reading without following a symlink at the final component.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Do not traverse a reparse point (junction or symlink) placed at the
        // final component; open the link itself so the read fails closed.
        options.custom_flags(windows_flags::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Create a new file that must not already exist, private and no-follow.
fn create_new_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
        // `create_new` already implies O_EXCL; O_NOFOLLOW additionally refuses
        // an attacker-planted symlink sitting at this exact name.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_flags::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
mod windows_flags {
    /// `FILE_FLAG_OPEN_REPARSE_POINT` from the Win32 API. Declared locally so
    /// this module does not take a `windows-sys` dependency for one constant.
    pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
}

/// Read a durable record, refusing symlinks and oversized files.
///
/// `Ok(None)` means the record does not exist. A symlink at the final
/// component, or a file above the record ceiling, is an error rather than a
/// value: both mean this is not a record this process wrote.
pub(crate) fn read_private_json(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        // O_NOFOLLOW reports ELOOP when the final component is a symlink.
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable record is not a regular file",
        ));
    }
    if metadata.len() > MAX_DURABLE_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable record exceeds its byte ceiling",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_DURABLE_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DURABLE_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable record exceeds its byte ceiling",
        ));
    }
    Ok(Some(bytes))
}

/// Digest of a durable record's current bytes, for compare-and-swap.
///
/// `None` means the record does not exist, which is itself a CAS precondition
/// a caller can require.
pub(crate) fn record_digest(path: &Path) -> io::Result<Option<String>> {
    Ok(read_private_json(path)?.map(|bytes| hex_digest(&bytes)))
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `fsync` a directory so a rename into it survives a crash.
///
/// Directory `fsync` is a no-op that returns an error on Windows; the NTFS
/// rename is already ordered there, so the error is not fatal.
fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match File::open(parent) {
        Ok(dir) => {
            #[cfg(unix)]
            {
                dir.sync_all()?;
            }
            #[cfg(not(unix))]
            {
                let _ = dir.sync_all();
            }
            Ok(())
        }
        // A missing parent means the caller never created the tree.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(error),
        Err(_) if cfg!(windows) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Stage bytes under an unpredictable private temp name in the same directory.
///
/// The name is a v4 UUID, so it comes from the OS RNG rather than from the
/// destination path. A local attacker cannot pre-create the path being staged,
/// and two concurrent writers cannot collide on it.
fn stage(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "durable path has no parent"))?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut file = create_new_private(&temp)?;
    let write = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    if let Err(error) = write {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(temp)
}

/// Write a durable record atomically, privately, and crash-durably.
pub(crate) fn write_private_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let temp = stage(path, &bytes)?;
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    // The contents were synced before the rename; sync the directory so the
    // rename itself is durable rather than only the bytes it points at.
    sync_parent(path)
}

/// Write a durable record only if its current bytes still hash to `expected`.
///
/// `expected == None` requires that the record does not yet exist. A caller
/// that read, decided, and is now writing passes the digest it read, so a
/// concurrent writer is refused instead of silently overwritten.
pub(crate) fn cas_private_json<T: Serialize>(
    path: &Path,
    expected: Option<&str>,
    value: &T,
) -> io::Result<()> {
    let observed = record_digest(path)?;
    if observed.as_deref() != expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "durable record changed since it was read",
        ));
    }
    if expected.is_none() {
        // Creation is its own compare-and-swap: O_EXCL means only one writer
        // can win, without a window between the check above and the create.
        let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        let temp = stage(path, &bytes)?;
        // `hard_link` fails if the destination exists, giving an atomic
        // create-only publish that `rename` cannot express.
        let published = fs::hard_link(&temp, path);
        let _ = fs::remove_file(&temp);
        published?;
        return sync_parent(path);
    }
    write_private_json(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Record {
        value: String,
    }

    fn record(value: &str) -> Record {
        Record {
            value: value.into(),
        }
    }

    #[test]
    fn records_are_private_and_staged_under_an_unpredictable_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");
        write_private_json(&path, &record("one")).unwrap();
        let bytes = read_private_json(&path).unwrap().expect("record exists");
        let decoded: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, record("one"));

        // The predictable name the previous implementation staged under must
        // not be what this one uses, and nothing may be left behind.
        assert!(!dir.path().join("record.json.tmp").exists());
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "record.json")
            .count();
        assert_eq!(leftovers, 0, "staging must not leave temp files behind");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, PRIVATE_FILE_MODE, "record must be owner-only");
        }
    }

    #[test]
    fn a_missing_record_reads_as_none_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_private_json(&dir.path().join("absent.json"))
            .unwrap()
            .is_none());
        assert!(record_digest(&dir.path().join("absent.json"))
            .unwrap()
            .is_none());
    }

    /// A symlink planted at the record's own name redirected the previous
    /// `File::create` write to whatever it pointed at.
    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_record_name_fails_closed_on_read_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.json");
        fs::write(&outside, b"{\"value\":\"attacker\"}").unwrap();
        let path = dir.path().join("record.json");
        std::os::unix::fs::symlink(&outside, &path).unwrap();

        assert!(
            read_private_json(&path).is_err(),
            "a symlinked record must not be read through",
        );
        // Writing publishes by rename, which replaces the symlink itself
        // rather than writing through it, so the target must be untouched.
        write_private_json(&path, &record("ours")).unwrap();
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "{\"value\":\"attacker\"}",
            "the symlink target must not have been written through",
        );
        let bytes = read_private_json(&path).unwrap().expect("record exists");
        let decoded: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, record("ours"));
    }

    #[test]
    fn compare_and_swap_refuses_a_concurrent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");

        // Creation requires that nothing is there yet.
        cas_private_json(&path, None, &record("first")).unwrap();
        assert!(
            cas_private_json(&path, None, &record("second")).is_err(),
            "a second create must not overwrite the first",
        );

        let seen = record_digest(&path).unwrap();
        // A writer holding a stale digest is refused.
        assert!(cas_private_json(&path, Some("stale"), &record("third")).is_err());
        // A writer holding the current digest wins.
        cas_private_json(&path, seen.as_deref(), &record("third")).unwrap();
        let bytes = read_private_json(&path).unwrap().unwrap();
        let decoded: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, record("third"));
        // And the digest moved, so the previous holder is now stale too.
        assert_ne!(record_digest(&path).unwrap(), seen);
    }

    #[test]
    fn an_oversized_record_is_refused_rather_than_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");
        fs::write(&path, vec![b'x'; (MAX_DURABLE_RECORD_BYTES + 1) as usize]).unwrap();
        assert!(read_private_json(&path).is_err());
        assert!(record_digest(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_directories_are_narrowed_even_when_they_already_exist() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o777)).unwrap();
        create_private_dir_all(&nested).unwrap();
        let mode = fs::metadata(&nested).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group and other access must be removed");
    }
}
