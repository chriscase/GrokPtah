//! Adversarial tests for the failure modes the first two passes named but did
//! not exercise: a volume with no space left, a coordinator that dies holding
//! a lease, secrets leaking into durable or public surfaces, and the evidence
//! bundle that says what was actually proved.
//!
//! Two of these run real child processes. That is the point: a lease exists to
//! survive the death of the process holding it, and a test that only ever has
//! one process cannot observe that.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::SealStamp;
use grokptah_agent_bridge::orchestration::{
    AcceptanceIntent, AttemptLeaseState, AuthContext, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, SealedBounds, WorkspaceAllowlist, ACCEPTANCE_INTENT_VERSION,
    PROJECTION_VERSION, SEAL_VERSION, TOMBSTONE_VERSION,
};
use grokptah_agent_bridge::{
    safe_id_filename, set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig,
    RunExecutionMode, RunState, SessionKind,
};
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN: &str = "durability-p2-bearer-do-not-leak";

/// Environment switches read by the child-process helpers at the bottom of
/// this file. A child is this same test binary re-invoked with one ignored
/// test selected, which keeps the cross-process cases in the same crate as
/// the invariants they exercise.
const CHILD_MODE: &str = "GROKPTAH_P2_CHILD_MODE";
const CHILD_STORE: &str = "GROKPTAH_P2_CHILD_STORE";
const CHILD_RUN: &str = "GROKPTAH_P2_CHILD_RUN";
const CHILD_TTL_MS: &str = "GROKPTAH_P2_CHILD_TTL_MS";
const CHILD_ATTEMPT: &str = "GROKPTAH_P2_CHILD_ATTEMPT";
const CHILD_OWNER: &str = "GROKPTAH_P2_CHILD_OWNER";
const CHILD_SEED_INTENT: &str = "GROKPTAH_P2_CHILD_SEED_INTENT";
const CHILD_CLEAR_FENCE: &str = "GROKPTAH_P2_CHILD_CLEAR_FENCE";

struct Rig {
    home: TempDir,
    ws: TempDir,
    _env: ProcessEnvGuard,
    host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
    session: Uuid,
}

impl Rig {
    async fn new() -> Self {
        Self::with_store_root(None).await
    }

    /// Build a rig whose ledger lives at `store_root` when given, so a test
    /// can put the ledger on a volume it controls.
    async fn with_store_root(store_root: Option<PathBuf>) -> Self {
        let mut env = ProcessEnvGuard::new();
        let home = tempdir().unwrap();
        let grokptah_home = home.path().join(".grokptah");
        std::fs::create_dir_all(&grokptah_home).unwrap();
        set_grokptah_home_override(Some(grokptah_home));
        env.set("GROKPTAH_AGENT_OFFLINE", "1");

        let ws = tempdir().unwrap();
        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        let root = store_root.unwrap_or_else(|| home.path().join("orch"));
        let orch = build_service(&host, &root, ws.path()).await;
        Self {
            home,
            ws,
            _env: env,
            host,
            orch,
            session: session.id,
        }
    }

    fn auth(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN}")))
            .unwrap()
    }

    fn store_path(&self) -> PathBuf {
        self.orch.store().root().to_path_buf()
    }

    /// Bring the coordinator back against the same ledger, as a restart does.
    async fn restart(self) -> Self {
        let Rig {
            home,
            ws,
            _env,
            host,
            orch,
            session,
        } = self;
        let root = orch.store().root().to_path_buf();
        drop(orch);
        drop(host);
        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        host.session_set_cwd(session, ws.path()).unwrap();
        let orch = build_service(&host, &root, ws.path()).await;
        Self {
            home,
            ws,
            _env,
            host,
            orch,
            session,
        }
    }

    fn markers(&self) -> Vec<String> {
        std::fs::read_to_string(self.ws.path().join("ledger.txt"))
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

async fn start_host() -> AgentHostHandle {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        match host.start() {
            Ok(()) => return host,
            Err(error) if std::time::Instant::now() < deadline => {
                drop(host);
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("host never started: {error}"),
        }
    }
}

async fn open_store(root: &Path) -> OrchStore {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match OrchStore::open(root) {
            Ok(store) => return store,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("store never opened: {error}"),
        }
    }
}

async fn build_service(
    host: &AgentHostHandle,
    store_root: &Path,
    ws: &Path,
) -> Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        open_store(store_root).await,
        OrchestrationConfig {
            bearer_token: TOKEN.to_string(),
            allowlist: WorkspaceAllowlist::new([ws.to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds {
                max_prompt_bytes: 50_000,
                max_rounds: 4,
                max_duration_ms: 30_000,
            },
        },
    )
}

fn marker_prompt(marker: &str) -> String {
    format!("run printf '{marker}\\n' >> ledger.txt")
}

