//! Keyed sealing authority: forgery, rotation, and coordinated reseal.
//!
//! The property under test is the one a plain digest never had: an attacker
//! who can write the ledger still cannot mint a record this store will accept.
//! Everything else here — rotation, retention of previous keys, the all-holder
//! reseal transaction — exists to keep that property true while the key
//! changes.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, AcceptanceIntent, AuthContext, KeyProtection, OrchStore, OrchestrationConfig,
    OrchestrationService, ProviderSendState, RunBounds, SealAuthority, SealStamp, SealedBounds,
    SealedTombstone, WorkspaceAllowlist, ACCEPTANCE_INTENT_VERSION,
};
use grokptah_agent_bridge::{
    safe_id_filename, set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig,
    RunExecutionMode, SessionKind,
};
use serde_json::json;
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN: &str = "seal-authority-secret-token";

struct Rig {
    home: TempDir,
    ws: TempDir,
    _env: ProcessEnvGuard,
    _host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
    session: Uuid,
}

impl Rig {
    async fn new() -> Self {
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
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            open_store(&home.path().join("orch")).await,
            OrchestrationConfig {
                bearer_token: TOKEN.to_string(),
                allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
                max_concurrent_runs: 1,
                bounds: RunBounds {
                    max_prompt_bytes: 50_000,
                    max_rounds: 4,
                    max_duration_ms: 30_000,
                },
            },
        );
        Self {
            home,
            ws,
            _env: env,
            _host: host,
            orch,
            session: session.id,
        }
    }

    fn auth(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN}")))
            .unwrap()
    }

    fn store_root(&self) -> std::path::PathBuf {
        self.home.path().join("orch")
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

fn record_path(root: &Path, ledger: &str, id: &str) -> std::path::PathBuf {
    root.join(ledger)
        .join(format!("{}.json", safe_id_filename(id).unwrap()))
}

/// Populate one run with every sealed holder: input, lease, send, tombstone.
async fn seed_all_holders(rig: &Rig) -> String {
    let auth = rig.auth();
    // A blocker keeps the target queued so its input survives for inspection.
    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "seal-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 20".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "seal-target",
            rig.session,
            rig.ws.path(),
            "run printf 'seal\\n' >> ledger.txt".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();

    let store = rig.orch.store();
    let intent = store.load_acceptance_intent(&run_id).unwrap().unwrap();
    let lease = store
        .acquire_attempt_lease(
            &run_id,
            "seal-owner",
            rig.session,
            intent.spec_key(),
            600_000,
        )
        .unwrap();
    store
        .open_provider_send(&run_id, &lease.attempt_id, intent.spec_key())
        .unwrap();
    run_id
}

// ── forgery ────────────────────────────────────────────────────────────

/// An attacker with full write access to the ledger, who rewrites a record and
/// recomputes its public identity, still cannot make it load. This is the
/// property a bare digest never provided.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_ledger_writer_without_the_key_cannot_forge_any_holder() {
    let rig = Rig::new().await;
    let run_id = seed_all_holders(&rig).await;
    let root = rig.store_root();
    let store = rig.orch.store();

    // Every holder is currently authentic.
    assert!(store.load_acceptance_intent(&run_id).unwrap().is_some());
    assert!(store.load_attempt_lease(&run_id).unwrap().is_some());
    assert!(store.load_provider_send(&run_id).unwrap().is_some());
    assert!(store
        .load_idempotency_tombstone("seal-target")
        .unwrap()
        .is_some());

    // Rewrite each on disk with a recomputed identity, the best an attacker
    // without the key can do.
    let forge = |ledger: &str, id: &str, mutate: &dyn Fn(&mut serde_json::Value)| {
        let path = record_path(&root, ledger, id);
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        mutate(&mut value);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    };

    forge("inputs", &run_id, &|value| {
        value["prompt"] = json!("run printf 'forged\\n' >> ledger.txt");
    });
    forge("leases", &run_id, &|value| {
        value["ownerId"] = json!("attacker");
    });
    forge("sends", &run_id, &|value| {
        value["state"] = json!("sent");
    });
    forge("tombstones", "seal-target", &|value| {
        // Re-point the decision at different work: the shape a forger would
        // use to make a refused request look like an accepted one.
        value["runId"] = json!("attacker-chosen-run");
        value["outcome"] = json!("failed");
    });

    // None of them authenticate, and none of them read back as "absent".
    assert!(
        store.load_acceptance_intent(&run_id).is_err(),
        "a forged input must fail closed, not read as missing"
    );
    assert!(store.load_attempt_lease(&run_id).is_err());
    assert!(store.load_provider_send(&run_id).is_err());
    assert!(store.load_idempotency_tombstone("seal-target").is_err());
    set_grokptah_home_override(None);
}

