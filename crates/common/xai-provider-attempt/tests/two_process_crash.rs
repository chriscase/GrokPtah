use std::fs;
use std::process::Command;

use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use tempfile::tempdir;
use uuid::Uuid;

use xai_provider_attempt::{AttemptContext, ProviderAttemptStore, SendState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorityPayload {
    principal_incarnation: String,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: String,
    effect_scope: String,
    revoked_effect_lease_ids: Vec<String>,
    issued_effect_lease_ids: Vec<String>,
}

#[derive(Serialize)]
struct SignedAuthorityRecord {
    #[serde(flatten)]
    payload: AuthorityPayload,
    signature: String,
}

fn install_host_snapshot(root: &std::path::Path, scope: &str) {
    fs::create_dir_all(root.join("canonical-authorities")).unwrap();
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let public_key = root
        .join("canonical-authorities")
        .join(".authority-public-key");
    fs::write(&public_key, signing_key.verifying_key().to_bytes()).unwrap();
    let lease_id = format!("two-process-lease-{}", Uuid::new_v4());
    let payload = AuthorityPayload {
        principal_incarnation: "two-process-principal".into(),
        auth_generation: 1,
        capability_generation: 1,
        effect_lease_id: lease_id.clone(),
        effect_scope: scope.into(),
        revoked_effect_lease_ids: Vec::new(),
        issued_effect_lease_ids: vec![lease_id],
    };
    let signature = signing_key.sign(&serde_json::to_vec(&payload).unwrap());
    fs::write(
        root.join("canonical-authorities")
            .join(format!("{scope}.json")),
        serde_json::to_vec(&SignedAuthorityRecord {
            payload,
            signature: signature
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&public_key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(
            root.join("canonical-authorities")
                .join(format!("{scope}.json")),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

#[test]
fn child_entry() {
    let Some(root) = std::env::var_os("XAI_PROVIDER_ATTEMPT_CHILD_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let scope = std::env::var("XAI_PROVIDER_ATTEMPT_CHILD_SCOPE").unwrap();
    let mode = std::env::var("XAI_PROVIDER_ATTEMPT_CHILD_MODE").unwrap();
    let store = ProviderAttemptStore::open(&root).unwrap();
    let context =
        AttemptContext::from_host_ledger(store, format!("two-process-operation-{mode}"), scope)
            .unwrap();
    let permit = context
        .begin("fake-provider", b"two-process-body", true)
        .unwrap();
    fs::write(root.join("attempt-id"), permit.attempt_id().as_bytes()).unwrap();
    if mode == "after-possible-write" {
        fs::write(root.join("provider-bytes"), b"one-fake-provider-byte").unwrap();
    }
    // A hard process exit intentionally skips Permit::Drop. The next process
    // must recover Sending to Uncertain and must not replay it automatically.
    std::process::exit(0);
}

#[test]
fn genuine_two_process_kill_restart_preserves_uncertainty_and_request_identity() {
    for mode in ["before-socket-write", "after-possible-write"] {
        let temp = tempdir().unwrap();
        let scope = format!("two-process-scope-{}", Uuid::new_v4());
        install_host_snapshot(temp.path(), &scope);
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("child_entry")
            .arg("--nocapture")
            .env("XAI_PROVIDER_ATTEMPT_CHILD_ROOT", temp.path())
            .env("XAI_PROVIDER_ATTEMPT_CHILD_SCOPE", &scope)
            .env("XAI_PROVIDER_ATTEMPT_CHILD_MODE", mode)
            .status()
            .unwrap();
        assert!(child.success(), "child process failed for {mode}");

        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let attempt_id = fs::read_to_string(temp.path().join("attempt-id")).unwrap();
        let attempt = store.load(attempt_id.trim()).unwrap().unwrap();
        let projection = attempt.projection().unwrap();
        assert_eq!(projection.send_state, SendState::Uncertain);
        let provider_bytes = fs::read(temp.path().join("provider-bytes"))
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        assert_eq!(provider_bytes > 0, mode == "after-possible-write");
        assert!(
            attempt
                .projection()
                .unwrap()
                .provider_request_id
                .starts_with("opaque-")
        );
    }
}