async fn wait_for<F>(label: &str, timeout: Duration, mut ready: F)
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    loop {
        if ready() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {label}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Every regular file under `root`, recursively.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

// ── a volume with no space left ────────────────────────────────────────

/// A guard that unmounts and releases a loop-mounted scratch filesystem.
struct TinyVolume {
    mount: PathBuf,
    image: PathBuf,
    _dir: TempDir,
}

impl TinyVolume {
    /// A real filesystem small enough to fill, when this host allows one.
    ///
    /// Returns `None` on any host that cannot create loop mounts, which is
    /// most CI. The caller then falls back to an unwritable ledger, which
    /// reaches the same `io::Error` on the same write path.
    fn create(megabytes: u64) -> Option<Self> {
        let dir = tempdir().ok()?;
        let image = dir.path().join("ledger.img");
        let mount = dir.path().join("mnt");
        std::fs::create_dir_all(&mount).ok()?;
        let dd = Command::new("dd")
            .arg("if=/dev/zero")
            .arg(format!("of={}", image.display()))
            .arg("bs=1M")
            .arg(format!("count={megabytes}"))
            .output()
            .ok()?;
        if !dd.status.success() {
            return None;
        }
        if !Command::new("mkfs.ext2")
            .args(["-q", "-F"])
            .arg(&image)
            .status()
            .ok()?
            .success()
        {
            return None;
        }
        if !Command::new("mount")
            .args(["-o", "loop"])
            .arg(&image)
            .arg(&mount)
            .status()
            .ok()?
            .success()
        {
            return None;
        }
        Some(Self {
            mount,
            image,
            _dir: dir,
        })
    }

    /// Consume the free space, leaving a filesystem that accepts no new bytes.
    fn fill(&self) {
        let ballast = self.mount.join("ballast");
        let Ok(mut file) = std::fs::File::create(&ballast) else {
            return;
        };
        let block = vec![0u8; 64 * 1024];
        while file.write_all(&block).is_ok() {}
        let _ = file.flush();
    }
}

impl Drop for TinyVolume {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg(&self.mount).status();
        let _ = std::fs::remove_file(&self.image);
    }
}

/// A ledger volume with no space left refuses the admission outright. It never
/// half-decides: nothing executes, then or after any number of restarts.
///
/// On a host that permits loop mounts this is a genuine `ENOSPC`. Everywhere
/// else the ledger directory is made unwritable, which reaches the same
/// `io::Error` on the same write path — the injection differs, the invariant
/// under test does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_ledger_volume_with_no_space_left_refuses_rather_than_half_deciding() {
    let volume = TinyVolume::create(8);
    let (rig, injection) = match &volume {
        Some(volume) => {
            let root = volume.mount.join("orch");
            let rig = Rig::with_store_root(Some(root)).await;
            volume.fill();
            (rig, "real ENOSPC on a loop-mounted volume")
        }
        None => {
            let rig = Rig::new().await;
            let root = rig.store_path();
            // Every private ledger write lands in one of these; taking write
            // permission away from all of them is the portable stand-in.
            for ledger in ["inputs", "idempotency", "leases", "tombstones"] {
                let path = root.join(ledger);
                if path.is_dir() {
                    let mut perms = std::fs::metadata(&path).unwrap().permissions();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        perms.set_mode(0o500);
                    }
                    #[cfg(not(unix))]
                    perms.set_readonly(true);
                    std::fs::set_permissions(&path, perms).unwrap();
                }
            }
            (rig, "an unwritable ledger directory")
        }
    };

    let auth = rig.auth();
    let error = rig
        .orch
        .submit_task(
            &auth,
            "no-space-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("no-space-marker"),
            None,
        )
        .await
        .expect_err(&format!(
            "a submission the ledger cannot record must fail ({injection})"
        ));
    assert!(
        !error.message.is_empty(),
        "the refusal must name a reason: {error:?}"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rig.markers().is_empty(),
        "work executed off a ledger that could not record it: {:?}",
        rig.markers()
    );

    // Now heal the volume. This is the case that matters and the one a
    // single-shot assertion misses: on a full disk the coordinator may not be
    // able to write the tombstone that records its own refusal, so a `Queued`
    // run can survive on disk with no sealed input behind it. Recovery must
    // read that as "never admitted", not as "work to pick up" — otherwise a
    // refused submission becomes executable by waiting for space.
    heal(&volume, &rig.store_path());

    let mut rig = rig;
    for pass in 0..3 {
        rig = rig.restart().await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            rig.markers().is_empty(),
            "a refused admission executed on recovery pass {pass} ({injection}): {:?}",
            rig.markers()
        );
        for run in rig.orch.store().list_runs().unwrap_or_default() {
            if run.request_id == "no-space-request" {
                assert!(
                    run.state.is_terminal(),
                    "recovery left a refused admission dispatchable: {:?}",
                    run.state
                );
            }
        }
    }
    set_grokptah_home_override(None);
}

