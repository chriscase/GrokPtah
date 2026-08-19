# ADR-002: Runtime, service, storage, and authority boundaries

**Status:** Accepted
**Date:** 2026-08-18
**Issues:** #300, #301

## Context

GrokPtah has two shipped hosts over the same agent runtime:

- the Tauri desktop, which can host execution in-process and supplies the
  interactive operator experience and OS-bound capabilities; and
- `grokptah-service`, which hosts the bridge and authenticated control plane
  without Tauri for local, VM, or private-cloud deployment.

The product is local-first, not local-only. A user may keep the complete agent
home on one computer, or place one persistent home on a private service so
long-running agents remain available while personal devices are offline. In
either mode, desktop, MCP, and future web or mobile clients communicate with
the one process that owns the home. They do not synchronize copies of its
files or become independent writers.

This ADR records the boundaries required before durable workloads, routines,
manager-agent delegation, or additional clients are added. It replaces the
superseded assumption that the desktop must always be the sole runtime anchor.

## Decision

### 1. Local and hosted homes are first-class modes

GrokPtah supports two deployment shapes over the same runtime and protocol:

| Mode | Authoritative home | Typical clients |
| --- | --- | --- |
| Local-first | Desktop-hosted runtime or a service on the user's machine | Desktop and local MCP clients |
| Hosted persistent home | One service on a private VM or managed single-user/single-team deployment | Authenticated desktop, MCP, and future web/mobile clients |

Hosted mode is the expected fit for always-available Grokbot-style agents;
local mode remains a complete privacy- and control-preserving option. This
decision does not commit the project to operating a public multi-tenant SaaS.

Each deployment has one authoritative `GROKPTAH_HOME`. A client may reconnect
from another device and continue from durable IDs and event cursors, but it
does not mount or copy the home directory.

### 2. Runtime, host, transport, and client responsibilities are separate

The following table is the normative ownership target, including the durable
workload slice shipped in issue #305:

| Layer | Owns | Must not own |
| --- | --- | --- |
| `grokptah-agent-bridge` runtime | Sessions, finite runs, agents, memory, permissions, tools, durable workload state, durable events, policy, recovery, and promotion rules | Authoritative Tauri selection state, HTTP request state, focused UI state used as domain input, or provider-specific UI behavior |
| Tauri desktop host | In-process runtime lifecycle, keychain integration, PTY, dialogs, local permission decisions, locally granted Computer Use, and typed IPC adapters | A second domain state machine in commands/React, implicit remote authority, or policy that differs from the service |
| `grokptah-service` host | Process lifecycle, configured roots, listener/auth configuration, readiness, and declared host capabilities | Desktop permissions, keychain/PTY assumptions, ambient Computer Use, or transport-specific work transitions |
| MCP/API transport | Versioned schemas, authentication, scoped request mapping, idempotency identity, bounded responses, and cursor recovery | Business transitions implemented independently of the runtime or authority inferred from bearer authentication alone |
| Desktop/web/mobile/coordinator clients | Presentation, explicit operator intent, reconnect, and protocol consumption | Direct durable-file mutation, copied-home conflict resolution, or authority broader than the server grants |

Tauri commands and HTTP handlers are adapters. Persistent agents, workloads,
assignments, attempts, routines, and messages belong in transport-neutral
runtime/domain services.

Issue #305's first workload slice uses the existing single-owner, file-backed
ledger and authenticated MCP boundary in both desktop and service hosts. It
supports durable WorkItems and Attempt/Lease records, scoped reads, idempotent
mutations, dependency/deadline/retry/approval-aware admission, Lane archival
independence, and a shared startup/periodic reconciliation supervisor. The
supervisor is recovery-only; finite Runs remain the execution boundary and
model execution is never resumed implicitly. The service boundary now accepts
named device credentials that map to one configured Agent-owner account, so
client attribution is distinct from durable Agent ownership without implying
multi-tenant authority. Per-account grants, multi-node claims, and a
database/coordinator remain follow-on milestones.

The current bridge also persists compatibility chrome (`active_session`, open
tabs, appearance, and related desktop restore fields), and some credential
operations still enter through the bridge host. These are implementation
exceptions, not domain authority. Persisted chrome is a non-authoritative
projection: workload, lease, assignment, permission, and recovery code must
never consult it to infer ownership or permission. Keychain ownership moves to
the Tauri adapter when that boundary is physically extracted.

### 3. Host capabilities are declared and fail closed

The desktop and service are not assumed to have identical capabilities.

- Desktop may offer OS keychain access, PTY, native dialogs, interactive
  permission decisions, and locally granted Computer Use.
- The service MCP boundary selects only exact canonical workspace identities
  present in its configured allowlist. That check is authorization, not an OS
  filesystem or child-process sandbox.
