//! Versioned, byte-bounded pages over the durable work lane.
//!
//! Scope, total order, continuation checks, and the transport budget live
//! here. MCP and both Desktop adapters call this module; they do not filter,
//! sort, or authorize a lane on their own.
//!
//! Consistency is a version fence. A continuation is bound to the digest of
//! the whole scoped lane, not only the filtered subset (identity, revision,
//! priority, creation time, state, assignment, and dependency ids). A later
//! page that observes a different digest refuses with `restart_required`
//! instead of mixing two snapshots. Graph admission time is the instant
//! sealed into the first page of a traversal, so a deadline that crosses the
//! wall clock between pages does not change membership or admission.
//!
//! Continuations are opaque HMAC tokens. The MAC key is host-owned. The
//! token body carries lane coordinates and digests, never a workspace path,
//! lease, credential, or the MAC key.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::graph::{project_scoped_graph, GraphScope, WorkGraphNode};
use super::types::{hash_payload, OrchError, OrchErrorCode};
use super::workload::{AssignmentStatus, WorkItem, WorkState};

pub const WORK_PAGE_SCHEMA_VERSION: u32 = 1;
pub const WORK_LIST_PAGE_KIND: &str = "grokptah.work.page.v1";
pub const WORK_GRAPH_PAGE_KIND: &str = "grokptah.work.graph-page.v1";
pub const WORK_DETAIL_KIND: &str = "grokptah.work.detail.v1";
pub const WORK_PAGE_ORDER_V1: &str = "priority_desc_created_at_asc_work_id_asc";
pub const WORK_PAGE_CONSISTENCY: &str = "version_fence";
pub const DEFAULT_WORK_PAGE_LIMIT: usize = 50;
pub const MAX_WORK_PAGE_LIMIT: usize = 200;
/// Soft target. A single legal entry may exceed it when that entry still fits
/// under the hard transport ceiling.
pub const PREFERRED_WORK_PAGE_BYTES: usize = 256 * 1024;
pub const WORK_PAGE_CURSOR_TTL_SECS: i64 = 15 * 60;
pub const MAX_WORK_CURSOR_CHARS: usize = 4_096;
pub const DEFAULT_WORK_DETAIL_LIMIT: usize = 32;
pub const MAX_WORK_DETAIL_LIMIT: usize = 64;
/// Hard ceiling for one JSON-RPC `tools/call` body. Kept equal to
/// `MAX_MCP_RESPONSE_BODY_BYTES`. Do not raise it to hide an oversized lane.
pub const WORK_PAGE_TRANSPORT_CEILING_BYTES: usize = 2 * 1024 * 1024;