/// Undo whichever injection the no-space test used, so its recovery passes
/// run against a ledger that works again.
fn heal(volume: &Option<TinyVolume>, store_root: &Path) {
    match volume {
        Some(volume) => {
            let _ = std::fs::remove_file(volume.mount.join("ballast"));
        }
        None => {
            for ledger in ["inputs", "idempotency", "leases", "tombstones"] {
                let path = store_root.join(ledger);
                if path.is_dir() {
                    let mut perms = std::fs::metadata(&path).unwrap().permissions();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        perms.set_mode(0o700);
                    }
                    #[cfg(not(unix))]
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(&path, perms);
                }
            }
        }
    }
}

// ── a coordinator that dies holding a lease ────────────────────────────

fn child_command(mode: &str, store: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test binary path"));
    command
        .args(["--exact", "--ignored", "--nocapture"])
        .arg(child_test_name(mode))
        // Piped explicitly: `output()` would do this itself, but a child that
        // is `spawn()`ed and read with `wait_with_output` inherits this
        // process's stdout instead and reports nothing back.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env(CHILD_MODE, mode)
        .env(CHILD_STORE, store);
    command
}

fn child_test_name(mode: &str) -> &'static str {
    match mode {
        "fence-and-die" => "child_fences_a_running_attempt_and_dies",
        "queue-and-die" => "child_leases_a_queued_run_and_dies",
        "recover-and-hold" => "child_recovers_and_takes_the_run",
        "try-renew" => "child_tries_to_renew",
        "open-only" => "child_tries_to_open",
        other => panic!("unknown child mode {other}"),
    }
}