- A remote client cannot turn authentication into desktop permission,
  credential visibility, Computer Use authority, approval, or promotion.
- An unavailable host capability returns a typed unsupported/forbidden result;
  it never silently falls back to broader process or filesystem access.

The same domain record may therefore project different *available actions* on
different hosts while preserving identical durable identity and state.

Today the service exposes a static tool surface over `HostConfig::default()`;
it does not yet advertise a declared capability document. Its shipped surface
includes allowlisted Build-session creation/discovery, bounded run and queue
control, durable history/events/checkpoints, explicit persistent-agent resume,
review/approval/promotion for isolated runs, and scoped Computer Run reads when
compiled on a capable host. Explicit capability advertisement is a required
future contract before workload assignment may select a worker by capability.
That contract must define stable capability identifiers, the host/version that
asserted them, attempt-time capture, and typed unsupported/forbidden failures.

Hosted mode additionally requires host-level confinement: a dedicated service
account plus systemd, container, VM, or equivalent policy limiting writable
paths and process authority. Tool safety profiles and workspace allowlists are
application gates, not substitutes for that confinement. Non-loopback traffic
must use TLS, a trusted encrypted tunnel, or a trusted TLS terminator. A
firewall alone does not protect bearer credentials in transit.

### 4. Storage has one writer until a designed boundary replaces it

One process owns a `GROKPTAH_HOME` at a time, enforced by the existing instance
lock and store locks. Desktop and service must not concurrently write the same
home. Multiple clients share the owning process's protocol and durable state;
they are not filesystem peers. Named bearer credentials identify the client
device for audit/run attribution, while `GROKPTAH_SERVICE_AGENT_OWNER` identifies
the account allowed to own durable Agents on that service.

The runtime now exposes a validated `RuntimeHome` context. The default desktop
and CLI path still discovers `GROKPTAH_HOME`, while hosted/library bootstraps
may inject an explicit root. That context owns the shared layout and remains
held for the host lifetime, so orchestration, Computer Use, sessions, memory,
MCP trust, provider state, and instance locking resolve against one selected
home. This is a filesystem-backed portability seam, not yet a database or
multi-node abstraction.

Copying a home while its owner is live, placing it on a multi-writer network
filesystem, or syncing it between devices is unsupported. A future
local-to-hosted migration must define:

1. quiescing and locking the source home;
2. integrity/version verification and credential handling;
3. one-way transfer with an attributable cutover point;
4. rollback before, but never concurrent writing after, cutover; and
5. client re-binding to the new authoritative endpoint.

A database or coordinator becomes justified when concurrent writers,
multi-node worker claims, retention/query scale, or atomic transitions can no
longer be satisfied by the single-owner stores. Wanting another UI or remote
client is not by itself a storage-extraction trigger.

### 5. Authority tiers are explicit and may only narrow

| Tier | May do | May never do implicitly |
| --- | --- | --- |
| Local privileged operator | Configure the local host, make permission decisions, grant supported local capabilities, review, and promote | Widen an already captured agent/worker policy without a new attributable decision |
| Authenticated service coordinator | Use the documented scoped reads and mutations allowed by its credential and workspace binding | Read credentials, inherit desktop permissions, grant Computer Use, approve tools, or promote code |
| Bounded worker/client | Claim or execute explicitly assigned work within captured policy, bounds, lease, and resource scope | Create authority, cross agent/project scope, or treat a model prompt as approval |
| Read-only observer | Read bounded redacted projections and replay permitted events | Mutate queues, runs, work, permissions, credentials, or files |

The target model binds every credential to a transport-neutral
`AuthorityContext` containing an immutable principal ID, credential ID, tier,
workspace and Agent scopes, permitted operations, and delegation source. Lease
claims, renewals, completions, approvals, and promotions record that context.
A caller-supplied worker, Agent, session, or Lane ID is a requested resource;
it never substitutes for authenticated principal identity. Delegation may only
narrow the delegator's captured authority and must remain attributable.

This tier separation is not fully shipped. The current service accepts a
primary bearer plus optional named device credentials, but every credential
still receives the configured allowlist and complete MCP tool surface. In
particular, `ptah_approve_run` and `ptah_promote_run` remain operator-equivalent
for every configured credential. Deployments must treat possession of any
configured bearer as privileged operator access. The initial durable workload
slice uses the same operator-equivalent identity for claims and mutations;
separate attributable operator approval and scoped principal credentials are
required before exposing it to less-trusted workers.

Every new operation must name its caller tier, durable resource scope,
mutation authority, idempotency behavior, redaction contract, and recovery
behavior. Operator authority captured for an agent or assignment may be
narrowed by policy; ambient host state cannot widen it.