const CURSOR_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkPageReader {
    pub owner_id: String,
    pub token_id: String,
    pub session_id: Uuid,
    pub workspace_digest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkPageFilter {
    /// Empty means the whole lane. Values are matched exactly. Stored in
    /// serde-name order so the digest does not depend on caller order.
    pub states: Vec<WorkState>,
}

impl WorkPageFilter {
    pub fn from_states(states: impl IntoIterator<Item = WorkState>) -> Self {
        let mut states = states.into_iter().collect::<Vec<_>>();
        states.sort_by(|left, right| {
            serde_json::to_string(left)
                .unwrap_or_default()
                .cmp(&serde_json::to_string(right).unwrap_or_default())
        });
        states.dedup();
        Self { states }
    }

    fn digest(&self) -> String {
        let names = self
            .states
            .iter()
            .map(|state| serde_json::to_string(state).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(",");
        hash_payload(&json!(names))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPageKind {
    List,
    Graph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkListEntryV1 {
    pub work_id: String,
    pub kind: String,
    pub state: WorkState,
    pub priority: i32,
    pub revision: u64,
    pub attempt_count: u32,
    pub assignment_status: AssignmentStatus,
    pub is_container: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_agent_id: Option<String>,
    /// The entry was reduced to identity fields so the page could advance.
    /// The objective and progress remain available from the detail contract.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detail_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkListPageV1 {
    pub schema_version: u32,
    pub kind: String,
    pub order: String,
    pub consistency: String,
    /// True only when this response finishes the filtered lane.
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
    pub items: Vec<WorkListEntryV1>,
    /// Items in this page. This is not a lane total unless `complete` is true
    /// and the caller has followed every prior page.
    pub page_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkGraphPageV1 {
    pub schema_version: u32,
    pub kind: String,
    pub order: String,
    pub consistency: String,
    /// Admission instant sealed by the first page of this traversal.
    pub admitted_at: DateTime<Utc>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
    pub nodes: Vec<WorkGraphNode>,
    pub page_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkDetailSection {
    Summary,
    Objective,
    Progress,
    Result,
    Attempts,
    Policy,
    Dependencies,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDetailPageV1 {
    pub schema_version: u32,
    pub kind: String,
    pub work_id: String,
    pub revision: u64,
    pub section: WorkDetailSection,
    pub offset: usize,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub body: Value,
}

#[derive(Debug, Clone)]
pub struct WorkPageQuery {
    pub limit: usize,
    pub cursor: Option<String>,
    pub filter: WorkPageFilter,
    pub kind: WorkPageKind,
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorClaims {
    v: u32,
    kind: String,
    session_id: Uuid,
    workspace_digest: String,
    owner_id: String,
    token_id: String,
    filter_digest: String,
    order: String,
    consistency: String,
    lane_digest: String,
    admitted_at: DateTime<Utc>,
    after_priority: i32,
    after_created_at: DateTime<Utc>,
    after_work_id: String,
    exp: i64,
}

pub fn workspace_digest(canonical_workspace: &str) -> String {
    hash_payload(&json!(canonical_workspace))
}

pub fn lane_digest(items: &[WorkItem]) -> String {
    let mut lines = String::new();
    for item in items {
        let deps = item
            .dependencies
            .iter()
            .map(|dependency| dependency.work_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        lines.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            item.work_id,
            item.revision,
            item.priority,
            item.created_at.to_rfc3339(),
            serde_json::to_string(&item.state).unwrap_or_default(),
            serde_json::to_string(&item.assignment_status).unwrap_or_default(),
            deps
        ));
    }
    hash_payload(&Value::String(lines))
}

/// JSON-RPC `tools/call` body as axum serializes `JsonRpcResp` around the
/// tool result. The page budget counts these bytes, including the pretty-text
/// duplicate of `structuredContent`.
pub fn encoded_tools_call_response(id: &Value, structured: &Value) -> Vec<u8> {
    let pretty = serde_json::to_string_pretty(structured).unwrap_or_default();
    let result = json!({
        "content": [{ "type": "text", "text": pretty }],
        "structuredContent": structured,
        "isError": false,
    });
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    serde_json::to_vec(&response).unwrap_or_default()
}

pub fn tools_call_response_len(structured: &Value) -> usize {
    // A u64 id is the largest numeric id the in-tree client emits. Budgeting
    // against it keeps a smaller real id inside the same ceiling.
    encoded_tools_call_response(&json!(u64::MAX), structured).len()
}

fn restart(message: impl Into<String>) -> OrchError {
    OrchError::with_data(
        OrchErrorCode::RestartRequired,
        message,
        json!({ "restartRequired": true }),
    )
}

fn invalid_cursor() -> OrchError {
    OrchError::new(
        OrchErrorCode::InvalidRequest,
        "work page continuation is not valid",
    )
}

pub fn validate_page_limit(limit: usize) -> Result<(), OrchError> {
    if limit == 0 || limit > MAX_WORK_PAGE_LIMIT {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("limit must be between 1 and {MAX_WORK_PAGE_LIMIT}"),
        ));
    }
    Ok(())
}

pub fn select_work_list_page(
    ordered_lane: &[WorkItem],
    query: &WorkPageQuery,
    reader: &WorkPageReader,
    mac_key: &[u8],
    now: DateTime<Utc>,
    max_response_bytes: usize,
) -> Result<WorkListPageV1, OrchError> {
    validate_page_limit(query.limit)?;
    if query.kind != WorkPageKind::List {
        return Err(invalid_cursor());
    }
    // The fence covers the whole scoped lane, including items the filter
    // hides. A dependency or priority change outside the filter still restarts.
    let digest = lane_digest(ordered_lane);
    let filtered = filter_lane(ordered_lane, &query.filter);
    let (start, admitted_at) = page_start(&filtered, query, reader, mac_key, now, &digest)?;
    let entries = filtered
        .iter()
        .skip(start)
        .map(|item| WorkListEntryV1::full(item))
        .collect::<Vec<_>>();
    let chosen = pack_list_entries(&entries, query.limit, max_response_bytes)?;
    let taken = chosen.len();
    let complete = start + taken >= filtered.len();
    let continuation = if complete {
        None
    } else {
        let anchor = &filtered[start + taken - 1];
        Some(seal_cursor(
            mac_key,
            &cursor_claims(
                WorkPageKind::List,
                reader,
                &query.filter,
                &digest,
                admitted_at,
                anchor,
                now,
            ),
        )?)
    };
    let page = WorkListPageV1 {
        schema_version: WORK_PAGE_SCHEMA_VERSION,
        kind: WORK_LIST_PAGE_KIND.to_string(),
        order: WORK_PAGE_ORDER_V1.to_string(),
        consistency: WORK_PAGE_CONSISTENCY.to_string(),
        complete,
        continuation,
        page_count: chosen.len(),
        items: chosen,
    };
    confirm_budget(&page, max_response_bytes)?;
    Ok(page)
}

pub fn select_work_graph_page(
    ordered_lane: &[WorkItem],
    scope: GraphScope<'_>,
    query: &WorkPageQuery,
    reader: &WorkPageReader,
    mac_key: &[u8],
    now: DateTime<Utc>,
    max_response_bytes: usize,
) -> Result<WorkGraphPageV1, OrchError> {
    validate_page_limit(query.limit)?;
    if query.kind != WorkPageKind::Graph {
        return Err(invalid_cursor());
    }
    let digest = lane_digest(ordered_lane);
    let filtered = filter_lane(ordered_lane, &query.filter);
    let (start, admitted_at) = page_start(&filtered, query, reader, mac_key, now, &digest)?;
    // Project the whole scoped lane at the sealed instant. An in-lane target
    // on another page, or outside this state filter, keeps its work id. A
    // foreign id stays an unresolved count.
    let projected = project_scoped_graph(ordered_lane, scope, admitted_at);
    let by_id = projected
        .into_iter()
        .map(|node| (node.work_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::new();
    for item in filtered.iter().skip(start) {
        let Some(node) = by_id.get(&item.work_id) else {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "work graph page omitted a lane item",
            ));
        };
        nodes.push(node.clone());
    }
    let chosen = pack_values(
        WorkPageKind::Graph,
        &nodes,
        query.limit,
        max_response_bytes,
        |node| json!(node),
        |node| json!(node),
    )?;
    let taken = chosen.len();
    let complete = start + taken >= filtered.len();
    let continuation = if complete {
        None
    } else {
        let anchor = &filtered[start + taken - 1];
        Some(seal_cursor(
            mac_key,
            &cursor_claims(
                WorkPageKind::Graph,
                reader,
                &query.filter,
                &digest,
                admitted_at,
                anchor,
                now,
            ),
        )?)
    };
    let page = WorkGraphPageV1 {
        schema_version: WORK_PAGE_SCHEMA_VERSION,
        kind: WORK_GRAPH_PAGE_KIND.to_string(),
        order: WORK_PAGE_ORDER_V1.to_string(),
        consistency: WORK_PAGE_CONSISTENCY.to_string(),
        admitted_at,
        complete,
        continuation,
        page_count: chosen.len(),
        nodes: chosen,
    };
    confirm_budget(&page, max_response_bytes)?;
    Ok(page)
}

fn confirm_budget<T: Serialize>(page: &T, max_response_bytes: usize) -> Result<(), OrchError> {
    let value = serde_json::to_value(page)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    let len = tools_call_response_len(&value);
    if len > max_response_bytes {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "work page exceeded the transport ceiling",
        ));
    }
    Ok(())
}

fn filter_lane<'a>(items: &'a [WorkItem], filter: &WorkPageFilter) -> Vec<&'a WorkItem> {
    items
        .iter()
        .filter(|item| filter.states.is_empty() || filter.states.contains(&item.state))
        .collect()
}

fn page_start(
    filtered: &[&WorkItem],
    query: &WorkPageQuery,
    reader: &WorkPageReader,
    mac_key: &[u8],
    now: DateTime<Utc>,
    digest: &str,
) -> Result<(usize, DateTime<Utc>), OrchError> {
    let Some(token) = query.cursor.as_deref() else {
        return Ok((0, now));
    };
    let claims = open_cursor(token, mac_key, reader, &query.filter, query.kind, now)?;
    if claims.lane_digest != digest {
        return Err(restart("the work lane changed; restart the page traversal"));
    }
    let start = filtered
        .iter()
        .position(|item| is_strictly_after(item, &claims))
        .unwrap_or(filtered.len());
    Ok((start, claims.admitted_at))
}

fn is_strictly_after(item: &WorkItem, claims: &CursorClaims) -> bool {
    let anchor_order = (
        std::cmp::Reverse(claims.after_priority),
        claims.after_created_at,
        claims.after_work_id.as_str(),
    );
    let item_order = (
        std::cmp::Reverse(item.priority),
        item.created_at,
        item.work_id.as_str(),
    );
    item_order > anchor_order
}

fn pack_values<T: Clone>(
    kind: WorkPageKind,
    entries: &[T],
    limit: usize,
    max_response_bytes: usize,
    full_json: impl Fn(&T) -> Value,
    minimal_json: impl Fn(&T) -> Value,
) -> Result<Vec<T>, OrchError> {
    let mut chosen: Vec<T> = Vec::new();
    let mut encoded: Vec<Value> = Vec::new();
    for entry in entries {
        if chosen.len() == limit {
            break;
        }
        let mut trial = encoded.clone();
        trial.push(full_json(entry));
        if page_fits(kind, &trial, max_response_bytes, chosen.is_empty()) {
            chosen.push(entry.clone());
            encoded = trial;
            continue;
        }
        if chosen.is_empty() {
            let minimal = vec![minimal_json(entry)];
            if page_fits(kind, &minimal, max_response_bytes, true) {
                chosen.push(entry.clone());
                encoded = minimal;
                continue;
            }
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "a single work page entry exceeds the transport ceiling",
            ));
        }
        break;
    }
    if encoded.len() != chosen.len() {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "work page pack diverged",
        ));
    }
    Ok(chosen)
}

