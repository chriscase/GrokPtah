# Packaged Computer Use authority

This document describes how GrokPtah decides whether packaged macOS Computer Use
may run, and — just as importantly — what that decision does **not** establish.

## The verdict, stated plainly

As of this branch, on every machine that has run this code:

| Question | Answer |
| --- | --- |
| Is the source-level authority implemented and tested? | Yes |
| Is packaged macOS Computer Use qualified? | **No** |
| Has a signed, notarized helper been inspected? | **No** |
| Have TCC grants been observed? | **No** |
| Has Virtualization.framework been launched? | **No** |
| Has a guest booted, produced frames, or accepted input? | **No** |
| Has any hardware or soak run happened? | **No** |

The production verdict is `unavailable` (the inputs to decide were absent) or
`fail_closed` (they were present and admission denied). It cannot be `pass`.
`partial` — synthetic contract holds, artifacts admitted, hardware unobserved —
requires a real signed helper and an operator trust root that no one has
supplied yet.

## Two rules that make admission mean something

### 1. Identity comes from the operating system

Signing class, Team ID, and the designated requirement are properties macOS
computes by verifying a signature against a code directory. A file inside a
bundle stating those properties is not evidence of them: whoever can place the
bundle can place the file.

So `CodeIdentityProbe` is the only source of those facts. Its production
implementation runs pinned `/usr/bin/codesign` and `/usr/sbin/spctl` and retains
their output verbatim in the evidence record. On any host without those binaries
the probe reports itself unavailable and every inspection fails closed. There is
no fallback that reads an attestation out of the artifact.

Filenames that have historically been mistaken for signing evidence —
`codesign-display.txt`, `helper.signed`, `guest.img.signed`, and friends — are
never read. When one is found it is recorded in
`ignoredSelfAttestations` so a reviewer can see the artifact tried to vouch for
itself.

Parsing is deliberately narrow. `codesign -d --verbose=2` emits `Key=Value`
lines, and classification reads only values from a recognized key **anchored at
the start of a line**. Prose containing "Developer ID Application" does not
promote a bundle. A value carrying a negation token (`not`, `no`, `never`,
`invalid`, `failed`, `rejected`) is refused outright, so
`Authority=not Developer ID Application` classifies as unsigned rather than as
Developer ID. Notarization is read from Gatekeeper's `source=` line, not from
the word "notarized" appearing anywhere in codesign output.

### 2. Expectations come from an operator, not the artifact

Admission compares an observation against an expectation. If the expectation is
derived from the thing being checked, the comparison always succeeds and proves
nothing.

`PackagedTrustRoot` is a JSON file named by `GROKPTAH_COMPUTER_USE_TRUST_ROOT`.
It declares the expected designated requirement (compared for **exact** equality,
not `contains`), Team ID, app and helper bundle identifiers, helper entitlements
digest, guest-image digest, guest-image authorization digest, format, and
provenance. Loading it refuses a symlink, a non-file, an oversized file, a
partial record, an unknown field, and — critically — **a trust root that resolves
inside the artifact root it would authorize**.

There is no default and no inference. Absent trust root means deny.

## One authority

Exactly one component owns leases, revisions, dispatch de-duplication, and
cleanup receipts: `IsolatedVisualHost` in `grokptah-isolated-visual`. Helper
launch, dispatch, cancel, crash, expiry, and restart are all fenced there,
against durable records.

There is deliberately no second helper-local state machine with its own lease
id, its own `used` dispatch map, and its own receipt. Two authorities can
disagree, and when they do neither can be trusted about whether physical input
reached the guest. `scripts/check-adversarial-reachable.sh` fails the build if
one reappears.

## Durability is part of the dispatch contract

`Prepared` and `Injected` are written durably *before* the corresponding
real-world step:

- If the `Injected` write is not durable, **nothing is injected** and the
  dispatch is refused as known-not-injected.
- If a write fails *after* injection, the outcome is **Uncertain**, surfaced as
  such, and never replayed.
- Across restarts, an `Injected` dispatch becomes `Uncertain` and stays there.
  Two consecutive restarts do not replay it; the guest incarnation is not
  resumable, so nothing can be replayed onto it either.
- A dispatch id reused with a *different* payload is a conflict. Reused with the
  *identical* payload it is idempotent and injects exactly once.

## Cleanup receipts are re-observed, not asserted

A receipt that reports the same booleans the teardown code just set records an
intention, not an outcome. A failed `remove_file` whose error was discarded would
still read as "overlay removed".

`CleanupReceipt::observe` therefore runs *after* teardown and re-derives each
fact from its own source: the filesystem for overlay and marker files, the guest
handle for the VM and resident frames, the occupancy store on disk for the
occupancy lease. Each resource is digested individually and the whole set is
bound by a receipt digest that must recompute. Every required resource must
appear exactly once, so a probe cannot pass by omitting one.

Three states are distinguished, and only one is success:

- `Released` — independently confirmed gone.
- `Present` — independently confirmed still there.
- `Unknown` — could not be determined. Never counts as released.

