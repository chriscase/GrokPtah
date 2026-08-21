//! Bounded, crash-safe artifact storage for certification campaigns.
//!
//! This module deliberately does not accept an arbitrary path and then rely on
//! `canonicalize` after a write.  The output root is owned by a schema sentinel,
//! campaigns are exclusive direct children, and every caller-supplied relative
//! path is walked one component at a time without following symbolic links.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_OUTPUT_RELATIVE_PATH: &str = "evals/runs/persistent-agent-cert";
pub const OUTPUT_ROOT_SCHEMA: &str = "grokptah.certification_output_root.v1";
pub const CAMPAIGN_SENTINEL_SCHEMA: &str = "grokptah.certification_campaign.v1";
pub const CAMPAIGN_COMPLETE_SCHEMA: &str = "grokptah.certification_campaign_complete.v1";

const ROOT_SENTINEL: &str = ".grokptah-cert-output.json";
const CAMPAIGN_SENTINEL: &str = ".grokptah-cert-campaign.json";
const COMPLETE_SENTINEL: &str = ".grokptah-cert-complete.json";
const CAMPAIGN_LOCK: &str = ".grokptah-cert-lock";
const INTERNAL_PREFIX: &str = ".grokptah-cert-";
const MAX_CONTROL_BYTES: usize = 4 * 1024;
const MAX_RELATIVE_PATH_BYTES: usize = 1_024;
const MAX_PATH_COMPONENT_BYTES: usize = 255;
const MAX_CAMPAIGN_ID_BYTES: usize = 128;
const MAX_RETENTION_CHILDREN: usize = 10_000;
const MAX_TREE_ENTRIES: usize = 100_000;
const MAX_ARTIFACT_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_SEALED_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStage {
    Partial,
    Final,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDigest {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub stage: ArtifactStage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub removed: usize,
    pub retained: usize,
    pub preserved: usize,
}

#[derive(Debug)]
pub struct SafeOutputRoot {
    root: PathBuf,
    repository_root: PathBuf,
    active_workspace: Option<PathBuf>,
    artifact_budget_bytes: u64,
}

#[derive(Debug)]
pub struct CampaignArtifacts {
    directory: PathBuf,
    campaign_id: String,
    budget: Arc<Budget>,
    final_installs: Mutex<HashSet<FinalInstall>>,
    seal_allowed: bool,
    // Held for the entire writable campaign lifetime. A second process cannot
    // resume, seal, or mutate the same campaign concurrently.
    _lock: File,
}

#[derive(Debug)]
struct Budget {
    limit: u64,
    used: Mutex<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FinalInstall {
    relative_path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootSentinel {
    schema: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignSentinel {
    schema: String,
    campaign_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteSentinel {
    schema: String,
    campaign_id: String,
    completed_at_unix_millis: u64,
    sealed_artifact: ArtifactDigest,
}

#[derive(Debug)]
struct CompletedCampaign {
    path: PathBuf,
    campaign_id: String,
    completed_at_unix_millis: u64,
}

impl SafeOutputRoot {
    /// Opens or initializes a certification output root.
    ///
    /// `active_workspace` means the disposable workspace on which a scenario
    /// will operate, not the source repository.  The documented ignored output
    /// root is allowed to be a descendant of `repository_root`; it must never
    /// overlap the active scenario workspace.
    pub fn open(
        root: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
        active_workspace: Option<&Path>,
        artifact_budget_bytes: u64,
    ) -> Result<Self> {
        validate_budget(artifact_budget_bytes)?;

        let repository_root = canonical_directory(repository_root.as_ref(), "repository root")?;
        let root = absolute_lexical_path(root.as_ref()).context("output root")?;
        validate_root_relationships(&root, &repository_root, active_workspace)?;

        create_directory_chain_without_symlinks(&root)
            .with_context(|| format!("could not create safe output root {}", root.display()))?;
        let canonical_root = dunce::canonicalize(&root).context("canonicalizing output root")?;
        if canonical_root != root {
            bail!("output root resolves through a symbolic link");
        }
        if !canonical_root.is_dir() {
            bail!("output root is not a directory");
        }

        let active_workspace = match active_workspace {
            Some(path) => {
                let path = canonical_directory(path, "active workspace")?;
                if paths_overlap(&canonical_root, &path) {
                    bail!("output root overlaps the active scenario workspace");
                }
                Some(path)
            }
            None => None,
        };

        initialize_or_validate_root_sentinel(&canonical_root)?;
        let available = fs2::available_space(&canonical_root)
            .context("checking free space for certification artifacts")?;
        if available < artifact_budget_bytes {
            bail!("free disk is below the configured artifact budget");
        }

        Ok(Self {
            root: canonical_root,
            repository_root,
            active_workspace,
            artifact_budget_bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    pub fn active_workspace(&self) -> Option<&Path> {
        self.active_workspace.as_deref()
    }

    pub fn artifact_budget_bytes(&self) -> u64 {
        self.artifact_budget_bytes
    }

    /// Creates a new, exclusively named campaign directory.
    pub fn create_campaign(&self, campaign_id: &str) -> Result<CampaignArtifacts> {
        validate_campaign_id(campaign_id)?;
        self.validate_root_sentinel()?;
        let directory = self.root.join(campaign_id);
        match fs::symlink_metadata(&directory) {
            Ok(_) => bail!("campaign directory already exists"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspecting campaign directory"),
        }
        create_private_directory(&directory).context("creating campaign directory")?;
        sync_directory(&self.root)?;

        let marker = CampaignSentinel {
            schema: CAMPAIGN_SENTINEL_SCHEMA.to_owned(),
            campaign_id: campaign_id.to_owned(),
        };
        let marker_bytes = serde_json::to_vec_pretty(&marker)?;
        if let Err(error) = atomic_install(
            &directory,
            Path::new(CAMPAIGN_SENTINEL),
            &marker_bytes,
            InstallMode::NoClobber,
        ) {
            // A marker-less directory is intentionally left behind.  Retention
            // treats it as unknown and will never remove it automatically.
            return Err(error).context("writing campaign ownership sentinel");
        }
        let lock = create_and_lock_campaign(&directory)?;
        Ok(CampaignArtifacts {
            directory,
            campaign_id: campaign_id.to_owned(),
            budget: Arc::new(Budget {
                limit: self.artifact_budget_bytes,
                used: Mutex::new(0),
            }),
            final_installs: Mutex::new(HashSet::new()),
            seal_allowed: true,
            _lock: lock,
        })
    }

    /// Reopens an interrupted campaign after validating its ownership marker.
    /// Existing regular artifact bytes are charged against the budget.
    pub fn resume_campaign(&self, campaign_id: &str) -> Result<CampaignArtifacts> {
        validate_campaign_id(campaign_id)?;
        self.validate_root_sentinel()?;
        let directory = self.root.join(campaign_id);
        validate_campaign_directory(&directory, campaign_id)?;
        ensure_campaign_unsealed(&directory)?;
        let lock = open_and_lock_campaign(&directory)?;
        let used = measure_artifact_tree(&directory)?;
        if used > self.artifact_budget_bytes {
            bail!("existing campaign artifacts exceed the configured budget");
        }
        Ok(CampaignArtifacts {
            directory,
            campaign_id: campaign_id.to_owned(),
            budget: Arc::new(Budget {
                limit: self.artifact_budget_bytes,
                used: Mutex::new(used),
            }),
            final_installs: Mutex::new(HashSet::new()),
            // V1 campaign resume is recovery-only. Without a persistent,
            // immutable action/install journal, a reopened process cannot
            // prove which existing pathname was installed as final.
            seal_allowed: false,
            _lock: lock,
        })
    }

    /// Removes only the oldest completed, marker-owned direct campaign
    /// children. Unknown, malformed, current, incomplete, or symlink-containing
    /// children are preserved.
    pub fn prune_completed(
        &self,
        keep_newest: usize,
        current_campaign_id: Option<&str>,
    ) -> Result<PruneSummary> {
        self.validate_root_sentinel()?;
        if keep_newest == 0 {
            bail!("retention must keep at least one completed campaign");
        }
        if let Some(id) = current_campaign_id {
            validate_campaign_id(id)?;
        }

        let mut completed = Vec::new();
        let mut summary = PruneSummary::default();
        let mut children = 0usize;
        for entry in fs::read_dir(&self.root).context("reading output root for retention")? {
            let entry = entry?;
            children = children
                .checked_add(1)
                .ok_or_else(|| anyhow!("retention child count overflow"))?;
            if children > MAX_RETENTION_CHILDREN {
                bail!("output root has too many direct children for safe retention");
            }
            let name = entry.file_name();
            if name == OsStr::new(ROOT_SENTINEL) {
                continue;
            }
            let Some(name) = name.to_str() else {
                summary.preserved += 1;
                continue;
            };
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                summary.preserved += 1;
                continue;
            }
            if current_campaign_id == Some(name) {
                summary.preserved += 1;
                continue;
            }
            match completed_campaign(&path, name) {
                Ok(Some(candidate)) if !tree_contains_symlink(&candidate.path)? => {
                    completed.push(candidate)
                }
                Ok(None) | Err(_) => summary.preserved += 1,
                Ok(Some(_)) => summary.preserved += 1,
            }
        }

        completed.sort_by(|left, right| {
            right
                .completed_at_unix_millis
                .cmp(&left.completed_at_unix_millis)
                .then_with(|| right.campaign_id.cmp(&left.campaign_id))
        });
        summary.retained = completed.len().min(keep_newest);
        for candidate in completed.into_iter().skip(keep_newest) {
            // Revalidate immediately before deletion, including a bounded scan
            // which rejects every nested symbolic link.
            if completed_campaign(&candidate.path, &candidate.campaign_id)?.is_none()
                || tree_contains_symlink(&candidate.path)?
            {
                summary.preserved += 1;
                continue;
            }
            let Ok(_lock) = open_and_lock_campaign(&candidate.path) else {
                summary.preserved += 1;
                continue;
            };
            fs::remove_dir_all(&candidate.path).context("pruning completed campaign")?;
            summary.removed = summary
                .removed
                .checked_add(1)
                .ok_or_else(|| anyhow!("retention removal count overflow"))?;
        }
        if summary.removed > 0 {
            sync_directory(&self.root)?;
        }
        Ok(summary)
    }

    fn validate_root_sentinel(&self) -> Result<()> {
        validate_exact_root_sentinel(&self.root)
    }
}

impl CampaignArtifacts {
    pub fn path(&self) -> &Path {
        &self.directory
    }

    pub fn campaign_id(&self) -> &str {
        &self.campaign_id
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget.limit
    }

    pub fn used_bytes(&self) -> Result<u64> {
        Ok(*self
            .budget
            .used
            .lock()
            .map_err(|_| anyhow!("artifact budget lock is poisoned"))?)
    }

    /// Atomically installs a recoverable checkpoint. A prior checkpoint at the
    /// same path may be replaced, but the transient old+new byte total must fit
    /// within the campaign budget.
    pub fn write_partial(
        &self,
        relative_path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<ArtifactDigest> {
        self.write(relative_path.as_ref(), bytes, ArtifactStage::Partial)
    }

    /// Atomically and exclusively installs a final artifact. Existing targets
    /// are never overwritten.
    pub fn write_final(
        &self,
        relative_path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<ArtifactDigest> {
        self.write(relative_path.as_ref(), bytes, ArtifactStage::Final)
    }

    pub fn bounded_read(
        &self,
        relative_path: impl AsRef<Path>,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>> {
        if maximum_bytes == 0 || maximum_bytes > self.budget.limit {
            bail!("read bound is zero or exceeds the campaign artifact budget");
        }
        let relative = validate_relative_artifact_path(relative_path.as_ref())?;
        let path = resolve_existing_file(&self.directory, &relative)?;
        read_regular_file_bounded(&path, maximum_bytes)
    }

    pub fn verify(
        &self,
        relative_path: impl AsRef<Path>,
        expected_bytes: u64,
        expected_sha256: &str,
    ) -> Result<VerifiedArtifact> {
        if !is_sha256(expected_sha256) {
            bail!("expected artifact digest is not lowercase SHA-256");
        }
        if expected_bytes > self.budget.limit {
            bail!("expected artifact bytes exceed campaign budget");
        }
        let relative = validate_relative_artifact_path(relative_path.as_ref())?;
        let bytes = self.bounded_read(&relative, expected_bytes.max(1))?;
        if bytes.len() as u64 != expected_bytes {
            bail!("artifact byte count does not match its reference");
        }
        let sha256 = digest(&bytes);
        if sha256 != expected_sha256 {
            bail!("artifact digest does not match its reference");
        }
        Ok(VerifiedArtifact {
            relative_path: portable_relative_path(&relative)?,
            sha256,
            bytes: expected_bytes,
        })
    }

    /// Installs the completion marker. Retention ignores a campaign until this
    /// succeeds, so an interruption can leave only recoverable partial data.
    pub fn mark_complete(&self, sealed_artifact: &ArtifactDigest) -> Result<()> {
        self.validate_writable()?;
        if !self.seal_allowed || sealed_artifact.stage != ArtifactStage::Final {
            bail!("campaign completion requires a current-lifetime final install");
        }
        let key = FinalInstall {
            relative_path: sealed_artifact.relative_path.clone(),
            sha256: sealed_artifact.sha256.clone(),
            bytes: sealed_artifact.bytes,
        };
        if !self
            .final_installs
            .lock()
            .map_err(|_| anyhow!("final install registry lock is poisoned"))?
            .contains(&key)
        {
            bail!("campaign seal does not reference a final install from this writer");
        }
        self.verify(
            &sealed_artifact.relative_path,
            sealed_artifact.bytes,
            &sealed_artifact.sha256,
        )?;
        if tree_contains_internal_temporary(&self.directory)? {
            bail!("campaign cannot be sealed while interrupted temporary files remain");
        }
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock precedes Unix epoch")?;
        let millis = u64::try_from(elapsed.as_millis())
            .map_err(|_| anyhow!("completion timestamp is outside its bound"))?;
        let marker = CompleteSentinel {
            schema: CAMPAIGN_COMPLETE_SCHEMA.to_owned(),
            campaign_id: self.campaign_id.clone(),
            completed_at_unix_millis: millis,
            sealed_artifact: sealed_artifact.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&marker)?;
        atomic_install(
            &self.directory,
            Path::new(COMPLETE_SENTINEL),
            &bytes,
            InstallMode::NoClobber,
        )?;
        Ok(())
    }

    fn write(&self, relative: &Path, bytes: &[u8], stage: ArtifactStage) -> Result<ArtifactDigest> {
        self.validate_writable()?;
        let relative = validate_relative_artifact_path(relative)?;
        let bytes_len = u64::try_from(bytes.len()).map_err(|_| anyhow!("artifact is too large"))?;
        if bytes_len > self.budget.limit {
            bail!("artifact exceeds campaign byte budget");
        }
        let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
        let parent = create_relative_directory_chain(&self.directory, parent_relative)?;
        let file_name = relative
            .file_name()
            .ok_or_else(|| anyhow!("artifact path has no filename"))?;
        let target = parent.join(file_name);
        let old_bytes = inspect_target_for_write(&target, stage)?;

        let mut used = self
            .budget
            .used
            .lock()
            .map_err(|_| anyhow!("artifact budget lock is poisoned"))?;
        let transient = used
            .checked_add(bytes_len)
            .ok_or_else(|| anyhow!("artifact byte accounting overflow"))?;
        if transient > self.budget.limit {
            bail!("artifact write would exceed campaign byte budget");
        }
        let mode = match stage {
            ArtifactStage::Partial => InstallMode::Replace,
            ArtifactStage::Final => InstallMode::NoClobber,
        };
        atomic_install(&parent, Path::new(file_name), bytes, mode)?;
        *used = transient
            .checked_sub(old_bytes)
            .ok_or_else(|| anyhow!("artifact byte accounting underflow"))?;

        let digest = ArtifactDigest {
            relative_path: portable_relative_path(&relative)?,
            sha256: digest(bytes),
            bytes: bytes_len,
            stage,
        };
        if stage == ArtifactStage::Final {
            self.final_installs
                .lock()
                .map_err(|_| anyhow!("final install registry lock is poisoned"))?
                .insert(FinalInstall {
                    relative_path: digest.relative_path.clone(),
                    sha256: digest.sha256.clone(),
                    bytes: digest.bytes,
                });
        }
        Ok(digest)
    }

    fn validate_writable(&self) -> Result<()> {
        validate_campaign_directory(&self.directory, &self.campaign_id)?;
        ensure_campaign_unsealed(&self.directory)
    }
}

#[derive(Debug, Clone, Copy)]
enum InstallMode {
    NoClobber,
    Replace,
}

fn validate_budget(budget: u64) -> Result<()> {
    if budget == 0 || budget > MAX_ARTIFACT_BUDGET_BYTES {
        bail!("artifact budget is zero or exceeds the supported maximum");
    }
    Ok(())
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let canonical = dunce::canonicalize(path).with_context(|| format!("canonicalizing {label}"))?;
    if !canonical.is_dir() {
        bail!("{label} is not a directory");
    }
    Ok(canonical)
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        dunce::canonicalize(path).map_err(Into::into)
    } else {
        absolute_lexical_path(path)
    }
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("path must be absolute");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => {
                if part == OsStr::new(".git") {
                    bail!("path may not traverse a .git directory");
                }
                normalized.push(part);
            }
            Component::CurDir | Component::ParentDir => {
                bail!("path may not contain traversal components")
            }
        }
    }
    if normalized.parent().is_none() {
        bail!("filesystem root is not a safe output path");
    }
    Ok(normalized)
}

fn validate_root_relationships(
    root: &Path,
    repository_root: &Path,
    active_workspace: Option<&Path>,
) -> Result<()> {
    if repository_root.starts_with(root) {
        bail!("output root may not be the repository root or one of its ancestors");
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = canonical_or_absolute(Path::new(&home)).context("validating home path")?;
        if home.starts_with(root) {
            bail!("output root may not be the home directory or one of its ancestors");
        }
    }
    if let Some(workspace) = active_workspace {
        let workspace = canonical_directory(workspace, "active workspace")?;
        if paths_overlap(root, &workspace) {
            bail!("output root overlaps the active scenario workspace");
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn create_directory_chain_without_symlinks(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("path contains a symbolic link component");
                }
                if !metadata.is_dir() {
                    bail!("path component is not a directory");
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| anyhow!("could not find an existing path ancestor"))?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| anyhow!("could not find an existing path ancestor"))?;
            }
            Err(error) => return Err(error).context("inspecting output path component"),
        }
    }
    let mut cursor = cursor.to_owned();
    for component in missing.iter().rev() {
        let next = cursor.join(component);
        create_private_directory(&next).context("creating output path component")?;
        let metadata = fs::symlink_metadata(&next)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("created output path component is not a safe directory");
        }
        sync_directory(&cursor)?;
        cursor = next;
    }
    Ok(())
}

fn initialize_or_validate_root_sentinel(root: &Path) -> Result<()> {
    let sentinel = root.join(ROOT_SENTINEL);
    match fs::symlink_metadata(&sentinel) {
        Ok(_) => validate_exact_root_sentinel(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::read_dir(root)?.next().transpose()?.is_some() {
                bail!("non-empty output root has no valid ownership sentinel");
            }
            let bytes = serde_json::to_vec_pretty(&RootSentinel {
                schema: OUTPUT_ROOT_SCHEMA.to_owned(),
            })?;
            atomic_install(
                root,
                Path::new(ROOT_SENTINEL),
                &bytes,
                InstallMode::NoClobber,
            )
        }
        Err(error) => Err(error).context("inspecting output root sentinel"),
    }
}

fn validate_exact_root_sentinel(root: &Path) -> Result<()> {
    let value: RootSentinel = read_control_json(&root.join(ROOT_SENTINEL))?;
    if value.schema != OUTPUT_ROOT_SCHEMA {
        bail!("output root sentinel uses an unknown schema");
    }
    Ok(())
}

fn validate_campaign_directory(directory: &Path, campaign_id: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(directory).context("inspecting campaign directory")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("campaign path is not a safe directory");
    }
    let marker: CampaignSentinel = read_control_json(&directory.join(CAMPAIGN_SENTINEL))?;
    if marker.schema != CAMPAIGN_SENTINEL_SCHEMA || marker.campaign_id != campaign_id {
        bail!("campaign ownership sentinel does not match");
    }
    Ok(())
}

fn ensure_campaign_unsealed(directory: &Path) -> Result<()> {
    match fs::symlink_metadata(directory.join(COMPLETE_SENTINEL)) {
        Ok(_) => bail!("completed campaign is sealed against mutation or resume"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspecting campaign completion state"),
    }
}

fn create_and_lock_campaign(directory: &Path) -> Result<File> {
    let path = directory.join(CAMPAIGN_LOCK);
    let file = open_private_create_new(&path).context("creating campaign lifetime lock")?;
    file.try_lock_exclusive()
        .context("acquiring campaign lifetime lock")?;
    sync_directory(directory)?;
    Ok(file)
}

fn open_and_lock_campaign(directory: &Path) -> Result<File> {
    let path = directory.join(CAMPAIGN_LOCK);
    let metadata = fs::symlink_metadata(&path).context("campaign lifetime lock is unavailable")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("campaign lifetime lock is not a regular file");
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .context("opening campaign lifetime lock")?;
    let opened = file.metadata().context("inspecting opened campaign lock")?;
    if !same_file(&metadata, &opened) {
        bail!("campaign lifetime lock changed while opening");
    }
    file.try_lock_exclusive()
        .context("campaign is already active in another process")?;
    Ok(file)
}

fn completed_campaign(directory: &Path, campaign_id: &str) -> Result<Option<CompletedCampaign>> {
    validate_campaign_id(campaign_id)?;
    validate_campaign_directory(directory, campaign_id)?;
    let complete_path = directory.join(COMPLETE_SENTINEL);
    let marker: CompleteSentinel = match fs::symlink_metadata(&complete_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Ok(None);
            }
            read_control_json(&complete_path)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if marker.schema != CAMPAIGN_COMPLETE_SCHEMA
        || marker.campaign_id != campaign_id
        || marker.completed_at_unix_millis == 0
        || marker.sealed_artifact.stage != ArtifactStage::Final
    {
        return Ok(None);
    }
    let relative =
        match validate_relative_artifact_path(Path::new(&marker.sealed_artifact.relative_path)) {
            Ok(relative) => relative,
            Err(_) => return Ok(None),
        };
    if !is_sha256(&marker.sealed_artifact.sha256)
        || marker.sealed_artifact.bytes == 0
        || marker.sealed_artifact.bytes > MAX_SEALED_ARTIFACT_BYTES
    {
        return Ok(None);
    }
    let sealed_path = match resolve_existing_file(directory, &relative) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let sealed_bytes = match read_regular_file_bounded(&sealed_path, marker.sealed_artifact.bytes) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    if sealed_bytes.len() as u64 != marker.sealed_artifact.bytes
        || digest(&sealed_bytes) != marker.sealed_artifact.sha256
    {
        return Ok(None);
    }
    Ok(Some(CompletedCampaign {
        path: directory.to_owned(),
        campaign_id: campaign_id.to_owned(),
        completed_at_unix_millis: marker.completed_at_unix_millis,
    }))
}

/// Validate a sealed direct campaign child and return its independently
/// verified final artifact reference. This is the read-only entry point used
/// by report inspection; an unsealed but otherwise valid report is not a
/// completed campaign.
pub fn verify_completed_campaign(directory: &Path) -> Result<ArtifactDigest> {
    if !directory.is_absolute() {
        bail!("completed campaign path must be absolute");
    }
    let parent = directory
        .parent()
        .ok_or_else(|| anyhow!("completed campaign has no output root"))?;
    validate_exact_root_sentinel(parent)?;
    let campaign_id = directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("completed campaign id is not valid UTF-8"))?;
    validate_campaign_id(campaign_id)?;
    validate_campaign_directory(directory, campaign_id)
        .map_err(|_| anyhow!("campaign_completion_tampered"))?;
    let complete_path = directory.join(COMPLETE_SENTINEL);
    let metadata = match fs::symlink_metadata(&complete_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("campaign_not_complete")
        }
        Err(_) => bail!("campaign_completion_tampered"),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("campaign_completion_tampered");
    }
    let marker: CompleteSentinel =
        read_control_json(&complete_path).map_err(|_| anyhow!("campaign_completion_tampered"))?;
    if marker.schema != CAMPAIGN_COMPLETE_SCHEMA
        || marker.campaign_id != campaign_id
        || marker.completed_at_unix_millis == 0
        || marker.sealed_artifact.stage != ArtifactStage::Final
        || !is_sha256(&marker.sealed_artifact.sha256)
        || marker.sealed_artifact.bytes == 0
        || marker.sealed_artifact.bytes > MAX_SEALED_ARTIFACT_BYTES
    {
        bail!("campaign_completion_tampered");
    }
    let relative =
        validate_relative_artifact_path(Path::new(&marker.sealed_artifact.relative_path))
            .map_err(|_| anyhow!("campaign_completion_tampered"))?;
    let sealed_path = resolve_existing_file(directory, &relative)
        .map_err(|_| anyhow!("campaign_completion_tampered"))?;
    let sealed_bytes = read_regular_file_bounded(&sealed_path, marker.sealed_artifact.bytes)
        .map_err(|_| anyhow!("campaign_completion_tampered"))?;
    if sealed_bytes.len() as u64 != marker.sealed_artifact.bytes
        || digest(&sealed_bytes) != marker.sealed_artifact.sha256
    {
        bail!("campaign_completion_tampered");
    }
    Ok(marker.sealed_artifact)
}

fn validate_campaign_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_CAMPAIGN_ID_BYTES {
        bail!("campaign id is empty or too long");
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        bail!("campaign id is empty");
    };
    if !first.is_ascii_alphanumeric()
        || !chars.all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        || value == "."
        || value == ".."
    {
        bail!("campaign id contains unsafe characters");
    }
    Ok(())
}

fn validate_relative_artifact_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("artifact path must be a non-empty relative path");
    }
    let text = path
        .to_str()
        .ok_or_else(|| anyhow!("artifact path must be valid UTF-8"))?;
    if text.len() > MAX_RELATIVE_PATH_BYTES {
        bail!("artifact path exceeds its byte bound");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part_text = part
                    .to_str()
                    .ok_or_else(|| anyhow!("artifact path component must be valid UTF-8"))?;
                if part_text.is_empty()
                    || part_text.len() > MAX_PATH_COMPONENT_BYTES
                    || part_text == ".git"
                    || part_text.starts_with(INTERNAL_PREFIX)
                {
                    bail!("artifact path contains a reserved or oversized component");
                }
                normalized.push(part);
            }
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => bail!("artifact path contains traversal components"),
        }
    }
    if normalized.as_os_str().is_empty() || normalized.file_name().is_none() {
        bail!("artifact path has no filename");
    }
    Ok(normalized)
}