/// Read the single `P2CHILD {...}` line a child helper prints.
fn child_report(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("P2CHILD "))
        .unwrap_or_else(|| {
            panic!(
                "child printed no report\nstdout={stdout}\nstderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    serde_json::from_str(line).expect("child report is JSON")
}

/// A fence outlives the process that set it.
///
/// A coordinator killed mid-run leaves work it never observed stop. Exclusive
/// ledger ownership proves *that process* is gone; it proves nothing about the
/// worker it spawned. So a fenced attempt keeps its lease across the death of
/// the coordinator that fenced it, is not terminalized by recovery, and is not
/// handed to a successor — until an operator clears the fence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fenced_attempt_survives_the_death_of_the_coordinator_that_fenced_it() {
    let home = tempdir().unwrap();
    let root = home.path().join("orch");
    let run_id = "cross-process-fenced";

    let seeded = child_report(
        &child_command("fence-and-die", &root)
            .env(CHILD_RUN, run_id)
            .env(CHILD_TTL_MS, "600000")
            .output()
            .expect("spawn the fencing child"),
    );
    assert_eq!(seeded["acquired"], true, "child report: {seeded}");
    let dead_attempt = seeded["attemptId"].as_str().unwrap().to_string();
    let dead_owner = seeded["ownerId"].as_str().unwrap().to_string();

    // A successor coordinator opens the ledger the dead one held.
    let store = OrchStore::open(&root).expect("successor opens the ledger");
    let lease = store
        .load_attempt_lease(run_id)
        .unwrap()
        .expect("the fenced attempt keeps its lease");
    assert_eq!(
        lease.state,
        AttemptLeaseState::Held,
        "recovery released a lease it never proved was free"
    );
    assert_eq!(lease.attempt_id, dead_attempt);
    assert!(
        store.load_teardown_uncertain(run_id).unwrap().is_some(),
        "the fence must survive the process that set it"
    );
    let run = store.load_run(run_id).unwrap().expect("the run is durable");
    assert!(
        !run.state.is_terminal(),
        "a fenced run must be neither recovered nor tombstoned: {:?}",
        run.state
    );
    // And nobody may take it, however long they wait: the fence is not a TTL.
    let blocked = store.reclaim_expired_attempt_lease(run_id).unwrap();
    assert!(
        blocked.is_none(),
        "a fenced attempt was reclaimed: {blocked:?}"
    );

    // Clearing the fence is the operator disposition that lets recovery act.
    assert!(store.clear_teardown_uncertain(run_id).unwrap());
    drop(store);
    let store = OrchStore::open(&root).expect("successor reopens the ledger");
    let run = store.load_run(run_id).unwrap().expect("the run is durable");
    assert!(
        run.state.is_terminal(),
        "an unfenced interrupted run must be terminalized: {:?}",
        run.state
    );
    let lease = store.load_attempt_lease(run_id).unwrap().unwrap();
    assert_eq!(
        lease.state,
        AttemptLeaseState::Released,
        "a terminal run's lease must be released"
    );
    drop(store);

    // The superseded attempt comes back as a live process and tries to
    // heartbeat its way back in.
    let zombie = child_report(
        &child_command("try-renew", &root)
            .env(CHILD_RUN, run_id)
            .env(CHILD_ATTEMPT, &dead_attempt)
            .env(CHILD_OWNER, &dead_owner)
            .output()
            .expect("spawn the returning child"),
    );
    assert_eq!(
        zombie["renewed"], false,
        "a released attempt renewed its lease from another process: {zombie}"
    );
}

/// An unfenced lease is released across a coordinator death only when the
/// run's own durable cut proves nothing could be behind it — and then exactly
/// one successor takes the run.
///
/// `Queued` is that cut: the start gate never opened, so no worker began. The
/// advisory lock is what proves the previous coordinator is gone; the cut is
/// what proves its work is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unstarted_attempt_is_reclaimed_by_exactly_one_successor() {
    let home = tempdir().unwrap();
    let root = home.path().join("orch");
    let run_id = "cross-process-unstarted";

    let seeded = child_report(
        &child_command("queue-and-die", &root)
            .env(CHILD_RUN, run_id)
            .env(CHILD_TTL_MS, "600000")
            .output()
            .expect("spawn the queueing child"),
    );
    assert_eq!(seeded["acquired"], true, "child report: {seeded}");
    let dead_attempt = seeded["attemptId"].as_str().unwrap().to_string();
    let dead_owner = seeded["ownerId"].as_str().unwrap().to_string();
    assert_eq!(seeded["attempt"], 1);

    // Four successor coordinators race for the run. Each opens the ledger,
    // which is serialized by the exclusive advisory lock, so the race is over
    // the lease rather than over the file.
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            child_command("recover-and-hold", &root)
                .env(CHILD_RUN, run_id)
                .env(CHILD_TTL_MS, "600000")
                .spawn()
                .expect("spawn a racing child"),
        );
    }
    let reports: Vec<serde_json::Value> = children
        .into_iter()
        .map(|child| child_report(&child.wait_with_output().expect("racing child")))
        .collect();
    let attempts: Vec<u64> = reports
        .iter()
        .filter(|report| report["acquired"] == true)
        .filter_map(|report| report["attempt"].as_u64())
        .collect();
    assert_eq!(
        attempts.len(),
        reports.len(),
        "every successor should have been able to recover an unstarted run: {reports:?}"
    );
    let mut sorted = attempts.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        attempts.len(),
        "two successors minted the same attempt number: {attempts:?}"
    );
    assert!(
        attempts.iter().all(|attempt| *attempt > 1),
        "a successor reused the dead coordinator's attempt number: {attempts:?}"
    );

    // Whatever order they ran in, the superseded first attempt is finished.
    let zombie = child_report(
        &child_command("try-renew", &root)
            .env(CHILD_RUN, run_id)
            .env(CHILD_ATTEMPT, &dead_attempt)
            .env(CHILD_OWNER, &dead_owner)
            .output()
            .expect("spawn the returning child"),
    );
    assert_eq!(
        zombie["renewed"], false,
        "a superseded attempt renewed its lease from another process: {zombie}"
    );
}

/// Two coordinator processes cannot hold the same ledger at once. This is the
/// bound that makes the single-writer assumption the rest of the durability
/// design rests on true rather than hoped for, so it is asserted rather than
/// assumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_coordinator_process_cannot_open_a_held_ledger() {
    let home = tempdir().unwrap();
    let root = home.path().join("orch");
    let store = OrchStore::open(&root).expect("first coordinator opens the ledger");

    let blocked = child_report(
        &child_command("open-only", &root)
            .output()
            .expect("spawn the second coordinator"),
    );
    assert_eq!(
        blocked["opened"], false,
        "a second process opened a ledger another process holds: {blocked}"
    );

    drop(store);
    let released = child_report(
        &child_command("open-only", &root)
            .output()
            .expect("spawn the second coordinator again"),
    );
    assert_eq!(
        released["opened"], true,
        "the ledger stayed locked after its holder went away: {released}"
    );
}