Anything not `Released`, plus any teardown error, lands in
`CleanupReceipt::unresolved`, the outcome is `Unresolved`, and the guest is
**not** marked clean. `require_exact()` turns that into `UncertainOutcome` rather
than letting it pass quietly.

## Records that are shaped right but wrong

Deserialization succeeding says only that bytes had the right shape. A record
that deserializes but fails its own `validate()` — a lease with revision zero, a
state/dispatch combination the machine cannot produce — is not a usable record.
On open, such records are moved to `quarantine/` alongside unreadable and
oversized ones, and the move is itself fsynced so the evidence survives.
`IsolatedVisualStore::recovery()` reports what was quarantined, what expired
grants were reaped, and which dispatches were carried to `Uncertain`.

Occupancy reads fail closed: a corrupt, oversized, symlinked, or unreadable
occupancy record denies rather than reading as `Clear`, and an unreadable record
is never released blindly.

## Digest agreement

The Rust authority and the Node inspector must compute identical digests, or an
artifact could be admitted by one and rejected by the other. Both use a plain
SHA-256 over file bytes, and the same sorted `path\0digest\0` bundle manifest.
`rust_and_js_digests_agree` in the adversarial matrix pins this against Node's
`crypto.createHash("sha256")` and additionally asserts the Rust digest is a
single SHA-256, not a double hash.

## Ancestry is three-valued

The inspector reports whether the tree descends from the reviewed base. That
question has three answers, not two.

`git merge-base --is-ancestor` exits non-zero both when a commit genuinely is
not an ancestor and when it is simply missing from a truncated history. CI
checks out a shallow merge ref, so the second case is the normal one there.
Collapsing them into a boolean turns "we could not look" into "it does not
descend" — a definite negative the tool has not earned, and the same
Unknown-as-negative confusion the cleanup receipts avoid.

So `baseAncestry.result` is `proven`, `refuted`, or `indeterminate`, each with
a reason. A non-zero exit is read as `refuted` only when the history was
complete enough to have answered: the clone is not shallow *and* the base
commit is present locally. Otherwise it is `indeterminate`.

Consumers do not treat `indeterminate` as proven. `refuted` is `fail_closed`
— this is not the reviewed base. `indeterminate` is `unavailable`, alongside
the other cases where the inputs to decide were absent. Neither can reach
`partial`.

`scripts/qualify-ancestry.test.mjs` covers the decision table through an
injected runner and then builds a real shallow clone of a real merge commit and
drives it through real git, so the CI shape is exercised rather than described.

## The projection carries no filesystem paths

Evidence records get shared. A path in one discloses the layout of the machine
that produced it and nothing a reader can verify, so the operator projection
identifies things by digest and by fact:

- the trust root appears as `trustRoot.sha256` and its issuer, never its path;
- inspected bundles appear as digests; the `commands` list records *what* was
  run with `<packaged-app>` placeholders rather than the supplied paths;
- the Rust record reports `artifactRootConfigured` as a boolean, not a location;
- `IsolatedPreflight` carries no path-typed field at all. `bundle_path` and
  `image_path` live on the observation types that feed admission, which are
  inputs and are never projected.

`declaredBaseSha` in the Rust record is named for what it is: the base this work
targets, declared by the binary. That binary does not read git history, so it
makes no ancestry claim of its own.

## What would move the verdict

To reach `partial`, a machine needs all of:

1. A real `GrokPtah.app` containing an assembled
   `Contents/Helpers/GrokPtah Computer Use Helper.app`.
2. Both bundles signed with a Developer ID Application certificate, notarized,
   and stapled, so `spctl --assess` reports
   `source=Notarized Developer ID`.
3. An operator trust root outside the bundle declaring the exact designated
   requirement, Team ID, and entitlements digest that the OS reports.
4. A guest image whose digest, format, provenance, and authorization digest
   match that trust root.
5. Apple silicon with `kern.hv_support=1`, Virtualization.framework present, at
   least 25 GiB free, and a clear occupancy store.

To go beyond `partial`, someone must additionally observe TCC Screen Recording
and Accessibility grants against the *helper's* code identity, a real
Virtualization.framework launch, a guest boot, real frames, real input, verified
cleanup on hardware, and a soak run. None of that has happened, and no code in
this repository can assert it happened.

## Reachability from the service

`ComputerUseService` exposes the production path directly:

- `packaged_authority_admission()` — the preflight for this host, right now.
- `packaged_helper_available()` — false wherever a signed helper and trust root
  are not both present, which is every CI runner and every developer checkout.
- `executor_identity()` — the identity Computer Use would actually run under.
  Without an admitted helper this is the in-process host identity, which is
  never packaged-helper qualification, and which cannot carry a Team ID or
  designated requirement.
- `packaged_authority_evidence(head, branch)` — the full verdict record.

These are safe to call anywhere. On an unsupported host they deny with stated
reasons; they never enable an unsupported environment and never return a value
that reads as eligible by default.