fn page_shell(kind: WorkPageKind, items: &[Value]) -> Value {
    let continuation = "a".repeat(MAX_WORK_CURSOR_CHARS);
    match kind {
        WorkPageKind::List => json!({
            "schemaVersion": WORK_PAGE_SCHEMA_VERSION,
            "kind": WORK_LIST_PAGE_KIND,
            "order": WORK_PAGE_ORDER_V1,
            "consistency": WORK_PAGE_CONSISTENCY,
            "complete": false,
            "continuation": continuation,
            "items": items,
            "pageCount": items.len(),
        }),
        WorkPageKind::Graph => json!({
            "schemaVersion": WORK_PAGE_SCHEMA_VERSION,
            "kind": WORK_GRAPH_PAGE_KIND,
            "order": WORK_PAGE_ORDER_V1,
            "consistency": WORK_PAGE_CONSISTENCY,
            "admittedAt": "1970-01-01T00:00:00Z",
            "complete": false,
            "continuation": continuation,
            "nodes": items,
            "pageCount": items.len(),
        }),
    }
}

fn page_fits(
    kind: WorkPageKind,
    items: &[Value],
    max_response_bytes: usize,
    allow_hard: bool,
) -> bool {
    let len = tools_call_response_len(&page_shell(kind, items));
    if len <= PREFERRED_WORK_PAGE_BYTES {
        return true;
    }
    allow_hard && items.len() == 1 && len <= max_response_bytes
}

