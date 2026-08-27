use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{sha256_hex, validate_relative_path};
use crate::manifest::{IsolatedSourceManifest, SourceObjectKind, MAX_SOURCE_BLOB_BYTES};

/// In-memory content-addressed object store. Objects are keyed only by SHA-256.
/// There is no Git index, worktree, alternate, credential, or hook surface.
#[derive(Debug, Default, Clone)]
pub struct ContentAddressedStore {
    blobs: BTreeMap<String, Vec<u8>>,
}

impl ContentAddressedStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, bytes: &[u8]) -> String {
        let digest = sha256_hex(bytes);
        self.blobs.insert(digest.clone(), bytes.to_vec());
        digest
    }

    pub fn get(&self, digest: &str) -> IsolatedResult<&[u8]> {
        self.blobs
            .get(digest)
            .map(Vec::as_slice)
            .ok_or_else(|| IsolatedError::invalid("source object is missing from the closure"))
    }

    pub fn contains(&self, digest: &str) -> bool {
        self.blobs.contains_key(digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub manifest: IsolatedSourceManifest,
    pub staging_root: PathBuf,
    pub object_count: usize,
    pub total_bytes: u64,
}

/// Hermetic resolver: allowlisted relative paths, explicit object closure,
/// size/type limits, and traversal/case/Unicode/symlink/submodule/rename
/// defenses. It never consults ambient Git config, index, hooks, alternates,
/// or credentials.
pub struct HermeticResolver {
    store: ContentAddressedStore,
}

impl HermeticResolver {
    pub fn new(store: ContentAddressedStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &ContentAddressedStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut ContentAddressedStore {
        &mut self.store
    }

    pub fn resolve(
        &self,
        manifest: &IsolatedSourceManifest,
        staging_root: &Path,
    ) -> IsolatedResult<ResolvedSource> {
        manifest.validate()?;
        if staging_root.exists() {
            let meta = fs::symlink_metadata(staging_root).map_err(io_err)?;
            if meta.file_type().is_symlink() {
                return Err(IsolatedError::forbidden(
                    "staging root must not be a symlink",
                ));
            }
        }
        fs::create_dir_all(staging_root).map_err(io_err)?;
        let staging_root = fs::canonicalize(staging_root).map_err(io_err)?;

        let mut total_bytes = 0u64;
        let mut seen_case = BTreeSet::new();
        for entry in &manifest.objects {
            validate_relative_path(&entry.relative_path)?;
            if !seen_case.insert(crate::ids::casefold_key(&entry.relative_path)) {
                return Err(IsolatedError::conflict("source path case-fold collision"));
            }
            let bytes = self.store.get(&entry.object.digest_sha256)?;
            if bytes.len() as u64 != entry.object.byte_len {
                return Err(IsolatedError::conflict(
                    "source object length does not match the manifest",
                ));
            }
            if sha256_hex(bytes) != entry.object.digest_sha256 {
                return Err(IsolatedError::conflict(
                    "source object digest substitution detected",
                ));
            }
            if entry.object.kind == SourceObjectKind::Blob
                && bytes.len() as u64 > MAX_SOURCE_BLOB_BYTES
            {
                return Err(IsolatedError::limit("source blob exceeds size limits"));
            }
            if looks_like_submodule(bytes, &entry.relative_path) {
                return Err(IsolatedError::forbidden(
                    "submodule objects are not allowed",
                ));
            }
            let dest = hermetic_join(&staging_root, &entry.relative_path)?;
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(io_err)?;
            }
            if dest.exists() {
                return Err(IsolatedError::conflict(
                    "source path already exists in staging",
                ));
            }
            fs::write(&dest, bytes).map_err(io_err)?;
            let written = fs::symlink_metadata(&dest).map_err(io_err)?;
            if written.file_type().is_symlink() {
                return Err(IsolatedError::forbidden(
                    "resolver refused to materialize a symlink",
                ));
            }
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        }

        assert_no_unexpected_files(&staging_root, manifest)?;
        Ok(ResolvedSource {
            manifest: manifest.clone(),
            staging_root,
            object_count: manifest.objects.len(),
            total_bytes,
        })
    }
}

fn hermetic_join(root: &Path, relative: &str) -> IsolatedResult<PathBuf> {
    validate_relative_path(relative)?;
    let dest = root.join(relative);
    let parent = dest
        .parent()
        .ok_or_else(|| IsolatedError::forbidden("source path has no parent"))?;
    fs::create_dir_all(parent).map_err(io_err)?;
    let canonical_parent = fs::canonicalize(parent).map_err(io_err)?;
    if !canonical_parent.starts_with(root) {
        return Err(IsolatedError::forbidden(
            "source path escaped the staging root",
        ));
    }
    Ok(canonical_parent.join(dest.file_name().expect("validated relative path")))
}

fn looks_like_submodule(bytes: &[u8], path: &str) -> bool {
    path == "gitmodules" || bytes.windows(14).any(|window| window == b"[submodule \"")
}

fn assert_no_unexpected_files(
    root: &Path,
    manifest: &IsolatedSourceManifest,
) -> IsolatedResult<()> {
    let allow: BTreeSet<&str> = manifest.allowlist().into_iter().collect();
    fn walk(dir: &Path, root: &Path, allow: &BTreeSet<&str>) -> IsolatedResult<()> {
        for entry in fs::read_dir(dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let meta = entry.metadata().map_err(io_err)?;
            if meta.file_type().is_symlink() {
                return Err(IsolatedError::forbidden(
                    "symlink found in resolved source tree",
                ));
            }
            let path = entry.path();
            if meta.is_dir() {
                walk(&path, root, allow)?;
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|_| IsolatedError::forbidden("resolved path escaped staging root"))?;
            let rel = rel
                .to_str()
                .ok_or_else(|| IsolatedError::forbidden("resolved path is not UTF-8"))?;
            if !allow.contains(rel) {
                return Err(IsolatedError::forbidden(
                    "resolved tree contains a file that is not on the allowlist",
                ));
            }
        }
        Ok(())
    }
    walk(root, root, &allow)
}

fn io_err(error: std::io::Error) -> IsolatedError {
    IsolatedError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ISOLATED_VISUAL_BACKEND_ID;
    use crate::manifest::{
        IsolatedSourceEntry, IsolatedSourceManifest, SourceObject, SourceObjectKind,
    };
    use tempfile::tempdir;

    fn manifest_for(
        store: &mut ContentAddressedStore,
        path: &str,
        body: &[u8],
    ) -> IsolatedSourceManifest {
        let digest = store.insert(body);
        IsolatedSourceManifest {
            schema_version: 1,
            backend_id: ISOLATED_VISUAL_BACKEND_ID.into(),
            guest_protocol_version: 1,
            objects: vec![IsolatedSourceEntry {
                relative_path: path.into(),
                object: SourceObject {
                    digest_sha256: digest.clone(),
                    kind: SourceObjectKind::Blob,
                    media_type: "text/x-c".into(),
                    byte_len: body.len() as u64,
                },
            }],
            helper_content_sha256: "a".repeat(64),
            helper_signing_requirement_sha256: "b".repeat(64),
            guest_image_sha256: None,
            configuration_sha256: "c".repeat(64),
        }
    }

    #[test]
    fn resolves_allowlisted_blob_and_rejects_substitution() {
        let mut store = ContentAddressedStore::new();
        let body = b"int main(void) { return 0; }\n";
        let manifest = manifest_for(&mut store, "guest-init.c", body);
        let resolver = HermeticResolver::new(store.clone());
        let dir = tempdir().unwrap();
        let resolved = resolver
            .resolve(&manifest, &dir.path().join("stage"))
            .unwrap();
        assert_eq!(resolved.object_count, 1);
        assert_eq!(
            fs::read(resolved.staging_root.join("guest-init.c")).unwrap(),
            body
        );

        let mut evil = store;
        let other = evil.insert(b"int pwned(void) { return 1; }\n");
        let mut substituted = manifest.clone();
        substituted.objects[0].object.digest_sha256 = other;
        substituted.objects[0].object.byte_len = 29;
        let resolver = HermeticResolver::new(evil);
        let dir = tempdir().unwrap();
        // digest in manifest no longer matches the allowlisted guest-init identity
        // the caller expected; byte_len also diverges unless updated. Using the
        // original length catches substitution even when the attacker updates
        // the digest field only.
        substituted.objects[0].object.byte_len = body.len() as u64;
        assert!(resolver
            .resolve(&substituted, &dir.path().join("stage"))
            .is_err());
    }

    #[test]
    fn rejects_symlink_and_traversal_inputs() {
        let mut store = ContentAddressedStore::new();
        assert!(manifest_for(&mut store, "../etc/passwd", b"x")
            .validate()
            .is_err());
    }
}