fn create_relative_directory_chain(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut cursor = base.to_owned();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("artifact parent path contains traversal components");
        };
        cursor.push(part);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("artifact parent contains a symbolic link or non-directory");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&cursor).context("creating artifact directory")?;
                let metadata = fs::symlink_metadata(&cursor)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("artifact directory creation was redirected");
                }
                let parent = cursor
                    .parent()
                    .ok_or_else(|| anyhow!("artifact directory has no parent"))?;
                sync_directory(parent)?;
            }
            Err(error) => return Err(error).context("inspecting artifact directory"),
        }
    }
    Ok(cursor)
}

fn resolve_existing_file(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut cursor = base.to_owned();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            bail!("artifact path contains traversal components");
        };
        cursor.push(part);
        let metadata = fs::symlink_metadata(&cursor).context("inspecting artifact path")?;
        if metadata.file_type().is_symlink() {
            bail!("artifact path contains a symbolic link");
        }
        if index + 1 == component_count {
            if !metadata.is_file() {
                bail!("artifact path is not a regular file");
            }
        } else if !metadata.is_dir() {
            bail!("artifact parent path is not a directory");
        }
    }
    Ok(cursor)
}

fn inspect_target_for_write(path: &Path, stage: ArtifactStage) -> Result<u64> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("artifact target is a symbolic link or non-regular file");
            }
            if stage == ArtifactStage::Final {
                bail!("final artifact already exists");
            }
            Ok(metadata.len())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).context("inspecting artifact target"),
    }
}