impl WorkListEntryV1 {
    fn full(item: &WorkItem) -> Self {
        Self {
            work_id: item.work_id.clone(),
            kind: item.kind.clone(),
            state: item.state,
            priority: item.priority,
            revision: item.revision,
            attempt_count: item.attempt_count,
            assignment_status: item.assignment_status,
            is_container: item.is_container,
            created_at: item.created_at,
            updated_at: item.updated_at,
            deadline: item.deadline,
            objective: Some(item.objective.clone()),
            progress_summary: item
                .progress
                .as_ref()
                .map(|progress| progress.summary.clone()),
            progress_percent: item.progress.as_ref().and_then(|progress| progress.percent),
            assigned_agent_id: item.assigned_agent_id.clone(),
            detail_required: false,
        }
    }

    fn minimal(&self) -> Self {
        Self {
            objective: None,
            progress_summary: None,
            progress_percent: None,
            detail_required: true,
            ..self.clone()
        }
    }
}

/// Pack list entries, shrinking an oversized first entry instead of stalling.
pub fn pack_list_entries(
    entries: &[WorkListEntryV1],
    limit: usize,
    max_response_bytes: usize,
) -> Result<Vec<WorkListEntryV1>, OrchError> {
    let mut chosen = Vec::new();
    for entry in entries {
        if chosen.len() == limit {
            break;
        }
        let mut trial = chosen.clone();
        trial.push(entry.clone());
        if page_fits(
            WorkPageKind::List,
            &trial.iter().map(|item| json!(item)).collect::<Vec<_>>(),
            max_response_bytes,
            chosen.is_empty(),
        ) {
            chosen.push(entry.clone());
            continue;
        }
        if chosen.is_empty() {
            // The full entry did not fit. The identity projection is its own
            // page so a later small item cannot make this page look complete
            // while the oversized objective is still unread.
            let minimal = entry.minimal();
            if page_fits(
                WorkPageKind::List,
                &[json!(minimal.clone())],
                max_response_bytes,
                true,
            ) {
                chosen.push(minimal);
                break;
            }
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "a single work page entry exceeds the transport ceiling",
            ));
        }
        break;
    }
    Ok(chosen)
}