/// A terminal run never keeps executable input across a restart, whichever
/// path terminalized it — and a fenced one always does.
///
/// The leftover input is not dispatchable: nothing re-admits a terminal run.
/// It is the private prompt, though, and keeping it past the work it belongs
/// to is a retention leak. The fenced case is the deliberate exception,
/// because an attempt whose outcome is unknown may still need reconciling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_run_keeps_no_input_across_a_restart_but_a_fenced_one_does() {
    let home = tempdir().unwrap();
    let root = home.path().join("orch");

    let settled = child_report(
        &child_command("fence-and-die", &root)
            .env(CHILD_RUN, "p2-settled")
            .env(CHILD_TTL_MS, "600000")
            .env(CHILD_SEED_INTENT, "1")
            .env(CHILD_CLEAR_FENCE, "1")
            .output()
            .expect("spawn the settled child"),
    );
    assert_eq!(settled["acquired"], true, "child report: {settled}");
    let fenced = child_report(
        &child_command("fence-and-die", &root)
            .env(CHILD_RUN, "p2-fenced")
            .env(CHILD_TTL_MS, "600000")
            .env(CHILD_SEED_INTENT, "1")
            .output()
            .expect("spawn the fenced child"),
    );
    assert_eq!(fenced["acquired"], true, "child report: {fenced}");

    // Both children seeded input; the reports say so rather than the files,
    // because the *second* child opening the ledger already runs the recovery
    // pass under test against the first run.
    assert_eq!(settled["intentSeeded"], true, "child report: {settled}");
    assert_eq!(fenced["intentSeeded"], true, "child report: {fenced}");
    let input = |run_id: &str| {
        root.join("inputs")
            .join(format!("{}.json", safe_id_filename(run_id).unwrap()))
    };
    assert!(
        input("p2-fenced").is_file(),
        "a fence must hold the input through another coordinator's recovery"
    );

    // A successor coordinator opens the ledger the dead one held.
    let store = OrchStore::open(&root).expect("successor opens the ledger");
    assert!(
        store
            .load_run("p2-settled")
            .unwrap()
            .unwrap()
            .state
            .is_terminal(),
        "an unfenced interrupted run must be terminalized"
    );
    assert!(
        !input("p2-settled").is_file(),
        "a terminal, unfenced run kept its private input across a restart"
    );
    assert!(
        store
            .load_teardown_uncertain("p2-fenced")
            .unwrap()
            .is_some(),
        "the fence must survive the process that set it"
    );
    assert!(
        input("p2-fenced").is_file(),
        "a fenced run must keep its input until the fence is lifted"
    );
}

// ── secrets ────────────────────────────────────────────────────────────

/// Nothing durable and nothing public carries the bearer token, the sealing
/// key, or the private prompt body.
///
/// The two deliberate exceptions are named rather than skipped: the key store
/// holds the key (that is what it is for) and the sealed acceptance intent
/// holds the prompt (that is the private input the whole ledger exists to
/// protect). Both are asserted owner-only in the same pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn no_durable_or_public_surface_ever_carries_a_secret() {
    let rig = Rig::new().await;
    let auth = rig.auth();
    // The marker sits past the 120-character preview bound, so finding it
    // anywhere means something copied the prompt body, not the preview.
    let secret = "SUPERSECRETPROMPTBODY7f3a9c";
    let prompt = format!(
        "{} # {}",
        marker_prompt("secret-scan"),
        format_args!("{:pad$}{secret}", "", pad = 160)
    );
    let accepted = rig
        .orch
        .submit_task(
            &auth,
            "secret-scan-request",
            rig.session,
            rig.ws.path(),
            prompt,
            None,
        )
        .await
        .expect("submission is accepted");
    let run_id = accepted["runId"].as_str().unwrap().to_string();
    wait_for("the run to finish", Duration::from_secs(30), || {
        rig.orch
            .get_run(&auth, &run_id)
            .ok()
            .and_then(|value| serde_json::from_value::<RunState>(value["state"].clone()).ok())
            .map(|state| state.is_terminal())
            .unwrap_or(false)
    })
    .await;

    let root = rig.store_path();
    let key_bytes = std::fs::read_to_string(root.join("keys").join("authority.json"))
        .expect("the key store exists");
    let key_material: Vec<String> = serde_json::from_str::<serde_json::Value>(&key_bytes)
        .ok()
        .and_then(|value| value["keys"].as_object().cloned())
        .map(|keys| {
            keys.values()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !key_material.is_empty(),
        "the key store must actually hold key material for this scan to mean anything"
    );

    for path in walk_files(&root) {
        let relative = path.strip_prefix(&root).unwrap().to_path_buf();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let in_key_store = relative.starts_with("keys");
        let in_sealed_input = relative.starts_with("inputs");

        assert!(
            !text.contains(TOKEN),
            "the bearer token reached a durable record: {}",
            relative.display()
        );
        for key in &key_material {
            assert!(
                !text.contains(key.as_str()) || in_key_store,
                "sealing key material reached {}",
                relative.display()
            );
        }
        assert!(
            !text.contains(secret) || in_sealed_input,
            "the private prompt body reached {}",
            relative.display()
        );

        // Anything that can carry either is owner-only, no-follow, unaliased.
        #[cfg(unix)]
        if in_key_store || in_sealed_input {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert!(
                metadata.is_file(),
                "{} is not a regular file",
                relative.display()
            );
            assert_eq!(
                metadata.mode() & 0o077,
                0,
                "{} is readable beyond its owner",
                relative.display()
            );
            assert_eq!(metadata.nlink(), 1, "{} is aliased", relative.display());
        }
    }

    // The public surfaces the coordinator serves.
    // Two classes of surface, with deliberately different rules.
    //
    // The admission surfaces summarize the work: they must never carry the
    // input at all. The run-scoped event journal is a transcript delivered to
    // the authorized owner of that run, and an agent's own tool call
    // legitimately echoes the command it ran — so the prompt body may appear
    // there. What must never appear on *either* is a control-plane secret.
    let mut summaries = vec![
        (
            "get_run",
            rig.orch.get_run(&auth, &run_id).unwrap().to_string(),
        ),
        (
            "get_capacity",
            rig.orch.get_capacity(&auth).unwrap().to_string(),
        ),
        (
            "get_queue",
            rig.orch
                .get_queue(&auth, rig.session, rig.ws.path())
                .unwrap()
                .to_string(),
        ),
    ];
    if let Ok(progress) = rig.orch.get_progress(&auth, &run_id) {
        summaries.push(("get_progress", progress.to_string()));
    }
    let transcript = rig
        .orch
        .get_events(&auth, Some(&run_id), 0, 500)
        .map(|events| events.to_string())
        .unwrap_or_default();

    for (name, surface) in summaries
        .iter()
        .chain(std::iter::once(&("get_events", transcript.clone())))
    {
        assert!(
            !surface.contains(TOKEN),
            "{name} carries the control-plane bearer token"
        );
        for key in &key_material {
            assert!(
                !surface.contains(key.as_str()),
                "{name} carries sealing key material"
            );
        }
    }
    for (name, surface) in &summaries {
        assert!(
            !surface.contains(secret),
            "{name} carries the private prompt body"
        );
    }
    set_grokptah_home_override(None);
}