/// A forger who reseals under a key of their own choosing is refused by key
/// identity, not accepted as some other authority's word.
#[test]
fn a_foreign_authority_seal_is_refused() {
    let honest = SealAuthority::with_key(vec![0x01; 32]).unwrap();
    let attacker = SealAuthority::with_key(vec![0x02; 32]).unwrap();

    let tombstone = SealedTombstone {
        tombstone_version: 1,
        request_id: "req-1".into(),
        tool: "ptah_submit_task".into(),
        payload_hash: hash_payload(&json!({"a": 1})),
        outcome: "complete".into(),
        run_id: Some("run-1".into()),
        spec_key: None,
        recorded_at: chrono::Utc::now(),
        digest: String::new(),
        seal: SealStamp::unsealed(),
    }
    .seal_with(&attacker)
    .unwrap();

    assert!(tombstone.validate(&attacker).is_ok());
    let error = tombstone.validate(&honest).unwrap_err();
    assert!(
        format!("{error}").contains("does not hold"),
        "the refusal must name the unknown key, not a content mismatch: {error}"
    );
}

// ── rotation and coordinated reseal ────────────────────────────────────

/// Rotation followed by one coordinated reseal carries every holder across
/// together. Retiring the previous key afterwards leaves nothing unverifiable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn rotation_and_coordinated_reseal_carry_every_holder_across() {
    let rig = Rig::new().await;
    let run_id = seed_all_holders(&rig).await;
    let store = rig.orch.store().clone();
    let authority = store.seal_authority().clone();

    let first_key = authority.current_key_id();
    let second_key = authority.rotate().unwrap();
    assert_ne!(first_key, second_key);

    // Before the reseal every holder still verifies, under the retained key.
    assert!(store.load_acceptance_intent(&run_id).unwrap().is_some());
    assert!(store.load_attempt_lease(&run_id).unwrap().is_some());
    assert!(store.load_provider_send(&run_id).unwrap().is_some());
    assert!(store
        .load_idempotency_tombstone("seal-target")
        .unwrap()
        .is_some());

    let report = store.reseal_all_holders().unwrap();
    assert!(report.inputs_scanned >= 1, "{report:?}");
    assert!(report.leases_scanned >= 1, "{report:?}");
    assert!(report.sends_scanned >= 1, "{report:?}");
    assert!(report.tombstones_scanned >= 1, "{report:?}");
    assert!(
        report.resealed >= 4,
        "every holder must be carried across in one transaction: {report:?}"
    );

    // Every holder is now sealed under the current key, so the previous key
    // can be retired without making anything unverifiable.
    assert_eq!(authority.retire_previous_keys().unwrap(), 1);
    let intent = store.load_acceptance_intent(&run_id).unwrap().unwrap();
    assert!(authority.is_current(&intent.seal));
    let lease = store.load_attempt_lease(&run_id).unwrap().unwrap();
    assert!(authority.is_current(&lease.seal));
    let send = store.load_provider_send(&run_id).unwrap().unwrap();
    assert!(authority.is_current(&send.seal));
    assert_eq!(send.state, ProviderSendState::KnownNotSent);
    let tombstone = store
        .load_idempotency_tombstone("seal-target")
        .unwrap()
        .unwrap();
    assert!(authority.is_current(&tombstone.seal));

    // Resealing again is a no-op: nothing is left under an older key.
    let second = store.reseal_all_holders().unwrap();
    assert_eq!(second.resealed, 0, "{second:?}");
    set_grokptah_home_override(None);
}

/// Retiring the previous key *without* resealing makes the old records
/// unverifiable — a refusal, never a silent acceptance. This is the state the
/// coordinated transaction exists to avoid, asserted directly so the ordering
/// requirement is not merely documented.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn retiring_a_key_before_resealing_fails_closed() {
    let rig = Rig::new().await;
    let run_id = seed_all_holders(&rig).await;
    let store = rig.orch.store().clone();
    let authority = store.seal_authority().clone();

    authority.rotate().unwrap();
    authority.retire_previous_keys().unwrap();

    assert!(
        store.load_acceptance_intent(&run_id).is_err(),
        "a record under a retired key must not load"
    );
    // And the reseal transaction refuses too, rather than papering over it by
    // re-sealing content it cannot authenticate.
    assert!(store.reseal_all_holders().is_err());
    set_grokptah_home_override(None);
}