fn cursor_claims(
    kind: WorkPageKind,
    reader: &WorkPageReader,
    filter: &WorkPageFilter,
    digest: &str,
    admitted_at: DateTime<Utc>,
    anchor: &WorkItem,
    now: DateTime<Utc>,
) -> CursorClaims {
    CursorClaims {
        v: CURSOR_VERSION,
        kind: match kind {
            WorkPageKind::List => "list",
            WorkPageKind::Graph => "graph",
        }
        .to_string(),
        session_id: reader.session_id,
        workspace_digest: reader.workspace_digest.clone(),
        owner_id: reader.owner_id.clone(),
        token_id: reader.token_id.clone(),
        filter_digest: filter.digest(),
        order: WORK_PAGE_ORDER_V1.to_string(),
        consistency: WORK_PAGE_CONSISTENCY.to_string(),
        lane_digest: digest.to_string(),
        admitted_at,
        after_priority: anchor.priority,
        after_created_at: anchor.created_at,
        after_work_id: anchor.work_id.clone(),
        exp: now.timestamp().saturating_add(WORK_PAGE_CURSOR_TTL_SECS),
    }
}

fn seal_cursor(mac_key: &[u8], claims: &CursorClaims) -> Result<String, OrchError> {
    let body = serde_json::to_vec(claims)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    let mac = hmac_sha256(mac_key, &body);
    let token = format!("{}.{}", base64url(&body), base64url(&mac));
    if token.len() > MAX_WORK_CURSOR_CHARS {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "work page continuation exceeded its bound",
        ));
    }
    Ok(token)
}

fn open_cursor(
    token: &str,
    mac_key: &[u8],
    reader: &WorkPageReader,
    filter: &WorkPageFilter,
    kind: WorkPageKind,
    now: DateTime<Utc>,
) -> Result<CursorClaims, OrchError> {
    if token.len() > MAX_WORK_CURSOR_CHARS || token.contains('\0') {
        return Err(invalid_cursor());
    }
    let Some((body_b64, mac_b64)) = token.split_once('.') else {
        return Err(invalid_cursor());
    };
    if body_b64.is_empty() || mac_b64.is_empty() || token.matches('.').count() != 1 {
        return Err(invalid_cursor());
    }
    let Ok(body) = base64url_decode(body_b64) else {
        return Err(invalid_cursor());
    };
    let Ok(presented) = base64url_decode(mac_b64) else {
        return Err(invalid_cursor());
    };
    let expected = hmac_sha256(mac_key, &body);
    if !super::authz::constant_time_eq(&expected, &presented) {
        return Err(invalid_cursor());
    }
    let claims: CursorClaims = serde_json::from_slice(&body).map_err(|_| invalid_cursor())?;
    if claims.v != CURSOR_VERSION
        || claims.order != WORK_PAGE_ORDER_V1
        || claims.consistency != WORK_PAGE_CONSISTENCY
    {
        return Err(invalid_cursor());
    }
    let kind_name = match kind {
        WorkPageKind::List => "list",
        WorkPageKind::Graph => "graph",
    };
    if claims.kind != kind_name
        || claims.session_id != reader.session_id
        || claims.workspace_digest != reader.workspace_digest
        || claims.owner_id != reader.owner_id
        || claims.token_id != reader.token_id
        || claims.filter_digest != filter.digest()
    {
        return Err(invalid_cursor());
    }
    if now.timestamp() >= claims.exp {
        return Err(OrchError::with_data(
            OrchErrorCode::CursorExpired,
            "work page continuation expired; restart the traversal",
            json!({ "restartRequired": true }),
        ));
    }
    Ok(claims)
}

/// Issue a continuation for tests and for callers that already hold the host
/// key. Production reads go through [`select_work_list_page`].
#[allow(clippy::too_many_arguments)]
pub fn issue_cursor_for_tests(
    mac_key: &[u8],
    reader: &WorkPageReader,
    filter: &WorkPageFilter,
    kind: WorkPageKind,
    digest: &str,
    admitted_at: DateTime<Utc>,
    anchor: &WorkItem,
    now: DateTime<Utc>,
    exp: DateTime<Utc>,
) -> Result<String, OrchError> {
    let mut claims = cursor_claims(kind, reader, filter, digest, admitted_at, anchor, now);
    claims.exp = exp.timestamp();
    seal_cursor(mac_key, &claims)
}

