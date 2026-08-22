//! Deterministic logical-years certification for the v2 durable memory core.
//!
//! The checked-in fixture is seed/oracle data only. Runtime canaries never
//! enter that JSON. There are no live provider calls, sleeps, or Tokio-time
//! claims: every timestamp comes from the injected fake clock.

use std::fs;
use std::path::Path;

use grokptah_agent_bridge::{home_override_serial, set_grokptah_home_override, MemoryScope};
use serde_json::{json, Value};

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../evals/certification-lab/replay-fixtures/memory-long-horizon.v1.json"
));

/// Runtime-only canary. Must never appear in errors, debug codes, or the fixture.
const SCOPE_CANARY: &str = "gp_canary_7f3a9c2e1b_not_for_errors";

struct IsolatedHome {
    _serial: std::sync::MutexGuard<'static, ()>,
    _root: tempfile::TempDir,
}

impl IsolatedHome {
    fn install() -> Self {
        let serial = home_override_serial();
        let root = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(root.path().join(".grokptah")));
        Self {
            _serial: serial,
            _root: root,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
    }
}

struct Lab<'a> {
    workspace: &'a Path,
    agent: &'a str,
    teams: Vec<&'a str>,
}

impl<'a> Lab<'a> {
    fn project() -> MemoryScope {
        MemoryScope::Project
    }

    fn private(&self) -> MemoryScope {
        MemoryScope::AgentPrivate {
            agent_id: self.agent.to_string(),
        }
    }

    fn team(&self) -> MemoryScope {
        MemoryScope::Team {
            team_id: self.teams[0].to_string(),
        }
    }

    fn scope(&self, name: &str) -> MemoryScope {
        match name {
            "project" => Self::project(),
            "private" => self.private(),
            "team" => self.team(),
            other => panic!("unknown scope {other}"),
        }
    }

    fn write(&self, scope: &MemoryScope, now: &str, request: Value) -> Result<Value, Value> {
        scope.write_versioned(self.workspace, Some(self.agent), &self.teams, now, request)
    }

    fn retrieve(&self, scope: &MemoryScope, at: &str) -> Value {
        scope
            .retrieve_at(self.workspace, Some(self.agent), &self.teams, at)
            .unwrap_or_else(|error| panic!("retrieve failed: {error}"))
    }

    fn search(&self, scope: &MemoryScope, now: &str, query: &str) -> Value {
        scope
            .search_legacy(self.workspace, Some(self.agent), &self.teams, now, query)
            .unwrap_or_else(|error| panic!("search failed: {error}"))
    }

    fn stored_len(&self, scope: &MemoryScope, now: &str) -> usize {
        self.search(scope, now, "")["facts"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn hot_bytes(&self, scope: &MemoryScope) -> usize {
        scope
            .hot_store_bytes(self.workspace, Some(self.agent), &self.teams)
            .unwrap()
    }

    fn path(&self, scope: &MemoryScope) -> std::path::PathBuf {
        scope
            .durable_path(self.workspace, Some(self.agent), &self.teams)
            .unwrap()
    }
}

fn instant<'a>(fixture: &'a Value, key: &str) -> &'a str {
    fixture["instants"][key]
        .as_str()
        .unwrap_or_else(|| panic!("missing instant {key}"))
}

fn caller_request(
    idempotency_key: &str,
    text: &str,
    claim_key: Option<&str>,
    criticality: &str,
    salience: &str,
    actor: &str,
) -> Value {
    let mut request = json!({
        "idempotency_key": idempotency_key,
        "text": text,
        "tags": [],
        "criticality": criticality,
        "salience": salience,
        "source": { "kind": "caller", "actor": actor },
    });
    if let Some(claim_key) = claim_key {
        request["claim_key"] = json!(claim_key);
    }
    request
}

fn current_texts(retrieved: &Value) -> Vec<&str> {
    retrieved["current"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["text"].as_str().unwrap())
        .collect()
}

fn current_claim_keys(retrieved: &Value) -> Vec<&str> {
    retrieved["current"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|fact| fact["claim_key"].as_str())
        .collect()
}

fn assert_secret_free(label: &str, raw: &str) {
    let lower = raw.to_ascii_lowercase();
    for needle in [
        "secret",
        "password",
        "api_key",
        "sk-",
        "bearer ",
        "authorization:",
        SCOPE_CANARY,
    ] {
        assert!(
            !lower.contains(&needle.to_ascii_lowercase()),
            "{label} must not contain {needle}"
        );
    }
}

