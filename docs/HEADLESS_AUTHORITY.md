# Headless Authority — Portable Entry Point and Canonical DTO Seam

Status: **partial — canonical seam delivered; portable `AgentHost` process blocked**
Base: `codex/external-worker-hardening-v1` @ `8ad3be07eb27087acb67704fdf463ecb95b64505`

This document records what the headless authority slice delivered, the
evidence for the part that was **not** safe to build, and the acceptance
criteria for finishing it.

Nothing here claims self-hosting, a packaged VM, or Stage 6 qualification.
Those require executed evidence that does not exist yet.

---

## 1. What shipped

`grokptah-agent-sdk` becomes the authority DTO contract instead of a parallel
restatement of it. All work is confined to that crate plus the published
schemas it is now checked against.

### 1.1 Portable host API

`grokptah_agent_sdk::headless` is the port a Linux or cloud worker embeds.

| Item | Purpose |
| --- | --- |
| `HEADLESS_CONTRACT_VERSION` | `"grokptah.headless.v1"`; negotiated separately from the capability contract. |
| `HeadlessPlatform` | Descriptive platform tag. Grants nothing; `supports_native_computer_use()` is a necessary, never sufficient, condition. |
| `CapabilityRevision(u64)` | Monotonic revision a consumer caches its negotiated `CapabilitySet` against. Saturates rather than wrapping. |
| `HeadlessHostInfo` | Share-safe advertisement: host id, both contract versions, platform, revision, `CapabilitySet`. |
| `HeadlessLimits` | Bounded ceilings at the authority's integer widths, plus max concurrent runs. |
| `HeadlessOperation` | `Submit` / `Events` / `Review` / `Cancel`, each mapped to a capability id the trusted host **already** advertises. |
| `HeadlessAuthority` | The embedder-implemented trait. Every operation defaults to `AuthorityUnavailable`. |
| `HeadlessAdmission` | The admission gate: revision → scope → capability → bounds, in that order. |

`HeadlessAdmission` is **not a second authorization model**. It admits against
the existing `CapabilitySet` and `ErrorCode` contracts, introduces no
capability identifier of its own (asserted by test), and can only narrow: a
capability that is absent, `Gated`, or `Unavailable` is refused, and a gated
capability's human grant is something this port deliberately cannot issue.

### 1.2 Canonical type map

The private authority types and the public contract disagreed on integer
width. That disagreement is now an explicit, checked seam rather than an
implicit cast.

| Authority (bridge `RunBounds`) | Public (`Bounds`) | Seam |
| --- | --- | --- |
| `max_prompt_bytes: usize` | `Option<u32>` | `Bounds::from_authority_widths` / `resolve_authority_widths` |
| `max_rounds: u32` | `Option<u16>` | same; narrowing is checked, never cast |
| `max_duration_ms: u64` | `Option<u64>` | same; width-identical |

`AuthorityBounds` carries the authority-side widths. It is deliberately **not**
`Serialize`/`Deserialize`, so it cannot become a second wire DTO; the trusted
host's own resolved-bounds type remains the authority.

`BoundsConversionError` distinguishes a *representation* failure
(`RoundsOverflow`, `PromptBytesOverflow`) from a *policy* failure
(`RoundsAboveContract`, `AboveCeiling`) and a `ZeroValue`. A `u32` round count
of `65_560` wraps to `24` — a plausible, valid-looking value — under `as u16`.
The seam rejects it.

### 1.3 Fail-closed hardening

Every published `$defs` object declares `"additionalProperties": false`, but
`run.rs` and `computer.rs` had **zero** `deny_unknown_fields` while
`capability.rs` (2) and `external_worker.rs` (7) had it throughout. The Rust
decoder was more permissive than the contract it published. All eight run
types, all four computer types, and both notification branches now fail closed.

`projection.rs` adds two redaction guards at different strictness levels:

* `ensure_share_safe_metadata` — authority-generated fields. Rejects credential
  material, provider URLs, absolute/UNC/drive paths, `..` traversal, control
  characters, empty, and oversized.
* `ensure_no_credential_material` — user/model content (prompt previews, diffs,
  summaries, event payloads). Credential material is always rejected; URLs and
  absolute paths are allowed, because a diff legitimately contains both.

Credential matching is shape-based and token-boundary anchored, so
`risk-register` does not trip the `sk-` needle. A `LeakFinding` carries only
the field name and kind — never the offending value — so the finding is itself
share-safe.

### 1.4 Schema conformance

`tests/schema_conformance.rs` `include_str!`s the three published schemas, so
the fixture fails to compile if one moves. It checks `$id` version pins,
`CONTRACT_VERSION` / `EXTERNAL_WORKER_CONTRACT_VERSION` against the schema
`const`s, wire-key set equality per definition in both directions, enum value
sets, the `maxRounds` ceiling against `MAX_ROUNDS`, closure of every object
definition, and that a version mismatch is rejected.

---

## 2. Why the portable `AgentHost` process is blocked

The task's conditional — *"if safe, add the smallest portable AgentHost/service
entry"* — does not hold. Four independent defects, each reproduced at the exact
base commit:

### B1 — `grokptah-agent-bridge` cannot build on Linux (root blocker)

```
keyring 3.6.3 → dbus-secret-service 4.1.0 → libdbus-sys 0.2.7 → system `dbus-1`
```

`keyring` is declared unconditionally for all targets, so its
`sync-secret-service` feature makes a headless Linux worker depend on a C
system library and a desktop secret-service daemon. On a clean container:

```
The system library `dbus-1` required by crate `libdbus-sys` was not found.
thread 'main' panicked at libdbus-sys-0.2.7/build.rs:25:9: explicit panic
```

Fixing this means target-gating `keyring` and reworking `auth_store.rs` —
credential storage inside the trusted host. That is the "broad overlap / risky
refactor" the slice was told to stop at, and it collides with the self-hosting
and external-worker authority lanes. `build.rs` already gates the macOS
Objective-C shim correctly on `CARGO_CFG_TARGET_OS`; the keyring is the
remaining hard blocker.

### B2 — the bridge has no binary target

No `[[bin]]`, no `src/bin/`. `AgentHost::create(HostConfig)` and
`start_control_server_with(..)` are library APIs whose only consumer is the
Tauri desktop crate. Adding a `[[bin]]` is mechanically small but pointless
while B1 stands, and it would modify the running trusted-host package that this
slice is required to stay disjoint from.

### B3 — neither lockfile records the SDK

* Root `Cargo.lock`: **no `grokptah` entries at all**, though
  `crates/common/grokptah-agent-sdk` is a declared workspace member.
* Bridge nested `Cargo.lock`: missing `grokptah-agent-sdk`, so
  `cargo … --locked` fails outright.

### B4 — the SDK had never been compiled or linted

Consequences of B3, all reproduced at base:

* `run.rs:230` — `#![deny(missing_docs)]` vs undocumented
  `RunNotification::Event` fields: the crate **does not compile**.
* `error.rs:81` — `E0382` borrow-of-moved-value in its own unit test.
* `external_worker.rs:438` — a unit test asserts the wrong error string.
* `external_worker.rs:196` — a `collapsible_if` clippy error.

B1–B4 together are the concrete form of the audit's "parallel restatement
rather than authority contract" finding: an unbuilt, unlinted, untested,
unlocked crate cannot be an authority.

---

## 3. Acceptance criteria for the portable host

1. `keyring` is target- or feature-gated so a Linux worker build pulls no
   `libdbus-sys`; `auth_store.rs` fails closed (no silent plaintext fallback)
   when no credential store is available. Owner: self-hosting authority lane.
2. Both lockfiles register `grokptah-agent-sdk`; CI runs
   `cargo test --locked` for it on Linux.
3. `external_worker.rs:438` and `:196` are fixed by the external-worker lane.
4. A `[[bin]]` or `service` entry constructs `AgentHost::create` +
   `start_control_server_with`, and implements `HeadlessAuthority` by delegating
   to `OrchestrationService` — converting through `Bounds::from_authority_widths`
   / `resolve_authority_widths` rather than casting.
5. `cargo build --locked --target x86_64-unknown-linux-gnu` for that binary
   succeeds in a container with no D-Bus, no X11, and no macOS frameworks.
6. Only then may a self-hosting or packaged-VM claim be made, and only with the
   executed transcript attached.

Until 1–5 are executed, **no self-hosting, packaged-VM, or Stage 6 claim is
supported.**

---

## 4. Security analysis

| Property | Mechanism | Test |
| --- | --- | --- |
| No capability broadening | Operations map only to already-advertised ids | `admission_never_widens_beyond_the_advertised_capabilities` |
| No second auth model | Admission narrows an existing `CapabilitySet`; cannot issue a human grant | same |
| Scope binding | Session + workspace must equal the bound pair | `admission_rejects_a_scope_outside_the_binding` |
| Stale revision | Any revision delta → `StaleOrRecovery` | `admission_rejects_a_stale_capability_revision` |
| Stale Computer Use lease | `ensure_fresh_against` requires exact revision | `a_computer_lease_is_fenced_to_the_observed_revision` |
| No silent truncation | Checked narrowing conversions | `narrowing_conversions_fail_closed_instead_of_truncating` |
| Unknown fields | `deny_unknown_fields` on every run/computer DTO | `unknown_fields_are_refused_exactly_where_the_schema_closes_the_object` |
| Oversized payloads | Named byte bounds on every projection | `malformed_and_oversized_projections_fail_closed` |
| No credential leakage | Shape-based scan of content and metadata | `credentials_never_enter_a_public_projection` |
| No path/URL leakage | Strict metadata guard | `absolute_paths_and_provider_urls_never_enter_authority_metadata` |
| Unimplemented ≠ success | Trait defaults to `AuthorityUnavailable` | `an_unimplemented_operation_reports_unavailable_rather_than_succeeding` |
| No platform dependency | Manifest asserted to be serde-only | `headless_entry_carries_no_platform_or_ui_dependency` |

**Residual risks.**

* The credential scanner is shape-based. Unambiguous markers (`-----BEGIN`,
  `bearer `, `api_key=`, …) match directly; issuer prefixes (`sk-`, `xai-`,
  `ghp_`, `akia`, …) additionally require 12 trailing opaque characters, so
  `src/sk-module.rs` is not a false positive but `AKIAIOSFODNN7EXAMPLE` is
  caught. It is not a general secret detector, and it is defence in depth
  behind the authority's own redaction, not a replacement for it.
* `ensure_no_credential_material` permits URLs and absolute paths in diffs and
  prompt previews by design. Narrowing that would reject legitimate code.
* The seam is not yet *called* by the trusted host (blocked by B1/disjointness),
  so it constrains no production path until acceptance item 4 lands.
* `RunScope.workspace` remains an absolute path in the v1 contract and is
  therefore exempt from the absolute-path guard.