/// A host-signed continuation whose schema version is not the current one.
/// Production reads must reject it before returning lane data.
#[allow(clippy::too_many_arguments)]
pub fn issue_unknown_version_cursor_for_tests(
    mac_key: &[u8],
    reader: &WorkPageReader,
    filter: &WorkPageFilter,
    kind: WorkPageKind,
    digest: &str,
    admitted_at: DateTime<Utc>,
    anchor: &WorkItem,
    now: DateTime<Utc>,
) -> Result<String, OrchError> {
    let mut claims = cursor_claims(kind, reader, filter, digest, admitted_at, anchor, now);
    claims.v = CURSOR_VERSION.saturating_add(1);
    seal_cursor(mac_key, &claims)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= padded[index];
        opad[index] ^= padded[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let chunk = ((bytes[index] as u32) << 16)
            | ((bytes[index + 1] as u32) << 8)
            | bytes[index + 2] as u32;
        out.push(TABLE[((chunk >> 18) & 63) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 63) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 63) as usize] as char);
        out.push(TABLE[(chunk & 63) as usize] as char);
        index += 3;
    }
    if index < bytes.len() {
        let mut chunk = (bytes[index] as u32) << 16;
        out.push(TABLE[((chunk >> 18) & 63) as usize] as char);
        if index + 1 < bytes.len() {
            chunk |= (bytes[index + 1] as u32) << 8;
            out.push(TABLE[((chunk >> 12) & 63) as usize] as char);
            out.push(TABLE[((chunk >> 6) & 63) as usize] as char);
        } else {
            out.push(TABLE[((chunk >> 12) & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(text: &str) -> Result<Vec<u8>, ()> {
    fn value(byte: u8) -> Result<u8, ()> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(()),
        }
    }
    if text.chars().any(|ch| ch == '=') {
        return Err(());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let remaining = bytes.len() - index;
        if remaining == 1 {
            return Err(());
        }
        let b0 = value(bytes[index])?;
        let b1 = value(bytes[index + 1])?;
        out.push((b0 << 2) | (b1 >> 4));
        if remaining == 2 {
            break;
        }
        let b2 = value(bytes[index + 2])?;
        out.push((b1 << 4) | (b2 >> 2));
        if remaining == 3 {
            break;
        }
        let b3 = value(bytes[index + 3])?;
        out.push((b2 << 6) | b3);
        index += 4;
    }
    Ok(out)
}

/// Bounded section of one authorized work item. Collections page by offset.
/// A section that cannot fit even one element returns `capacity_exhausted`
/// rather than a truncated element.
pub fn project_work_detail(
    item: &WorkItem,
    attempts_json: &[Value],
    section: WorkDetailSection,
    offset: usize,
    limit: usize,
    max_response_bytes: usize,
) -> Result<WorkDetailPageV1, OrchError> {
    if limit == 0 || limit > MAX_WORK_DETAIL_LIMIT {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("limit must be between 1 and {MAX_WORK_DETAIL_LIMIT}"),
        ));
    }
    let (body, next_offset, complete) = match section {
        WorkDetailSection::Summary => (
            json!({
                "workId": item.work_id,
                "kind": item.kind,
                "state": item.state,
                "priority": item.priority,
                "revision": item.revision,
                "attemptCount": item.attempt_count,
                "objectiveBytes": item.objective.len(),
                "progressBytes": item.progress.as_ref().map(|progress| progress.summary.len()).unwrap_or(0),
                "evidenceCount": item.result.as_ref().map(|result| result.evidence.len()).unwrap_or(0),
                "artifactCount": item.result.as_ref().map(|result| result.artifacts.len()).unwrap_or(0),
                "attemptRecordCount": attempts_json.len(),
                "dependencyCount": item.dependencies.len(),
            }),
            None,
            true,
        ),
        WorkDetailSection::Objective => (json!({ "objective": item.objective }), None, true),
        WorkDetailSection::Progress => (json!({ "progress": item.progress }), None, true),
        WorkDetailSection::Policy => (json!({ "policy": item.policy }), None, true),
        WorkDetailSection::Dependencies => {
            let end = offset.saturating_add(limit).min(item.dependencies.len());
            if offset > item.dependencies.len() {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "detail offset is past the end of the collection",
                ));
            }
            let slice = &item.dependencies[offset..end];
            let complete = end == item.dependencies.len();
            (
                json!({ "dependencies": slice }),
                (!complete).then_some(end),
                complete,
            )
        }
        WorkDetailSection::Result => {
            let evidence = item
                .result
                .as_ref()
                .map(|result| result.evidence.as_slice())
                .unwrap_or(&[]);
            let artifacts = item
                .result
                .as_ref()
                .map(|result| result.artifacts.as_slice())
                .unwrap_or(&[]);
            let total = evidence.len().saturating_add(artifacts.len());
            if offset > total {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "detail offset is past the end of the collection",
                ));
            }
            let mut taken_evidence = Vec::new();
            let mut taken_artifacts = Vec::new();
            let mut index = offset;
            let mut count = 0;
            while index < total && count < limit {
                if index < evidence.len() {
                    taken_evidence.push(evidence[index].clone());
                } else {
                    taken_artifacts.push(artifacts[index - evidence.len()].clone());
                }
                index += 1;
                count += 1;
                let trial = WorkDetailPageV1 {
                    schema_version: WORK_PAGE_SCHEMA_VERSION,
                    kind: WORK_DETAIL_KIND.to_string(),
                    work_id: item.work_id.clone(),
                    revision: item.revision,
                    section,
                    offset,
                    complete: index == total,
                    next_offset: (index != total).then_some(index),
                    body: json!({
                        "summary": item.result.as_ref().map(|result| result.summary.clone()),
                        "failure": item.result.as_ref().and_then(|result| result.failure.clone()),
                        "evidence": taken_evidence,
                        "artifacts": taken_artifacts,
                    }),
                };
                if tools_call_response_len(&json!(trial)) > max_response_bytes {
                    if count == 1 {
                        return Err(OrchError::new(
                            OrchErrorCode::CapacityExhausted,
                            "a single work detail element exceeds the transport ceiling",
                        ));
                    }
                    index -= 1;
                    if index < evidence.len() {
                        taken_evidence.pop();
                    } else {
                        taken_artifacts.pop();
                    }
                    break;
                }
            }
            let complete = index >= total;
            (
                json!({
                    "summary": item.result.as_ref().map(|result| result.summary.clone()),
                    "failure": item.result.as_ref().and_then(|result| result.failure.clone()),
                    "evidence": taken_evidence,
                    "artifacts": taken_artifacts,
                }),
                (!complete).then_some(index),
                complete,
            )
        }
        WorkDetailSection::Attempts => {
            if offset > attempts_json.len() {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "detail offset is past the end of the collection",
                ));
            }
            let mut index = offset;
            let mut taken = Vec::new();
            while index < attempts_json.len() && taken.len() < limit {
                taken.push(attempts_json[index].clone());
                index += 1;
                let trial_complete = index == attempts_json.len();
                let trial = json!({
                    "schemaVersion": WORK_PAGE_SCHEMA_VERSION,
                    "kind": WORK_DETAIL_KIND,
                    "workId": item.work_id,
                    "revision": item.revision,
                    "section": "attempts",
                    "offset": offset,
                    "complete": trial_complete,
                    "nextOffset": if trial_complete { Value::Null } else { json!(index) },
                    "body": { "attempts": taken },
                });
                if tools_call_response_len(&trial) > max_response_bytes {
                    if taken.len() == 1 {
                        return Err(OrchError::new(
                            OrchErrorCode::CapacityExhausted,
                            "a single work detail element exceeds the transport ceiling",
                        ));
                    }
                    taken.pop();
                    index -= 1;
                    break;
                }
            }
            let complete = index >= attempts_json.len();
            (
                json!({ "attempts": taken }),
                (!complete).then_some(index),
                complete,
            )
        }
    };
    let page = WorkDetailPageV1 {
        schema_version: WORK_PAGE_SCHEMA_VERSION,
        kind: WORK_DETAIL_KIND.to_string(),
        work_id: item.work_id.clone(),
        revision: item.revision,
        section,
        offset,
        complete,
        next_offset,
        body,
    };
    confirm_budget(&page, max_response_bytes)?;
    Ok(page)
}