### 6. Provider integration remains profile-shaped until evidence requires more

`ProviderProfile` remains the boundary for OpenAI-compatible routes. Built-in
xAI is represented through that same profile-shaped path even though its
managed profile and credentials are not user-editable.

A provider adapter trait is introduced only when a real second API shape
cannot fit the profile contract without provider-specific lifecycle,
transport, authentication, streaming, or tool semantics leaking into the
runtime. The proposal must name the provider and incompatible operation and
include conformance tests demonstrating why a profile cannot represent it.
Abstraction preference alone is not a trigger.

### 7. Protocol and conformance precede extraction

Reusable behavior stays in internal modules/crates until a second real
consumer demonstrates a stable boundary. The ordering is:

1. place policy and state transitions in the bridge runtime;
2. define a versioned MCP/API contract with bounded schemas;
3. prove desktop/service parity with a shared, versioned fixture matrix and
   stated pass criteria against both hosts;
4. extract a separate crate only after two named running consumers execute that
   matrix against the same boundary; and
5. publish an SDK or split a repository only when a named compatibility and
   version owner maintains that matrix for a real external consumer.

The desktop and service are already two hosts, but that does not make every
internal module a public SDK. ContextDesk or another project counts as a
consumer only after it exercises the protocol or dependency in running code.
The current desktop control-plane soak and service conformance suite are
different surfaces; they are evidence toward, not completion of, the shared
parity matrix required by this trigger.

### 8. Durable relationships use IDs, never ambient state

Agents, sessions, runs, workloads, attempts, assignments, messages, artifacts,
retries, and dependencies reference validated durable IDs. Source resource
identity is distinct from a disposable execution worktree.

| Term | Canonical meaning and relationship |
| --- | --- |
| Agent | Durable identity, specification, policy, memory scope, and lifecycle; owns zero or more Sessions over time |
| Session | Current durable execution-context record and foreign-key target for runs/workloads |
| Lane | UI/product projection of one Session; today `lane_id == session_id` one-to-one, not a separate durable record |
| Assignment | Durable declaration that one work item is eligible for an Agent/worker under captured policy |
| Attempt / lease | One bounded, attributable claim to execute an assignment; an assignment may have many sequential attempts, but at most one active lease |
| Run | One finite model/tool execution linked to one Session and, in the workload slice, one attempt; an attempt may contain multiple finite runs only when its policy says so |

Until a separate Lane record is explicitly designed, workload foreign keys use
`session_id`; they do not persist a second `lane_id` alias.

No durable relationship may be inferred from the focused desktop session,
current process directory, an HTTP connection, a process-local handle, or
whether a Lane is visible. Archiving a Lane is a presentation lifecycle event;
it does not retire an Agent or mutate workload, memory, run, or artifact state.

### 9. Activation creates or wakes durable work; it does not execute policy

Schedules, timers, webhooks, GitHub/event adapters, and message adapters may
validate an event and create or wake eligible durable work through the shared
workload service. They do not own the workload state machine and cannot:

- resume an interrupted model invocation in place;
- bypass admission, dependency, lease, retry, deadline, or run bounds;
- approve a tool, permission, or Computer Use request;
- promote code or artifacts; or
- widen the captured agent or assignment policy.

Every activation is attributable and idempotent. Execution remains a finite,
bounded run linked to a durable work item/attempt.

Issue #306 ships the first routine/activation slice for manual, one-shot, and
interval/calendar triggers. The runtime-home owner fires due routines through
the shared workload API. Desktop UI timers are not authoritative. Webhook,
GitHub, and message adapters share the reserved `External` trigger boundary
and are not enabled in that slice.

## Consequences

- Persistent-agent and workload features can run in either host without
  becoming Tauri or HTTP concepts.
- A private cloud service can remain available to multiple devices while
  preserving one authoritative state owner.
- Local operation retains the same durable model without requiring a cloud
  account.
- Desktop-only capabilities remain possible, but their absence on a service
  is explicit rather than emulated unsafely.
- Multi-writer, multi-node, offline replication, and public multi-tenant
  operation require new decisions rather than accidental extension of the
  single-home design.
- No automatic permission approval, automatic promotion, or silent Computer
  Use authority follows from unattended activation.

## Non-goals

- Public multi-tenant SaaS architecture.
- Offline home replication or conflict resolution.
- Multi-region or active-active execution.
- A provider adapter trait without an incompatible real provider.
- A published SDK or separate repository without a second maintained consumer.
- Replacing finite runs with one never-ending model invocation.

## Relationship to ADR-001

ADR-001 remains the decision for the hybrid thin agent loop and its path toward
upstream embedding. ADR-002 defines where that runtime may be hosted, who owns
durable state and authority, and what evidence must exist before those
boundaries are extracted or distributed.