// ── packaged acceptance evidence ───────────────────────────────────────

/// Run one admission end to end and package what was actually observed.
///
/// The bundle is the artifact a reviewer reads instead of taking a summary on
/// trust, so it records versions and observed states rather than claims, and
/// is scanned for secrets exactly like any other published surface. Set
/// `GROKPTAH_ACCEPTANCE_EVIDENCE` to a path to keep a copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn acceptance_evidence_is_packaged_and_carries_no_secret() {
    let rig = Rig::new().await;
    let auth = rig.auth();
    let accepted = rig
        .orch
        .submit_task(
            &auth,
            "evidence-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("evidence-marker"),
            None,
        )
        .await
        .expect("submission is accepted");
    let run_id = accepted["runId"].as_str().unwrap().to_string();
    let accepted_state = accepted["state"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        accepted_state, "queued",
        "acceptance must be honest about not having started yet"
    );

    wait_for("the run to finish", Duration::from_secs(30), || {
        rig.orch
            .get_run(&auth, &run_id)
            .ok()
            .and_then(|value| serde_json::from_value::<RunState>(value["state"].clone()).ok())
            .map(|state| state.is_terminal())
            .unwrap_or(false)
    })
    .await;

    let store = rig.orch.store();
    let run = store
        .load_run(&run_id)
        .unwrap()
        .expect("the run is durable");
    let authority = store.seal_authority();
    let requests = store.list_provider_requests(&run_id).unwrap_or_default();
    let tombstone = store
        .load_idempotency_tombstone("evidence-request")
        .unwrap()
        .expect("a finished request leaves a tombstone");

    let evidence = serde_json::json!({
        "bundleVersion": 1,
        "subject": "grokptah durable agent admission",
        "versions": {
            "seal": SEAL_VERSION,
            "acceptanceIntent": ACCEPTANCE_INTENT_VERSION,
            "tombstone": TOMBSTONE_VERSION,
            "projection": PROJECTION_VERSION,
        },
        "sealAuthority": {
            "protection": format!("{:?}", authority.protection()),
            "keyId": authority.current_key_id(),
            "knownKeys": authority.known_key_ids().len(),
        },
        "observed": {
            "acceptedState": accepted_state,
            "terminalState": format!("{:?}", run.state),
            "terminalResult": run.terminal_result.clone(),
            "specKeyPresent": run.spec_key.is_some(),
            "specKeyAgrees": run.spec_key == tombstone.spec_key,
            "providerRequests": requests
                .iter()
                .map(|record| serde_json::json!({
                    "ordinal": record.request_ordinal,
                    "phase": record.phase.as_str(),
                    "idempotencyKeyPresent": !record.idempotency_key.is_empty(),
                }))
                .collect::<Vec<_>>(),
            "tombstoneOutcome": tombstone.outcome.clone(),
            "leaseReleased": store
                .load_attempt_lease(&run_id)
                .unwrap()
                .map(|lease| lease.state == AttemptLeaseState::Released),
            "teardownUncertain": store.list_teardown_uncertain().unwrap_or_default().len(),
            "markers": rig.markers(),
        },
        "notProved": [
            "soak and release qualification",
            "execution of the Windows ledger syscalls on a Windows host",
            "provider journaling through the production coding-agent loop",
        ],
    });

    // Every claim a reviewer would rely on must actually be present.
    for pointer in [
        "/versions/seal",
        "/versions/acceptanceIntent",
        "/versions/tombstone",
        "/versions/projection",
        "/sealAuthority/keyId",
        "/sealAuthority/protection",
        "/observed/acceptedState",
        "/observed/terminalState",
        "/observed/specKeyAgrees",
        "/observed/tombstoneOutcome",
        "/notProved",
    ] {
        assert!(
            evidence.pointer(pointer).is_some(),
            "the evidence bundle is missing {pointer}"
        );
    }
    assert_eq!(
        evidence.pointer("/observed/specKeyAgrees"),
        Some(&serde_json::Value::Bool(true)),
        "the run and its tombstone must name the same specification: {evidence}"
    );
    assert_eq!(
        evidence.pointer("/observed/teardownUncertain"),
        Some(&serde_json::json!(0)),
        "a clean run must leave no teardown uncertainty: {evidence}"
    );

    let rendered = serde_json::to_string_pretty(&evidence).unwrap();
    assert!(
        !rendered.contains(TOKEN),
        "the evidence bundle leaks the token"
    );
    assert!(
        !rendered.contains(&authority.current_key_id())
            || evidence["sealAuthority"]["keyId"] == authority.current_key_id(),
        "key ids are identifiers, not secrets, and appear only where declared"
    );

    let destination = std::env::var_os("GROKPTAH_ACCEPTANCE_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| rig.home.path().join("acceptance-evidence.json"));
    if let Some(parent) = destination.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&destination, rendered.as_bytes()).expect("write the evidence bundle");
    let reread: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
    assert_eq!(reread, evidence, "the bundle must round-trip");
    set_grokptah_home_override(None);
}