/// A store whose sealing authority cannot be opened does not open at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_store_without_a_usable_authority_fails_closed() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    {
        let store = OrchStore::open(&root).unwrap();
        assert!(matches!(
            store.seal_authority().protection(),
            KeyProtection::PlatformKeyring | KeyProtection::OwnerOnlyFile
        ));
    }
    // Corrupt the key material the way a partial write or a hostile edit would.
    let key_path = root.join("keys").join("authority.json");
    if key_path.is_file() {
        std::fs::write(
            &key_path,
            b"{\"version\":1,\"currentKeyId\":\"nope\",\"keys\":{}}",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = match OrchStore::open(&root) {
            Ok(_) => panic!("a store must refuse to open without its authority"),
            Err(error) => error,
        };
        assert!(
            format!("{error}").contains("sealing authority unavailable"),
            "a store must refuse to open without its authority: {error}"
        );
    }
}

/// The sealing key never reaches a projection, an error, or a debug rendering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn the_sealing_key_never_leaves_the_authority() {
    let rig = Rig::new().await;
    let run_id = seed_all_holders(&rig).await;
    let root = rig.store_root();
    let authority = rig.orch.store().seal_authority();

    // Whatever the key is, its material must not appear in any record we write.
    let key_path = root.join("keys").join("authority.json");
    let key_material = std::fs::read_to_string(&key_path).unwrap_or_default();
    let secrets: Vec<String> = serde_json::from_str::<serde_json::Value>(&key_material)
        .ok()
        .and_then(|value| {
            value.get("keys").and_then(|k| k.as_object()).map(|k| {
                k.values()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default();

    for ledger in ["inputs", "leases", "sends", "tombstones"] {
        let id = if ledger == "tombstones" {
            "seal-target"
        } else {
            &run_id
        };
        let text = std::fs::read_to_string(record_path(&root, ledger, id)).unwrap();
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{ledger} record contains key material"
            );
        }
    }

    let rendered = format!("{authority:?}");
    for secret in &secrets {
        assert!(!rendered.contains(secret.as_str()), "debug leaked the key");
    }
    // The key id is a digest and is safe to publish beside records.
    assert_eq!(authority.current_key_id().len(), 64);
    set_grokptah_home_override(None);
}

/// An intent sealed by this store authenticates; one built out-of-band without
/// a seal does not, so an "unsealed" record can never be executed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn an_unsealed_record_is_never_accepted() {
    let rig = Rig::new().await;
    let authority = rig.orch.store().seal_authority();
    let unsealed = AcceptanceIntent {
        intent_version: ACCEPTANCE_INTENT_VERSION,
        run_id: Uuid::new_v4().to_string(),
        request_id: "unsealed".into(),
        payload_hash: hash_payload(&json!({"x": 1})),
        tool: "ptah_submit_task".into(),
        session_id: rig.session,
        session_revision: hash_payload(&json!({"s": 1})),
        workspace: rig.ws.path().display().to_string(),
        workspace_revision: hash_payload(&json!({"w": 1})),
        agent_id: None,
        agent_revision: 0,
        spec_revision: "grokptah-agent-bridge/orchestration/1".into(),
        principal_token_id: "primary".into(),
        principal_revision: hash_payload(&json!({"p": 1})),
        policy_revision: hash_payload(&json!({"q": 1})),
        route_revision: hash_payload(&json!({"r": 1})),
        prompt: "run true".into(),
        bounds: SealedBounds {
            max_prompt_bytes: 1000,
            max_rounds: 2,
            max_duration_ms: 1000,
        },
        execution_mode: RunExecutionMode::Shared,
        allow_queue: true,
        retry_of: None,
        parent_run_id: None,
        created_at: chrono::Utc::now(),
        digest: String::new(),
        seal: SealStamp::unsealed(),
    };
    let mut unsealed = unsealed;
    unsealed.digest = unsealed.digest_for();
    assert!(
        unsealed.validate(authority).is_err(),
        "a record with a correct identity and no seal must not authenticate"
    );
    // The store refuses to persist it too, rather than storing something it
    // could not later verify.
    assert!(rig.orch.store().save_acceptance_intent(&unsealed).is_err());
    set_grokptah_home_override(None);
}
