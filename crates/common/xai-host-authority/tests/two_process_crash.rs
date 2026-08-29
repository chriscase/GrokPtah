//! A real crash cut across two processes.
//!
//! The child opens the authority, takes a physical-send permit, and then dies
//! by `abort()` between dispatch and settlement — no unwinding, no destructors,
//! no cooperative cleanup. The parent then opens the same root and must find
//! exactly one ambiguous attempt, never a retried or silently-failed one.

use std::path::Path;
use std::process::Command;

use xai_host_authority::*;

const OWNER: &str = "account-1";
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
    let (authority, admin) = HostAuthority::open(root, OWNER).expect("child open");
    authority
        .set_credentials(
            &admin,
            &[HostCredential::new("primary", SECRET).unwrap()],
            OWNER,
        )
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
        .seal_capability(&auth, resource, EffectClass::ProviderSend, 60_000)
        .expect("child capability");
    let req = request(b"body");
    let lease = authority
        .mint_lease(&auth, &capability, req.digest(), 60_000)
        .expect("child lease");

    // The permit exists only after the attempt and its intent are durable.
    let permit = authority
        .begin_send(&auth, lease, &req)
        .expect("child permit");
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
    let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
    let auth = authority.authenticate(SECRET).unwrap();
    let recovered = authority.recover_incomplete().unwrap();
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
    assert!(authority.recover_incomplete().unwrap().is_empty());

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
/// Appends each racing child makes. Every successful `authenticate` writes one
/// audit record, so this is also the number of appends per child.
const APPENDS_PER_CHILD: usize = 25;
const RACING_CHILDREN: usize = 4;

/// One racing child: hammer the shared root with audit appends from its own
/// process, then exit 21.
fn child_race(root: &Path) -> ! {
    let Ok((authority, _admin)) = HostAuthority::open(root, OWNER) else {
        std::process::exit(30);
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
    let (authority, admin) = HostAuthority::open(&root, OWNER).unwrap();
    authority
        .set_credentials(
            &admin,
            &[HostCredential::new("primary", SECRET).unwrap()],
            OWNER,
        )
        .unwrap();
    let before = authority.audit_records(&admin).unwrap().len();
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

    let (reopened, reopened_admin) = HostAuthority::open(&root, OWNER).unwrap();
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