// ── child-process helpers ──────────────────────────────────────────────
//
// These are not tests. They are `#[ignore]`d so the normal run skips them,
// and are selected by name when this binary re-invokes itself as a child.

fn child_store() -> OrchStore {
    let root = std::env::var(CHILD_STORE).expect("child store root");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match OrchStore::open(&root) {
            Ok(store) => return store,
            Err(error) => {
                if std::time::Instant::now() >= deadline {
                    // Reported in every shape a caller reads, so a child that
                    // never got the ledger is never mistaken for one that did
                    // and then failed for some more interesting reason.
                    println!(
                        "P2CHILD {}",
                        serde_json::json!({
                            "opened": false,
                            "acquired": false,
                            "renewed": false,
                            "error": error.to_string(),
                        })
                    );
                    std::process::exit(0);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn child_guard(mode: &str) -> bool {
    std::env::var(CHILD_MODE).ok().as_deref() == Some(mode)
}

fn child_acquire(store: &OrchStore) -> serde_json::Value {
    let run_id = std::env::var(CHILD_RUN).expect("child run id");
    let ttl: u64 = std::env::var(CHILD_TTL_MS)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);
    let owner = format!("child-{}", std::process::id());
    match store.acquire_attempt_lease(
        &run_id,
        &owner,
        Uuid::nil(),
        // A cross-process lease test only exercises the lease; there is no
        // intent behind it, so the bound digest is a fixed placeholder.
        &"0".repeat(64),
        ttl,
    ) {
        Ok(lease) => serde_json::json!({
            "acquired": true,
            "attempt": lease.attempt,
            "attemptId": lease.attempt_id,
            "ownerId": lease.owner_id,
        }),
        Err(error) => serde_json::json!({ "acquired": false, "error": error.message }),
    }
}

fn child_seed_run(store: &OrchStore, run_id: &str, state: RunState) {
    let now = chrono::Utc::now();
    store
        .save_run(&grokptah_agent_bridge::orchestration::RunRecord {
            run_id: run_id.into(),
            session_id: Uuid::nil(),
            workspace: std::env::temp_dir().display().to_string(),
            request_id: format!("{run_id}-request"),
            client_id: Some("p2-child".into()),
            state,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            spec_key: None,
            bounds: RunBounds::default(),
            prompt_preview: "cross-process fixture".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .expect("seed the run");
}

/// Seal and persist an acceptance intent for a seeded run, so the retention
/// invariant has something real to act on.
fn child_seed_intent(store: &OrchStore, run_id: &str) {
    let intent = AcceptanceIntent {
        intent_version: ACCEPTANCE_INTENT_VERSION,
        run_id: run_id.to_string(),
        request_id: format!("{run_id}-request"),
        payload_hash: "0".repeat(64),
        tool: "ptah_submit_task".into(),
        session_id: Uuid::nil(),
        session_revision: "0".repeat(64),
        workspace: std::env::temp_dir().display().to_string(),
        workspace_revision: "0".repeat(64),
        agent_id: None,
        agent_revision: 0,
        spec_revision: "grokptah-agent-bridge/orchestration/1".into(),
        principal_token_id: "primary".into(),
        principal_revision: "0".repeat(64),
        policy_revision: "0".repeat(64),
        route_revision: "0".repeat(64),
        prompt: "cross-process private input".into(),
        bounds: SealedBounds {
            max_prompt_bytes: 10_000,
            max_rounds: 4,
            max_duration_ms: 30_000,
        },
        execution_mode: RunExecutionMode::Shared,
        allow_queue: true,
        retry_of: None,
        parent_run_id: None,
        created_at: chrono::Utc::now(),
        digest: String::new(),
        seal: SealStamp::unsealed(),
    }
    .seal_with(store.seal_authority())
    .expect("seal the intent");
    store
        .save_acceptance_intent(&intent)
        .expect("persist the intent");
}

/// Seed a `Running` attempt, fence it the way a synchronous `Drop` does, and
/// die without ever proving the work stopped.
#[test]
#[ignore = "child-process helper, selected by name"]
fn child_fences_a_running_attempt_and_dies() {
    if !child_guard("fence-and-die") {
        return;
    }
    let store = child_store();
    let run_id = std::env::var(CHILD_RUN).expect("child run id");
    child_seed_run(&store, &run_id, RunState::Running);
    let seeded_intent = std::env::var_os(CHILD_SEED_INTENT).is_some();
    if seeded_intent {
        child_seed_intent(&store, &run_id);
    }
    let mut report = child_acquire(&store);
    report["intentSeeded"] = serde_json::Value::Bool(seeded_intent);
    if report["acquired"] == true {
        store
            .record_teardown_uncertain(
                &run_id,
                report["attemptId"].as_str().unwrap(),
                report["ownerId"].as_str().unwrap(),
                "coordinator dropped without bounded await",
            )
            .expect("fence the attempt");
        // Some fixtures want the *settled* shape instead: fenced on the way
        // out, then explicitly reconciled before the process died.
        if std::env::var_os(CHILD_CLEAR_FENCE).is_some() {
            store
                .clear_teardown_uncertain(&run_id)
                .expect("clear fence");
        }
    }
    println!("P2CHILD {report}");
    let _ = std::io::stdout().flush();
    // Exit without unwinding: nothing releases, nothing runs a destructor, and
    // the advisory lock dies with the process.
    std::process::exit(0);
}

/// Seed a `Queued` run holding a lease — the window between taking the lease
/// and persisting the `Starting` cut — and die inside it.
#[test]
#[ignore = "child-process helper, selected by name"]
fn child_leases_a_queued_run_and_dies() {
    if !child_guard("queue-and-die") {
        return;
    }
    let store = child_store();
    let run_id = std::env::var(CHILD_RUN).expect("child run id");
    child_seed_run(&store, &run_id, RunState::Queued);
    println!("P2CHILD {}", child_acquire(&store));
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Open the ledger as a successor coordinator would, then take the run.
#[test]
#[ignore = "child-process helper, selected by name"]
fn child_recovers_and_takes_the_run() {
    if !child_guard("recover-and-hold") {
        return;
    }
    let store = child_store();
    println!("P2CHILD {}", child_acquire(&store));
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

#[test]
#[ignore = "child-process helper, selected by name"]
fn child_tries_to_renew() {
    if !child_guard("try-renew") {
        return;
    }
    let store = child_store();
    let run_id = std::env::var(CHILD_RUN).expect("child run id");
    let attempt_id = std::env::var(CHILD_ATTEMPT).expect("child attempt id");
    let owner = std::env::var(CHILD_OWNER).expect("child owner id");
    let report = match store.renew_attempt_lease(&run_id, &attempt_id, &owner) {
        Ok(lease) => serde_json::json!({ "renewed": true, "attempt": lease.attempt }),
        Err(error) => serde_json::json!({ "renewed": false, "error": error.message }),
    };
    println!("P2CHILD {report}");
}

#[test]
#[ignore = "child-process helper, selected by name"]
fn child_tries_to_open() {
    if !child_guard("open-only") {
        return;
    }
    let root = std::env::var(CHILD_STORE).expect("child store root");
    let report = match OrchStore::open(&root) {
        Ok(store) => {
            drop(store);
            serde_json::json!({ "opened": true })
        }
        Err(error) => serde_json::json!({ "opened": false, "error": error.to_string() }),
    };
    println!("P2CHILD {report}");
}
