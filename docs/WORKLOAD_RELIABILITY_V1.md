# Workload reliability v1

Baseline `c2b1c0b0ce4f968238d722d2ecccd2456a30f212` (tree `2c3f278fafe2e440e40d89dfcd66e13bc3e83cc3`) matches the 2026-10-11 planning audit. This document is the acceptance matrix for issues #520 and #521. It does not close either issue.

## Red capacity observation

Command, from the bridge package, on that base plus a harness-only plant of 4096 legal items with 512-byte objectives:

`cargo test --locked --test work_graph_service legal_ceiling_list_and_graph_exceed_the_mcp_body_ceiling -- --test-threads=1 --exact`

Result: exit 0, the one test passed in 201.78s. Both `ptah_list_work` and `ptah_get_work_graph` returned an error. The production handlers do not emit `capacity_exhausted` for a lane of exactly 4096 items (that code is the 4097 refusal). The assertion therefore matched the client error `MCP response exceeded its byte ceiling` (`MAX_MCP_RESPONSE_BODY_BYTES` = 2 MiB). The server still built the full unpaged body. About three minutes of that run sat in `project_scoped_graph` → `resolve_dependency_states` → `workspaces_match`, which canonicalizes once per item per node when the request path and the stored path are not byte-identical (`/var` versus `/private/var`). That is a production cost on a legal lane, not a harness-only miss. The first plant attempt failed before any read because it wrote `work-items` before `OrchStore::open` created the directory. That was a test-depth miss and was rerun.

## Contract chosen before the product change

- One runtime selector. MCP and both Desktop adapters call it. They do not sort, filter, or authorize on their own.
- Order stays priority descending, `created_at` ascending, `work_id` ascending.
- Consistency is a version fence. The cursor binds a digest of the whole scoped lane (id, revision, priority, creation time, state, assignment, dependency ids), the filter digest, the sealed graph admission instant, and the reader. A mismatch returns `restart_required` and no page. Deadline admission uses the sealed instant.
- Continuations are HMAC-SHA256 tokens. The 32-byte key is created in the ledger as `page-mac.key` (mode 0600) and is not a caller checksum. The token body has no workspace path, lease, or credential.
- Legacy `ptah_list_work`, `ptah_get_work_graph`, and `ptah_get_work` keep their response shapes. If the JSON-RPC tools/call envelope would exceed 2 MiB they return `capacity_exhausted` and name the paged tool. They do not truncate.
- Additive tools: `ptah_list_work_page`, `ptah_get_work_graph_page`, `ptah_get_work_detail`.
- Default page limit 50, maximum 200. Preferred envelope 256 KiB. A single entry may use the 2 MiB hard ceiling. A nonterminal page has at least one entry and a strictly later continuation.
- List pages project `WorkListEntryV1`. Graph nodes stay the existing redacted `WorkGraphNode`. Detail sections page collections without cutting an element in half.
- Embedded Desktop reader identity is the existing `local-desktop` origin. Service cursors bind `AuthContext` owner and token ids, so a rotated bearer does not continue a page.
- Embedded `list_work_page_for_session` returns the orchestration error display, which includes `restart_required` or `cursor_expired`. The Work Board matches those codes.
- Detail `summary` carries the non-bulky operator identity (session, workspace, creator, assignment, approval, timestamps, assignment status). Objective, progress, policy, dependencies, results, and attempts stay in their sections. A remote get that returns `capacity_exhausted` is assembled from those sections. A single element that still does not fit stays `capacity_exhausted`.

## Acceptance matrix

| Requirement | Production caller | Baseline evidence | Gap | Proof |
| --- | --- | --- | --- | --- |
| Traverse 0 through 4096 items | `list_work_scoped`, `get_work_graph_scoped`, MCP tools | Legal 4096 exceeds 2 MiB; 4097 already returns `capacity_exhausted` | No continuation | Page the lane over MCP and assert every id once |
| Keyset order, including ties | `order_work` | Total order exists; the public tool returns the whole array | No cursor | Unchanged-lane traversal has no missing or repeated ids |
| Opaque authenticated continuation | none | Scope is the only argument | Caller could not resume | Tamper, expiry, foreign reader, foreign scope, unknown version refuse before a page |
| Version fence | none | Each read sees live state and presents it as the lane | Mixed pages would look complete | Insert, reprioritize, dependency, and state changes between pages return `restart_required` |
| Byte budget, not item count | client rejects after the server builds the body | 2 MiB client ceiling; SSE frame cap is live events only | Legacy tools still emit the oversized body | Raw HTTP body length matches the envelope estimator and stays at or under 2 MiB |
| Authorized projection | graph nodes are redacted; list returns full `WorkItem` | Graph test coverage already withholds foreign ids | Public list serializes the operator record | List pages omit workspace, creator, policy, result, leases; graph nodes stay redacted |
| Detail of one maximal item | `get_work_scoped` | One get returns work plus every attempt | A maximal result history can exceed 2 MiB | `ptah_get_work_detail` pages every section; legacy get refuses instead of truncating |
| Same semantics in Desktop | `work_list` sorts by `updated_at`; remote calls `ptah_list_work` | Both expect `WorkItem[]` | Remote board breaks if only the server changes | Both commands return one page; the board distinguishes loading, partial, complete, expired, and error |
| Managed finalization binding | `apply_managed_finalization_unlocked` | Journal is written before the lifecycle envelope. Recovery mints snapshots from later disk when the envelope is missing | P2 residual from #532, reproduced by reading that branch, not relabeled P0 | Intent is durable before the journal. Recovery adopts that envelope. A journal with no authentic binding is quarantined |
| Queued managed seal | `seal_queued_managed_work` | One-shot lifecycle commit. Reload-after-success test only | No crash cut | Child-process cuts at intent, attempt, item, and cleanup |
| Receipt follows the commit | service `lease.complete` after the store commit | `fail_orphaned_idempotency_claims` turns a pending receipt into "use a new request_id" | Recovered commit looks like a no-effect failure | Replay of the original request returns the committed result and does not mutate again |
| Same-process partial write | `atomic_write_json` returns the injected error | The process keeps serving | A torn multi-record write can still admit work | Admission closes and `/ready` is not ready until a fresh process recovers |
| Preserve #530 #532 #579 #580 | existing tests | Landed on this base | Must not regress | Re-run those suites on the head |

Intentional exclusion: `begin_managed_permission_resolve` and `abort_managed_permission_resolve` write one managed-intent record and no work or attempt snapshot. They stay single-record.

Out of scope: the rest of #305, #301, and #492; authentication-spine and scheduler rewrites; live provider spend; packaged native builds.