fn atomic_install(parent: &Path, file_name: &Path, bytes: &[u8], mode: InstallMode) -> Result<()> {
    if file_name.components().count() != 1 || file_name.file_name().is_none() {
        bail!("atomic artifact target must be a direct child filename");
    }
    let destination = parent.join(file_name);
    let (temporary_path, mut temporary) = create_unique_temporary(parent)?;
    let result = (|| -> Result<()> {
        temporary
            .write_all(bytes)
            .context("writing artifact temporary file")?;
        temporary
            .flush()
            .context("flushing artifact temporary file")?;
        set_file_private(&temporary_path)?;
        temporary
            .sync_all()
            .context("syncing artifact temporary file")?;
        drop(temporary);

        match mode {
            InstallMode::NoClobber => {
                // Linking a fully synced same-directory inode gives the final
                // name create-new semantics. Unlike a precheck followed by
                // rename, this cannot overwrite a racing destination.
                fs::hard_link(&temporary_path, &destination)
                    .context("installing no-clobber final artifact")?;
                sync_directory(parent)?;
                fs::remove_file(&temporary_path).context("removing artifact temporary link")?;
                sync_directory(parent)?;
            }
            InstallMode::Replace => {
                fs::rename(&temporary_path, &destination)
                    .context("atomically replacing partial artifact")?;
                sync_directory(parent)?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_unique_temporary(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("{INTERNAL_PREFIX}tmp-{}-{sequence}", std::process::id());
        let path = parent.join(name);
        match open_private_create_new(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("creating artifact temporary file"),
        }
    }
    bail!("could not allocate a unique artifact temporary file")
}

fn read_regular_file_bounded(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>> {
    let metadata_before = fs::symlink_metadata(path).context("inspecting bounded artifact")?;
    if metadata_before.file_type().is_symlink() || !metadata_before.is_file() {
        bail!("bounded artifact is not a regular file");
    }
    if metadata_before.len() > maximum_bytes {
        bail!("artifact exceeds the configured read bound");
    }
    let file = File::open(path).context("opening bounded artifact")?;
    let metadata_open = file.metadata().context("inspecting opened artifact")?;
    if !metadata_open.is_file() || !same_file(&metadata_before, &metadata_open) {
        bail!("artifact changed while it was being opened");
    }
    let take_limit = maximum_bytes
        .checked_add(1)
        .ok_or_else(|| anyhow!("artifact read bound overflow"))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata_open.len()).map_err(|_| anyhow!("artifact is too large"))?,
    );
    file.take(take_limit)
        .read_to_end(&mut bytes)
        .context("reading bounded artifact")?;
    if bytes.len() as u64 > maximum_bytes {
        bail!("artifact grew beyond the configured read bound");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
}

fn read_control_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = read_regular_file_bounded(path, MAX_CONTROL_BYTES as u64)?;
    serde_json::from_slice(&bytes).context("parsing bounded artifact control record")
}

fn measure_artifact_tree(root: &Path) -> Result<u64> {
    let mut stack = vec![root.to_owned()];
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut charged_files = HashSet::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).context("measuring campaign artifacts")? {
            let entry = entry?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| anyhow!("artifact entry count overflow"))?;
            if entries > MAX_TREE_ENTRIES {
                bail!("campaign has too many artifact entries");
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("campaign artifact tree contains a symbolic link");
            }
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let name = entry.file_name();
                // Control sentinels are fixed and bounded separately. Every
                // other regular file is charged, including interrupted
                // same-directory temporary files. Otherwise repeated process
                // crashes could grow disk outside the campaign budget.
                if name != OsStr::new(CAMPAIGN_SENTINEL)
                    && name != OsStr::new(COMPLETE_SENTINEL)
                    && name != OsStr::new(CAMPAIGN_LOCK)
                    && charged_files.insert(file_identity(&metadata, &path)?)
                {
                    total = total
                        .checked_add(metadata.len())
                        .ok_or_else(|| anyhow!("artifact byte accounting overflow"))?;
                }
            } else {
                bail!("campaign artifact tree contains a non-regular entry");
            }
        }
    }
    Ok(total)
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata, _path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata, path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).context("canonicalizing artifact for unique accounting")
}