fn assert_no_canary(label: &str, raw: &str) {
    assert!(
        !raw.contains(SCOPE_CANARY),
        "{label} leaked the scope canary"
    );
}

#[test]
fn logical_years_certification_matches_seed_oracle() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(
        fixture["schema"].as_str(),
        Some("grokptah.memory_long_horizon_fixture.v1")
    );
    assert_secret_free("checked-in fixture", FIXTURE);
    assert_eq!(
        fixture["declared_bounds"],
        MemoryScope::long_horizon_declared_bounds()
    );
    let oracle = &fixture["oracle"];
    assert_eq!(oracle["critical_current_retention_pct"], 100);
    assert_eq!(oracle["authoritative_superseded_expired_as_current"], 0);
    assert_eq!(oracle["latest_valid_revision_selection_pct"], 100);
    assert_eq!(oracle["unresolved_conflicts_surfaced_pct"], 100);
    assert_eq!(oracle["scope_canary_leakage"], 0);
    assert_eq!(oracle["idempotent_ack_unique_revisions"], 1);
    assert_eq!(oracle["malformed_cross_scope_cyclic_mutations"], 0);
    assert_eq!(oracle["repeated_read_reopen_deterministic"], true);
    assert_eq!(oracle["hot_store_within_byte_bound"], true);

    let _home = IsolatedHome::install();
    let workspace = tempfile::tempdir().unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let agent = fixture["scopes"]["private_agent_id"].as_str().unwrap();
    let team = fixture["scopes"]["team_id"].as_str().unwrap();
    let lab = Lab {
        workspace: workspace.path(),
        agent,
        teams: vec![team],
    };
    let y0 = instant(&fixture, "y0");
    let y1 = instant(&fixture, "y1");
    let y2 = instant(&fixture, "y2");
    let y6 = instant(&fixture, "y6");
    let y8 = instant(&fixture, "y8");
    let y10 = instant(&fixture, "y10");
    let equal_stamp = instant(&fixture, "equal_stamp");
    let status_until = instant(&fixture, "status_until");
    let max_bytes = fixture["declared_bounds"]["max_hot_store_bytes"]
        .as_u64()
        .unwrap() as usize;

    let mut mutation_failures = 0u32;
    let mut leaked = 0u32;

    for seed in fixture["seeds"]["critical_invariants"].as_array().unwrap() {
        let ack = lab
            .write(
                &lab.scope(seed["scope"].as_str().unwrap()),
                y0,
                caller_request(
                    seed["idempotency_key"].as_str().unwrap(),
                    seed["text"].as_str().unwrap(),
                    seed["claim_key"].as_str(),
                    "critical",
                    seed["salience"].as_str().unwrap(),
                    agent,
                ),
            )
            .unwrap();
        assert_eq!(ack["revision"], 1);
        assert_eq!(ack["replayed"], false);
    }

    let indent = &fixture["seeds"]["preference_revisions"];
    let indent_v1 = lab
        .write(
            &lab.scope(indent["scope"].as_str().unwrap()),
            instant(&fixture, indent["v1"]["at"].as_str().unwrap()),
            caller_request(
                indent["v1"]["idempotency_key"].as_str().unwrap(),
                indent["v1"]["text"].as_str().unwrap(),
                indent["claim_key"].as_str(),
                "normal",
                "medium",
                agent,
            ),
        )
        .unwrap();

    let path = lab.path(&Lab::project());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stored["facts"]
        .as_array_mut()
        .unwrap()
        .push(fixture["seeds"]["legacy_v1_fact"].clone());
    fs::write(&path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();

    let status = &fixture["seeds"]["expiring_status"];
    lab.write(
        &lab.scope(status["scope"].as_str().unwrap()),
        instant(&fixture, status["at"].as_str().unwrap()),
        json!({
            "idempotency_key": status["idempotency_key"],
            "text": status["text"],
            "claim_key": status["claim_key"],
            "criticality": "normal",
            "salience": "low",
            "valid_until": status_until,
            "source": { "kind": "caller", "actor": agent },
        }),
    )
    .unwrap();

    let indent_v2_request = json!({
        "idempotency_key": indent["v2"]["idempotency_key"],
        "text": indent["v2"]["text"],
        "claim_key": indent["claim_key"],
        "criticality": "normal",
        "salience": "medium",
        "supersedes": indent_v1["id"],
        "source": { "kind": "caller", "actor": agent },
    });
    let indent_v2 = lab
        .write(
            &lab.scope(indent["scope"].as_str().unwrap()),
            instant(&fixture, indent["v2"]["at"].as_str().unwrap()),
            indent_v2_request.clone(),
        )
        .unwrap();
    assert_eq!(indent_v2["revision"], 2);

    let duplicates = &fixture["seeds"]["duplicates"];
    for key in duplicates["keys"].as_array().unwrap() {
        lab.write(
            &lab.scope(duplicates["scope"].as_str().unwrap()),
            equal_stamp,
            caller_request(
                key.as_str().unwrap(),
                duplicates["text"].as_str().unwrap(),
                None,
                "normal",
                "medium",
                agent,
            ),
        )
        .unwrap();
    }

    let conflicts = &fixture["seeds"]["conflicts"];
    for entry in conflicts["texts"].as_array().unwrap() {
        lab.write(
            &lab.scope(conflicts["scope"].as_str().unwrap()),
            instant(&fixture, conflicts["at"].as_str().unwrap()),
            caller_request(
                entry["idempotency_key"].as_str().unwrap(),
                entry["text"].as_str().unwrap(),
                conflicts["claim_key"].as_str(),
                "normal",
                "medium",
                agent,
            ),
        )
        .unwrap();
    }

    let replay = lab
        .write(&Lab::project(), y2, indent_v2_request.clone())
        .unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["id"], indent_v2["id"]);
    assert_eq!(replay["revision"], indent_v2["revision"]);
    let mut changed = indent_v2_request;
    changed["text"] = json!("Use tabs again.");
    let conflict = lab.write(&Lab::project(), y2, changed).unwrap_err();
    assert_eq!(conflict["code"], "idempotency_conflict");
    assert_no_canary("idempotency error", &conflict.to_string());

    let project_at_y1 = lab.retrieve(&Lab::project(), y1);
    assert!(current_claim_keys(&project_at_y1).contains(&"deploy-window"));
    let project_at_y10 = lab.retrieve(&Lab::project(), y10);
    assert!(!current_claim_keys(&project_at_y10).contains(&"deploy-window"));
    assert!(!current_texts(&project_at_y10)
        .iter()
        .any(|text| *text == indent["v1"]["text"].as_str().unwrap()));
    assert!(current_texts(&project_at_y10)
        .iter()
        .any(|text| *text == indent["v2"]["text"].as_str().unwrap()));
    let superseded_as_current = project_at_y10["current"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fact| fact["id"] == indent_v1["id"])
        .count();
    assert_eq!(
        superseded_as_current,
        oracle["authoritative_superseded_expired_as_current"]
            .as_u64()
            .unwrap() as usize
    );
    let latest = project_at_y10["current"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["claim_key"] == indent["claim_key"])
        .unwrap();
    assert_eq!(latest["id"], indent_v2["id"]);
    assert_eq!(latest["revision"], 2);

    let search = lab.search(
        &Lab::project(),
        y10,
        fixture["seeds"]["legacy_substring_query"].as_str().unwrap(),
    );
    assert!(search["facts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|fact| fact["text"].as_str().unwrap().contains("substring needle")));

    let team_at_y10 = lab.retrieve(&lab.team(), y10);
    let conflict_groups = team_at_y10["conflicts"].as_array().unwrap();
    assert_eq!(conflict_groups.len(), 1);
    assert_eq!(
        conflict_groups[0]["claim_key"].as_str(),
        conflicts["claim_key"].as_str()
    );
    assert_eq!(conflict_groups[0]["facts"].as_array().unwrap().len(), 2);
    assert!(!current_claim_keys(&team_at_y10).contains(&"release-channel"));

    let equal_facts = lab.search(&Lab::project(), equal_stamp, "")["facts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fact| fact["updated_at"].as_str() == Some(equal_stamp))
        .count();
    assert_eq!(equal_facts, 2);
    let ordered = lab.retrieve(&Lab::project(), y10);
    let ordered_again = lab.retrieve(&Lab::project(), y10);
    assert_eq!(ordered, ordered_again);

    let mut filler = lab.stored_len(&Lab::project(), y6);
    let mut index = 0u32;
    while filler < 80 {
        let now = format!("2006-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60);
        lab.write(
            &Lab::project(),
            &now,
            caller_request(
                &format!("project-filler-{index}"),
                &format!("project filler {index}"),
                None,
                "normal",
                "low",
                agent,
            ),
        )
        .unwrap();
        filler = lab.stored_len(&Lab::project(), &now);
        index += 1;
        assert!(index < 200, "filler loop bounded");
    }
    lab.write(
        &Lab::project(),
        "2006-01-01T00:01:20.000Z",
        caller_request(
            "project-filler-overflow",
            "project filler overflow",
            None,
            "normal",
            "low",
            agent,
        ),
    )
    .unwrap();
    assert!(lab.stored_len(&Lab::project(), y6) <= 80);
    let after_overflow = lab.retrieve(&Lab::project(), y10);
    assert!(current_claim_keys(&after_overflow).contains(&"workspace-license"));

    let other = Lab {
        workspace: other_workspace.path(),
        agent,
        teams: vec![team],
    };
    for index in 0..80 {
        let now = format!("2006-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60);
        other
            .write(
                &Lab::project(),
                &now,
                caller_request(
                    &format!("other-crit-{index}"),
                    &format!("other critical {index}"),
                    Some(&format!("other-inv-{index}")),
                    "critical",
                    "high",
                    agent,
                ),
            )
            .unwrap();
    }
    let other_before = fs::read(other.path(&Lab::project())).unwrap();
    let other_capacity = other
        .write(
            &Lab::project(),
            y6,
            caller_request(
                "other-crit-80",
                "other critical overflow",
                Some("other-inv-80"),
                "critical",
                "high",
                agent,
            ),
        )
        .unwrap_err();
    assert_eq!(other_capacity["code"], "capacity");
    assert_eq!(fs::read(other.path(&Lab::project())).unwrap(), other_before);
    assert!(lab.retrieve(&Lab::project(), y10)["current"]
        .as_array()
        .unwrap()
        .iter()
        .all(|fact| !fact["text"].as_str().unwrap().starts_with("other critical")));

    let sibling = lab
        .path(&Lab::project())
        .with_extension("json.interrupted.tmp");
    fs::write(&sibling, b"partial-sibling").unwrap();
    lab.write(
        &Lab::project(),
        y8,
        caller_request(
            "after-sibling",
            "written beside an interrupted sibling",
            None,
            "normal",
            "low",
            agent,
        ),
    )
    .unwrap();
    assert_eq!(fs::read(&sibling).unwrap(), b"partial-sibling");

    let private_ack = lab
        .write(
            &lab.private(),
            y8,
            caller_request(
                "private-canary",
                SCOPE_CANARY,
                Some("canary-slot"),
                "normal",
                "low",
                agent,
            ),
        )
        .unwrap();
    let project_dump = lab.retrieve(&Lab::project(), y10).to_string();
    let team_dump = lab.retrieve(&lab.team(), y10).to_string();
    if project_dump.contains(SCOPE_CANARY) {
        leaked += 1;
    }
    if team_dump.contains(SCOPE_CANARY) {
        leaked += 1;
    }
    let private_dump = lab.retrieve(&lab.private(), y10).to_string();
    assert!(
        private_dump.contains(SCOPE_CANARY),
        "owning private scope retains the canary fact"
    );

    let oversized: String = "x".repeat(801);
    let path = lab.path(&Lab::project());
    let before_malformed = fs::read(&path).unwrap();
    let malformed = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": "malformed-oversize",
                "text": oversized,
                "extra_unknown": SCOPE_CANARY,
                "source": { "kind": "caller", "actor": agent },
            }),
        )
        .unwrap_err();
    assert_eq!(malformed["code"], "malformed");
    assert_no_canary("malformed error", &malformed.to_string());
    if fs::read(&path).unwrap() != before_malformed {
        mutation_failures += 1;
    }

    let dangling = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": "dangling-supersede",
                "text": "cyclic write",
                "supersedes": "missing-fact-id",
                "source": { "kind": "caller", "actor": agent },
            }),
        )
        .unwrap_err();
    assert_eq!(dangling["code"], "cross_scope");
    assert_no_canary("dangling supersede error", &dangling.to_string());
    if fs::read(&path).unwrap() != before_malformed {
        mutation_failures += 1;
    }

    let self_key = "self-cycle-key";
    let self_id = Lab::project()
        .preview_versioned_id(lab.workspace, Some(lab.agent), &lab.teams, self_key)
        .unwrap();
    let before_cycle = fs::read(&path).unwrap();
    let cyclic = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": self_key,
                "text": "cyclic write",
                "supersedes": self_id,
                "source": { "kind": "caller", "actor": agent },
            }),
        )
        .unwrap_err();
    assert_eq!(cyclic["code"], "cycle");
    assert_no_canary("cycle error", &cyclic.to_string());
    if fs::read(&path).unwrap() != before_cycle {
        mutation_failures += 1;
    }

    let before_cross = fs::read(&path).unwrap();
    let cross = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": "cross-scope-supersede",
                "text": "must not land",
                "supersedes": private_ack["id"],
                "source": { "kind": "caller", "actor": agent },
            }),
        )
        .unwrap_err();
    assert_eq!(cross["code"], "cross_scope");
    assert_no_canary("cross-scope error", &cross.to_string());
    if fs::read(&path).unwrap() != before_cross {
        mutation_failures += 1;
    }

    let unknown_fields = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": "unknown-field",
                "text": "ok",
                "not_a_field": SCOPE_CANARY,
                "source": { "kind": "caller" },
            }),
        )
        .unwrap_err();
    assert_eq!(unknown_fields["code"], "malformed");
    assert_no_canary("deny-unknown error", &unknown_fields.to_string());

    let interval = lab
        .write(
            &Lab::project(),
            y8,
            json!({
                "idempotency_key": "bad-interval",
                "text": "ok",
                "valid_from": y10,
                "valid_until": y0,
                "source": { "kind": "caller" },
            }),
        )
        .unwrap_err();
    assert_eq!(interval["code"], "malformed");
    if fs::read(&path).unwrap() != before_cross {
        mutation_failures += 1;
    }

    let rollback = lab.retrieve(&Lab::project(), y0);
    assert!(!current_texts(&rollback)
        .iter()
        .any(|text| *text == "written beside an interrupted sibling"));
    let jumped = lab.retrieve(&Lab::project(), y8);
    assert!(current_texts(&jumped)
        .iter()
        .any(|text| *text == "written beside an interrupted sibling"));

    let first_read = lab.retrieve(&Lab::project(), y10);
    let second_read = lab.retrieve(&Lab::project(), y10);
    assert_eq!(first_read, second_read);
    let reopened = Lab {
        workspace: workspace.path(),
        agent,
        teams: vec![team],
    };
    let third_read = reopened.retrieve(&Lab::project(), y10);
    assert_eq!(first_read, third_read);

    let mut byte_index = 0u32;
    let large = "B".repeat(800);
    loop {
        let before = lab.hot_bytes(&lab.team());
        if before + 64 >= max_bytes {
            break;
        }
        let bulky_tag = "t".repeat(64);
        let tags = vec![bulky_tag; 16];
        let result = lab.write(
            &lab.team(),
            y10,
            json!({
                "idempotency_key": format!("team-byte-{byte_index}"),
                "text": large,
                "tags": tags,
                "criticality": "normal",
                "salience": "low",
                "source": { "kind": "caller", "actor": agent },
            }),
        );
        byte_index += 1;
        if result.is_err() {
            assert_eq!(result.unwrap_err()["code"], "capacity");
            break;
        }
        if byte_index > 80 {
            break;
        }
    }
    let team_final = lab.retrieve(&lab.team(), y10);
    assert!(current_claim_keys(&team_final).contains(&"review-policy"));

    for scope in [Lab::project(), lab.private(), lab.team()] {
        assert!(lab.hot_bytes(&scope) <= max_bytes);
    }

    let project_final = lab.retrieve(&Lab::project(), y10);
    let private_final = lab.retrieve(&lab.private(), y10);
    let retained_critical = ["workspace-license", "exfil-policy", "review-policy"]
        .iter()
        .filter(|claim| {
            current_claim_keys(&project_final).contains(claim)
                || current_claim_keys(&private_final).contains(claim)
                || current_claim_keys(&team_final).contains(claim)
        })
        .count();
    assert_eq!(retained_critical, 3);

    assert_eq!(
        leaked,
        oracle["scope_canary_leakage"].as_u64().unwrap() as u32
    );
    assert_eq!(
        mutation_failures,
        oracle["malformed_cross_scope_cyclic_mutations"]
            .as_u64()
            .unwrap() as u32
    );
}
