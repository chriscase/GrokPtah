//! A real crash cut across two processes.
//!
//! The child opens the authority, wire-admits a physical-send permit, and then
//! dies by `abort()` between dispatch and settlement — no unwinding, no
//! destructors, no cooperative cleanup. The parent then opens the same root
//! and must find exactly one ambiguous attempt, never a retried or silently-
//! failed one.

use std::path::Path;
use std::process::Command;

use xai_host_authority::*;

const ADMIN_SECRET: &str = "host-admin-custody-secret-32-bytes-minimum-v1";

fn admin_credential() -> HostAdminCredential {
    HostAdminCredential::new(ADMIN_SECRET).unwrap()
}
const SECRET: &str = "s3cret-bearer-value";
const CHILD_ENV: &str = "XAI_HOST_AUTHORITY_CRASH_CHILD_ROOT";

fn request(body: &[u8]) -> RequestIdentity {
    RequestIdentity::new(
        "https://api.example.invalid/v1/chat",
        "POST",
        "openai-chat",
        b"provider-key",
        "grok-4",
        body,
    )
}

/// Everything the child does before it dies.
fn child_take_permit_then_die(root: &Path) -> ! {
    let (authority, admin) = HostAuthority::open(root, &admin_credential()).expect("child open");
    authority
        .set_credentials(&admin, &[HostCredential::new("primary", SECRET).unwrap()])
        .expect("child credentials");
    let auth = authority.authenticate(SECRET).expect("child authenticate");
    let session = authority.issue_session(&auth).expect("child session");
    let workspace = authority
        .issue_workspace(&auth, root)
        .expect("child workspace");
    let resource = authority
        .issue_resource(
            &auth,
            session,
            workspace,
            ContentDigest::of_bytes(b"frame-1"),
        )
        .expect("child resource");
    let capability = authority
        .seal_capability(
            &auth,
            resource,
            ActorClass::VerifiedOperator,
            EffectClass::ProviderSend,
            60_000,
        )
        .expect("child capability");
    let req = request(b"body");
    let lease = authority
        .mint_lease(&auth, &capability, req.digest(), 60_000)
        .expect("child lease");

    // The permit exists only after the attempt and its intent are durable.
    let permit = authority
        .begin_send(&auth, lease, &req, "test-route")
        .expect("child permit");
    let permit = authority
        .admit_sending(&auth, permit)
        .expect("child wire admission");
    // Signal that we got this far, then die where a physical send would be
    // in flight: after admission, before settlement.
    std::fs::write(
        root.join("child-reached-dispatch"),
        permit.attempt().public_handle(),
    )
    .expect("child marker");
    std::process::abort();
}

#[test]
fn an_attempt_in_flight_when_the_process_dies_recovers_as_uncertain() {
    // Child mode: the harness re-invokes this same binary.
    if let Ok(root) = std::env::var(CHILD_ENV) {
        child_take_permit_then_die(Path::new(&root));
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("an_attempt_in_flight_when_the_process_dies_recovers_as_uncertain")
        .arg("--nocapture")
        .env(CHILD_ENV, &root)
        .status()
        .expect("spawn crash child");

    assert!(
        !status.success(),
        "the child must die rather than exit cleanly"
    );
    let marker = root.join("child-reached-dispatch");
    assert!(
        marker.exists(),
        "the child must have held a permit before dying"
    );
    let handle = std::fs::read_to_string(&marker).unwrap();

    // A fresh host incarnation takes over.
    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let recovered = authority.recover_incomplete(&admin).unwrap();
    assert_eq!(
        recovered.len(),
        1,
        "exactly the in-flight attempt must be recovered"
    );
    assert_eq!(recovered[0].public_handle(), handle);

    let projection = authority
        .attempt_projection(&auth, recovered[0])
        .unwrap()
        .unwrap();
    assert!(
        projection.ambiguous,
        "an attempt cut short by a crash must be ambiguous"
    );
    assert!(
        projection.settled,
        "it must have left the sending state rather than staying in flight"
    );

    // Recovery never re-sends and is idempotent.
    assert!(authority.recover_incomplete(&admin).unwrap().is_empty());

    // The audit log survived the crash and still chains.
    assert!(authority.audit_chain_intact(&admin).unwrap());
    let records = authority.audit_records(&admin).unwrap();
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, AuditEvent::SendIntent { .. })),
        "the intent written before the crash must be durable"
    );
}

const RACE_ENV: &str = "XAI_HOST_AUTHORITY_RACE_CHILD_ROOT";
const CUSTODY_ENV: &str = "XAI_HOST_AUTHORITY_CUSTODY_CHILD_ROOT";
const CUSTODY_MODE: &str = "XAI_HOST_AUTHORITY_CUSTODY_CHILD_MODE";
/// Appends each racing child makes. Every successful `authenticate` writes one
/// audit record, so this is also the number of appends per child.
const APPENDS_PER_CHILD: usize = 25;
const RACING_CHILDREN: usize = 4;