fn tree_contains_internal_temporary(root: &Path) -> Result<bool> {
    let mut stack = vec![root.to_owned()];
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| anyhow!("campaign entry count overflow"))?;
            if entries > MAX_TREE_ENTRIES {
                bail!("campaign has too many entries to seal safely");
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                bail!("campaign artifact tree contains a symbolic link");
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file()
                && entry.file_name() != OsStr::new(CAMPAIGN_LOCK)
                && entry.file_name() != OsStr::new(CAMPAIGN_SENTINEL)
                && entry.file_name() != OsStr::new(COMPLETE_SENTINEL)
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(INTERNAL_PREFIX)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn tree_contains_symlink(root: &Path) -> Result<bool> {
    let mut stack = vec![root.to_owned()];
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() {
            return Ok(true);
        }
        if !metadata.is_dir() {
            return Ok(true);
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| anyhow!("retention tree entry count overflow"))?;
            if entries > MAX_TREE_ENTRIES {
                return Ok(true);
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Ok(true);
            }
            if metadata.is_dir() {
                stack.push(path);
            } else if !metadata.is_file() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn portable_relative_path(path: &Path) -> Result<String> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("artifact path is not valid UTF-8")),
            _ => Err(anyhow!("artifact path is not normalized and relative")),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(parts.join("/"))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("setting private artifact permissions")
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn open_private_create_new(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn open_private_create_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .context("syncing artifact directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    const BUDGET: u64 = 64 * 1024;

    struct Layout {
        _temp: TempDir,
        repo: PathBuf,
        output: PathBuf,
        workspace: PathBuf,
    }

    impl Layout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = dunce::canonicalize(temp.path()).unwrap();
            let repo = base.join("repo");
            let output = repo.join(DEFAULT_OUTPUT_RELATIVE_PATH);
            let workspace = base.join("scenario-workspace");
            fs::create_dir(&repo).unwrap();
            fs::create_dir(&workspace).unwrap();
            Self {
                _temp: temp,
                repo,
                output,
                workspace,
            }
        }

        fn root(&self) -> SafeOutputRoot {
            SafeOutputRoot::open(&self.output, &self.repo, Some(&self.workspace), BUDGET).unwrap()
        }
    }

    #[test]
    fn documented_repository_descendant_is_allowed_and_direct_campaigns_are_exclusive() {
        let layout = Layout::new();
        let root = layout.root();
        assert_eq!(root.path(), layout.output);
        assert_eq!(root.repository_root(), layout.repo);
        assert_eq!(root.active_workspace(), Some(layout.workspace.as_path()));
        assert_eq!(root.artifact_budget_bytes(), BUDGET);

        let campaign = root.create_campaign("campaign-001").unwrap();
        assert_eq!(campaign.path().parent(), Some(root.path()));
        assert!(root.create_campaign("campaign-001").is_err());
        assert!(root.create_campaign("../escape").is_err());
        assert!(root.create_campaign("nested/campaign").is_err());
    }

    #[test]
    fn concurrent_campaign_creation_has_one_owner_and_no_clobber() {
        let layout = Layout::new();
        let root = Arc::new(layout.root());
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    root.create_campaign("same-campaign")
                })
            })
            .collect();

        let mut owners = Vec::new();
        for worker in workers {
            if let Ok(Ok(campaign)) = worker.join() {
                owners.push(campaign);
            }
        }
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].campaign_id(), "same-campaign");
        assert!(root
            .path()
            .join("same-campaign")
            .join(CAMPAIGN_SENTINEL)
            .is_file());
    }

    #[test]
    fn dangerous_roots_and_workspace_overlap_are_rejected() {
        let layout = Layout::new();
        assert!(SafeOutputRoot::open(&layout.repo, &layout.repo, None, BUDGET).is_err());
        assert!(
            SafeOutputRoot::open(layout.repo.parent().unwrap(), &layout.repo, None, BUDGET)
                .is_err()
        );
        assert!(SafeOutputRoot::open(Path::new("/"), &layout.repo, None, BUDGET).is_err());
        assert!(SafeOutputRoot::open(
            layout.repo.join(".git").join("cert"),
            &layout.repo,
            None,
            BUDGET
        )
        .is_err());
        assert!(SafeOutputRoot::open(
            layout.repo.join("evals").join("..").join("runs"),
            &layout.repo,
            None,
            BUDGET
        )
        .is_err());
        assert!(SafeOutputRoot::open(
            &layout.output,
            &layout.repo,
            Some(&layout.output.join("workspace")),
            BUDGET
        )
        .is_err());
        assert!(SafeOutputRoot::open(&layout.output, &layout.repo, None, 0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn root_and_artifact_symlink_components_are_rejected() {
        use std::os::unix::fs::symlink;

        let layout = Layout::new();
        let real = layout.repo.join("real-output");
        fs::create_dir(&real).unwrap();
        let linked = layout.repo.join("linked-output");
        symlink(&real, &linked).unwrap();
        assert!(SafeOutputRoot::open(&linked, &layout.repo, None, BUDGET).is_err());

        let root = layout.root();
        let campaign = root.create_campaign("symlink-paths").unwrap();
        let outside = layout.repo.parent().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, campaign.path().join("redirect")).unwrap();
        assert!(campaign.write_final("redirect/leak.json", b"{}").is_err());

        fs::write(outside.join("secret.json"), b"secret").unwrap();
        symlink(
            outside.join("secret.json"),
            campaign.path().join("linked.json"),
        )
        .unwrap();
        assert!(campaign.bounded_read("linked.json", 64).is_err());
    }

    #[test]
    fn relative_paths_reject_escapes_reserved_names_and_absolute_paths() {
        let layout = Layout::new();
        let campaign = layout.root().create_campaign("paths").unwrap();
        assert!(campaign.write_final("../escape.json", b"{}").is_err());
        assert!(campaign
            .write_final("nested/../../escape.json", b"{}")
            .is_err());
        assert!(campaign.write_final("/absolute.json", b"{}").is_err());
        assert!(campaign.write_final(".git/config", b"{}").is_err());
        assert!(campaign
            .write_final(".grokptah-cert-complete.json", b"{}")
            .is_err());
        let oversized = format!("{}.json", "x".repeat(MAX_PATH_COMPONENT_BYTES + 1));
        assert!(campaign.write_final(oversized, b"{}").is_err());
    }

    #[test]
    fn final_writes_are_no_clobber_private_bounded_and_digestible() {
        let layout = Layout::new();
        let campaign = layout.root().create_campaign("atomic-final").unwrap();
        let artifact = campaign
            .write_final("reports/final.json", br#"{"safe":true}"#)
            .unwrap();
        assert_eq!(artifact.relative_path, "reports/final.json");
        assert_eq!(artifact.bytes, 13);
        assert!(is_sha256(&artifact.sha256));
        assert_eq!(artifact.stage, ArtifactStage::Final);
        assert_eq!(
            campaign.bounded_read("reports/final.json", 13).unwrap(),
            br#"{"safe":true}"#
        );
        assert!(campaign.bounded_read("reports/final.json", 12).is_err());
        assert!(campaign
            .write_final("reports/final.json", b"replacement")
            .is_err());
        campaign
            .verify("reports/final.json", artifact.bytes, &artifact.sha256)
            .unwrap();
        assert!(campaign
            .verify("reports/final.json", artifact.bytes, &"0".repeat(64),)
            .is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(campaign.path().join("reports/final.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(fs::read_dir(campaign.path().join("reports"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(INTERNAL_PREFIX)));
    }

    #[test]
    fn byte_budget_accounts_for_partial_replacement_and_oversize_reads() {
        let layout = Layout::new();
        let root = SafeOutputRoot::open(&layout.output, &layout.repo, None, 16).unwrap();
        let campaign = root.create_campaign("budget").unwrap();
        campaign
            .write_partial("state.partial", b"12345678")
            .unwrap();
        assert_eq!(campaign.used_bytes().unwrap(), 8);
        // Replacements are charged at their transient old+new disk use.
        assert!(campaign
            .write_partial("state.partial", b"abcdefghijkl")
            .is_err());
        assert_eq!(
            campaign.bounded_read("state.partial", 8).unwrap(),
            b"12345678"
        );
        campaign.write_partial("state.partial", b"1234").unwrap();
        assert_eq!(campaign.used_bytes().unwrap(), 4);
        campaign.write_final("other", b"123456789012").unwrap();
        assert_eq!(campaign.used_bytes().unwrap(), 16);
        assert!(campaign.write_final("overflow", b"x").is_err());
        assert!(campaign.bounded_read("other", 17).is_err());
    }

    #[test]
    fn unicode_is_preserved_whole_and_never_byte_truncated() {
        let layout = Layout::new();
        let campaign = layout.root().create_campaign("unicode").unwrap();
        let value = "agent=🦀; state=再接続".as_bytes();
        campaign
            .write_partial("evidence/再接続.partial", value)
            .unwrap();
        assert!(campaign
            .bounded_read("evidence/再接続.partial", value.len() as u64 - 1)
            .is_err());
        assert_eq!(
            campaign
                .bounded_read("evidence/再接続.partial", value.len() as u64)
                .unwrap(),
            value
        );
    }

    #[test]
    fn torn_temporary_output_is_not_mistaken_for_a_final_and_resume_is_bounded() {
        let layout = Layout::new();
        let root = layout.root();
        let campaign = root.create_campaign("interrupted").unwrap();
        campaign
            .write_partial("report.partial.json", b"recoverable")
            .unwrap();
        fs::write(
            campaign.path().join(format!("{INTERNAL_PREFIX}tmp-torn")),
            b"torn",
        )
        .unwrap();
        assert!(campaign.bounded_read("report.final.json", 64).is_err());
        drop(campaign);

        let resumed = root.resume_campaign("interrupted").unwrap();
        assert_eq!(resumed.used_bytes().unwrap(), 15);
        assert_eq!(
            resumed.bounded_read("report.partial.json", 11).unwrap(),
            b"recoverable"
        );
        assert!(root.resume_campaign("missing").is_err());
    }

    #[test]
    fn repeated_torn_temporaries_are_charged_and_can_exhaust_the_budget() {
        let layout = Layout::new();
        let root = SafeOutputRoot::open(&layout.output, &layout.repo, None, 12).unwrap();
        let campaign = root.create_campaign("many-torn").unwrap();
        for index in 0..3 {
            fs::write(
                campaign
                    .path()
                    .join(format!("{INTERNAL_PREFIX}tmp-torn-{index}")),
                b"torn",
            )
            .unwrap();
        }
        drop(campaign);

        let resumed = root.resume_campaign("many-torn").unwrap();
        assert_eq!(resumed.used_bytes().unwrap(), 12);
        assert!(resumed.write_partial("checkpoint", b"x").is_err());

        fs::write(
            resumed
                .path()
                .join(format!("{INTERNAL_PREFIX}tmp-torn-overflow")),
            b"x",
        )
        .unwrap();
        drop(resumed);
        assert!(root.resume_campaign("many-torn").is_err());
    }

    #[test]
    fn campaign_has_one_exclusive_lifetime_and_completion_seals_it() {
        let layout = Layout::new();
        let root = layout.root();
        let campaign = root.create_campaign("exclusive").unwrap();
        let partial = campaign
            .write_partial("checkpoint.json", b"partial")
            .unwrap();
        let forged_final = ArtifactDigest {
            stage: ArtifactStage::Final,
            ..partial
        };
        assert!(campaign.mark_complete(&forged_final).is_err());
        let sealed = campaign.write_final("report.json", b"sealed").unwrap();
        assert!(root.resume_campaign("exclusive").is_err());
        campaign.mark_complete(&sealed).unwrap();
        let verified = verify_completed_campaign(campaign.path()).unwrap();
        assert_eq!(verified, sealed);
        assert!(campaign.write_partial("late.partial", b"late").is_err());
        assert!(campaign.mark_complete(&sealed).is_err());
        drop(campaign);
        assert!(root.resume_campaign("exclusive").is_err());
    }

    #[test]
    fn completed_campaign_verification_distinguishes_incomplete_from_tampered() {
        let layout = Layout::new();
        let root = layout.root();
        let incomplete = root.create_campaign("not-complete").unwrap();
        let incomplete_error = verify_completed_campaign(incomplete.path()).unwrap_err();
        assert_eq!(incomplete_error.to_string(), "campaign_not_complete");
        drop(incomplete);

        let sealed = root.create_campaign("tampered").unwrap();
        let report = sealed.write_final("report.json", b"sealed").unwrap();
        sealed.mark_complete(&report).unwrap();
        fs::write(sealed.path().join("report.json"), b"changed").unwrap();
        let tamper_error = verify_completed_campaign(sealed.path()).unwrap_err();
        assert_eq!(tamper_error.to_string(), "campaign_completion_tampered");
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_no_clobber_hardlinks_are_charged_once_but_block_sealing() {
        let layout = Layout::new();
        let root = layout.root();
        let campaign = root.create_campaign("hardlink-recovery").unwrap();
        campaign.write_final("final.json", b"one-inode").unwrap();
        fs::hard_link(
            campaign.path().join("final.json"),
            campaign
                .path()
                .join(format!("{INTERNAL_PREFIX}tmp-interrupted-link")),
        )
        .unwrap();
        drop(campaign);

        let resumed = root.resume_campaign("hardlink-recovery").unwrap();
        assert_eq!(resumed.used_bytes().unwrap(), 9);
        let sealed = ArtifactDigest {
            relative_path: "final.json".into(),
            sha256: digest(b"one-inode"),
            bytes: 9,
            stage: ArtifactStage::Final,
        };
        assert!(resumed.mark_complete(&sealed).is_err());
    }

    #[test]
    fn malformed_or_unknown_root_sentinel_fails_closed() {
        let layout = Layout::new();
        fs::create_dir_all(&layout.output).unwrap();
        fs::write(layout.output.join(ROOT_SENTINEL), b"{torn").unwrap();
        assert!(SafeOutputRoot::open(&layout.output, &layout.repo, None, BUDGET).is_err());

        let other = layout.repo.join("other-output");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("unknown"), b"leave me").unwrap();
        assert!(SafeOutputRoot::open(&other, &layout.repo, None, BUDGET).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn retention_prunes_only_completed_marker_owned_safe_direct_children() {
        use std::os::unix::fs::symlink;

        let layout = Layout::new();
        let root = layout.root();
        let first = root.create_campaign("first").unwrap();
        let first_sealed = first.write_final("report.json", b"first").unwrap();
        first.mark_complete(&first_sealed).unwrap();
        let first_path = first.path().to_path_buf();
        drop(first);
        let second = root.create_campaign("second").unwrap();
        let second_sealed = second.write_final("report.json", b"second").unwrap();
        second.mark_complete(&second_sealed).unwrap();
        let second_path = second.path().to_path_buf();
        drop(second);
        let current = root.create_campaign("current").unwrap();
        let current_sealed = current.write_final("report.json", b"current").unwrap();
        current.mark_complete(&current_sealed).unwrap();
        let current_path = current.path().to_path_buf();
        drop(current);

        let incomplete = root.create_campaign("incomplete").unwrap();
        incomplete.write_partial("report.partial", b"keep").unwrap();
        let linked = root.create_campaign("linked").unwrap();
        let linked_sealed = linked.write_final("report.json", b"linked").unwrap();
        linked.mark_complete(&linked_sealed).unwrap();
        symlink(&layout.workspace, linked.path().join("workspace-link")).unwrap();
        let linked_path = linked.path().to_path_buf();
        drop(linked);

        let malformed = root.path().join("malformed");
        fs::create_dir(&malformed).unwrap();
        fs::write(malformed.join(CAMPAIGN_SENTINEL), b"{torn").unwrap();
        fs::create_dir(root.path().join("unknown")).unwrap();
        fs::write(root.path().join("loose-file"), b"unknown").unwrap();
        symlink(&layout.workspace, root.path().join("linked-child")).unwrap();

        assert!(root.prune_completed(0, Some("current")).is_err());
        let summary = root.prune_completed(1, Some("current")).unwrap();
        assert_eq!(summary.removed, 1);
        assert!(!first_path.exists());
        assert!(second_path.exists());
        assert!(current_path.exists());
        assert!(incomplete.path().exists());
        assert!(linked_path.exists());
        assert!(malformed.exists());
        assert!(root.path().join("unknown").exists());
        assert!(root.path().join("loose-file").exists());
        assert!(root.path().join("linked-child").exists());
        assert!(summary.preserved >= 7);
    }
}