pub fn legacy_payload_within_ceiling(payload: &Value, max_response_bytes: usize) -> bool {
    tools_call_response_len(payload) <= max_response_bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workload::WorkPolicy;

    fn item(id: &str, priority: i32, at: &str) -> WorkItem {
        let mut item = WorkItem::new(
            "test",
            "objective",
            Uuid::nil(),
            "/tmp/lane",
            "operator",
            WorkPolicy::default(),
        )
        .unwrap();
        item.work_id = id.to_string();
        item.priority = priority;
        let stamp: DateTime<Utc> = at.parse().unwrap();
        item.created_at = stamp;
        item.updated_at = stamp;
        item
    }

    fn reader() -> WorkPageReader {
        WorkPageReader {
            owner_id: "owner".into(),
            token_id: "token".into(),
            session_id: Uuid::nil(),
            workspace_digest: workspace_digest("/tmp/lane"),
        }
    }

    fn query(kind: WorkPageKind, limit: usize, cursor: Option<String>) -> WorkPageQuery {
        WorkPageQuery {
            limit,
            cursor,
            filter: WorkPageFilter::default(),
            kind,
        }
    }

    #[test]
    fn ties_traverse_once_in_total_order() {
        let key = b"host-owned-page-key______________";
        let now = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut lane = vec![
            item("b", 1, "2026-01-01T00:00:00Z"),
            item("a", 1, "2026-01-01T00:00:00Z"),
            item("c", 2, "2026-01-02T00:00:00Z"),
            item("d", 1, "2026-01-01T00:00:01Z"),
        ];
        crate::orchestration::order_work(&mut lane);
        let first = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 2, None),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert!(!first.complete);
        assert_eq!(
            first
                .items
                .iter()
                .map(|entry| entry.work_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "a"]
        );
        let second = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 2, first.continuation.clone()),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert!(second.complete);
        assert_eq!(
            second
                .items
                .iter()
                .map(|entry| entry.work_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "d"]
        );
        let mut seen = first
            .items
            .iter()
            .chain(second.items.iter())
            .map(|entry| entry.work_id.clone())
            .collect::<Vec<_>>();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn tamper_expiry_and_foreign_reader_refuse_before_a_page() {
        let key = b"host-owned-page-key______________";
        let now = "2026-01-01T00:00:00Z".parse().unwrap();
        let lane = vec![item("a", 1, "2026-01-01T00:00:00Z")];
        let page = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 1, None),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert!(page.complete);
        let mut other = reader();
        other.token_id = "other".into();
        let foreign = issue_cursor_for_tests(
            key,
            &other,
            &WorkPageFilter::default(),
            WorkPageKind::List,
            &lane_digest(&lane),
            now,
            &lane[0],
            now,
            "2026-01-01T00:20:00Z".parse().unwrap(),
        )
        .unwrap();
        let foreign = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 1, Some(foreign)),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap_err();
        assert_eq!(foreign.code, OrchErrorCode::InvalidRequest);
        assert_eq!(foreign.message, "work page continuation is not valid");
        let bogus = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 1, Some("not-a-cursor".into())),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        );
        assert_eq!(bogus.unwrap_err().code, OrchErrorCode::InvalidRequest);
        let mut flipped = page.continuation.clone().unwrap_or_default();
        if flipped.is_empty() {
            // A one-item lane is terminal and has no continuation. Mint one.
            flipped = issue_cursor_for_tests(
                key,
                &reader(),
                &WorkPageFilter::default(),
                WorkPageKind::List,
                &lane_digest(&lane),
                now,
                &lane[0],
                now,
                "2026-01-01T00:20:00Z".parse().unwrap(),
            )
            .unwrap();
        }
        let mut chars = flipped.chars().collect::<Vec<_>>();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        let refused = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 1, Some(tampered)),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        );
        assert_eq!(refused.unwrap_err().code, OrchErrorCode::InvalidRequest);
        let expired = issue_cursor_for_tests(
            key,
            &reader(),
            &WorkPageFilter::default(),
            WorkPageKind::List,
            &lane_digest(&lane),
            now,
            &lane[0],
            now,
            "2020-01-01T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let expired = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 1, Some(expired)),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        );
        assert_eq!(expired.unwrap_err().code, OrchErrorCode::CursorExpired);
    }

    #[test]
    fn an_oversized_entry_still_advances_and_a_changed_lane_restarts() {
        let key = b"host-owned-page-key______________";
        let now = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut huge = item("huge", 1, "2026-01-01T00:00:00Z");
        // Larger than the hard envelope once the tools/call body repeats it.
        huge.objective = "雪".repeat(900_000);
        let mut next = item("next", 0, "2026-01-01T00:00:01Z");
        next.objective = "plain".into();
        let lane = vec![huge, next];
        let page = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 50, None),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert!(!page.complete);
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].detail_required);
        assert!(page.items[0].objective.is_none());
        assert!(page.continuation.is_some());
        assert!(tools_call_response_len(&json!(page)) <= 2 * 1024 * 1024);
        let rest = select_work_list_page(
            &lane,
            &query(WorkPageKind::List, 50, page.continuation.clone()),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        )
        .unwrap();
        assert!(rest.complete);
        assert_eq!(rest.items[0].work_id, "next");
        let mut changed = lane.clone();
        changed[1].priority = 9;
        changed[1].revision = 2;
        let restarted = select_work_list_page(
            &changed,
            &query(WorkPageKind::List, 50, page.continuation),
            &reader(),
            key,
            now,
            2 * 1024 * 1024,
        );
        assert_eq!(restarted.unwrap_err().code, OrchErrorCode::RestartRequired);
    }
}