/// One racing child: hammer the shared root with audit appends from its own
/// process, then exit 21.
fn child_race(root: &Path) -> ! {
    // Admin authority is exclusive, so the children take the root in turn
    // rather than concurrently. The appends still come from separate
    // processes, which is what the chain has to survive.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let authority = loop {
        match HostAuthority::open(root, &admin_credential()) {
            Ok((authority, _admin)) => break authority,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => std::process::exit(30),
        }
    };
    for _ in 0..APPENDS_PER_CHILD {
        if authority.authenticate(SECRET).is_err() {
            std::process::exit(31);
        }
    }
    std::process::exit(21);
}

#[test]
fn concurrent_processes_cannot_fork_the_audit_chain() {
    if let Ok(root) = std::env::var(RACE_ENV) {
        child_race(Path::new(&root));
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    authority
        .set_credentials(&admin, &[HostCredential::new("primary", SECRET).unwrap()])
        .unwrap();
    let before = authority.audit_records(&admin).unwrap().len();
    // Release the root so the children can take it; exclusivity is proven by
    // the fact that they must.
    drop(authority);

    // Separate processes, so this is genuine cross-process exclusion rather
    // than an in-process mutex: `flock` is held per open file description.
    let mut children = Vec::new();
    for _ in 0..RACING_CHILDREN {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("concurrent_processes_cannot_fork_the_audit_chain")
                .env(RACE_ENV, &root)
                .spawn()
                .expect("spawn racing child"),
        );
    }
    for mut child in children {
        assert_eq!(
            child.wait().unwrap().code(),
            Some(21),
            "every racing child must complete its appends"
        );
    }

    let (reopened, reopened_admin) = HostAuthority::open(&root, &admin_credential()).unwrap();
    let records = reopened.audit_records(&reopened_admin).unwrap();

    // Not one append lost, and not one sequence number reused: without
    // cross-process serialisation the children would each have derived the
    // same chain head and written over each other.
    assert_eq!(
        records.len(),
        before + RACING_CHILDREN * APPENDS_PER_CHILD,
        "every append from every process must survive"
    );
    for (i, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            i as u64 + 1,
            "sequence numbers must be dense and unique across processes"
        );
    }
    assert!(
        reopened.audit_chain_intact(&reopened_admin).unwrap(),
        "the hash chain must survive concurrent multi-process appends"
    );
}

#[test]
fn administrative_custody_is_exclusive_and_authenticated_across_processes() {
    if let Ok(root) = std::env::var(CUSTODY_ENV) {
        let mode = std::env::var(CUSTODY_MODE).expect("custody child mode");
        let credential = if mode == "wrong" {
            HostAdminCredential::new("wrong-host-custody-secret-32-bytes-minimum").unwrap()
        } else {
            admin_credential()
        };
        let opened = HostAuthority::open(Path::new(&root), &credential);
        let code = match (mode.as_str(), opened) {
            ("contend", Err(AuthorityError::Durability(_))) => 41,
            ("wrong", Err(AuthorityError::Unauthenticated)) => 42,
            ("right", Ok((_authority, _admin))) => 43,
            _ => 49,
        };
        std::process::exit(code);
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (authority, _admin) = HostAuthority::open(&root, &admin_credential()).unwrap();

    // These processes race while the parent genuinely holds the OS lock; none
    // may mint a second admin token even with the correct custody secret.
    let mut contenders = Vec::new();
    for _ in 0..4 {
        contenders.push(
            Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("administrative_custody_is_exclusive_and_authenticated_across_processes")
                .env(CUSTODY_ENV, &root)
                .env(CUSTODY_MODE, "contend")
                .spawn()
                .unwrap(),
        );
    }
    for mut contender in contenders {
        assert_eq!(contender.wait().unwrap().code(), Some(41));
    }
    drop(authority);

    // Once custody is released, path knowledge and a wrong secret remain
    // insufficient, while the original host credential can resume custody.
    let wrong = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("administrative_custody_is_exclusive_and_authenticated_across_processes")
        .env(CUSTODY_ENV, &root)
        .env(CUSTODY_MODE, "wrong")
        .status()
        .unwrap();
    assert_eq!(wrong.code(), Some(42));
    let right = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("administrative_custody_is_exclusive_and_authenticated_across_processes")
        .env(CUSTODY_ENV, &root)
        .env(CUSTODY_MODE, "right")
        .status()
        .unwrap();
    assert_eq!(right.code(), Some(43));
}
