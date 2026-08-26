# Operator UX packaged acceptance — pending, not evidence

This draft is **source-isolated**. It is **not assembly-ready**. JSDOM and CSS
source checks are **source evidence only**. They do not prove a packaged desktop,
VoiceOver, zoom, or forced-colors session. Later `App.tsx` / `app.css` conflicts
require a re-audit and every gate rerun.

Do **not** treat this file as packaged qualification. Do **not** claim 100%.

## Identity

| Item | Value |
| --- | --- |
| Selected source | `codex/external-worker-hardening-v1` |
| Required parent / fail-closed HEAD | `8ad3be07eb27087acb67704fdf463ecb95b64505` |
| Isolated branch | `cursor/operator-consent-recovery-ux-772c` |
| Prior draft head (review baseline) | `f39d4628c8a46569ec20cc33c5b646fd500f5460` |
| Donor inspected, never merged | `b456178e2836916e9e646cc7cb262e1be794a01f` |
| Donor parent | `8ad3be07eb27087acb67704fdf463ecb95b64505` (sibling, not ancestor of this work) |
| Campaign | Disjoint from the authority-spine campaign |

## Independent review corrections (this successor)

Display-only. No backend authority change. No hosted or packaged acceptance claim.

1. **Closed display projection.** The renderer no longer blacklists fragments
   from `request.summary`, `detail.risk`, deny-history summaries, paths,
   commands, URLs, credential-like prose, ids, or Unicode formatting controls.
   Visible copy is fixed `CONSENT_COPY` plus own-key mapped tool and risk
   classes. Tool/risk tables are null-prototype maps; lookup uses
   `Object.prototype.hasOwnProperty.call` so `__proto__`, `constructor`,
   `toString`, and other inherited keys always return a string and cannot
   crash consent.
2. **Deny-history identity.** Persisted records keep the true owning
   `session_id`. Presentation never renders that id and never substitutes the
   literal `owning-session`. Two sessions keep distinct provenance.
3. **Inert containment.** `observeNonConsentInert` applies observer-backed
   ancestor-sibling inert from the consent layer through `document.body`,
   including late siblings and body portals. Pre-existing `inert` /
   `aria-hidden` values are snapshotted and restored exactly.
4. **Single acknowledgement owner.** `acknowledgeOperatorPermission` in App
   awaits `api.permissionRespond` once and returns one closed
   `acknowledged` / `rejected` / `lost` result. A late resolve after timeout
   cannot flip `lost`. PermissionModal does not start a timer and does not
   treat void or arbitrary resolution as acknowledgement. `submitGate` stays
   synchronous and at-most-once. Queue removal is ack-only.
5. **Queue-head lock.** Request identity, phase, and the at-most-once gate bind
   synchronously through `reduceConsentLock`. A mismatched request is not
   projected as an unlocked idle head. No passive effect clears the gate or
   unlocks pending/unconfirmed for the current request. Stale acknowledgement
   for a previous id is ignored. App remounts the dialog with `key={permission.id}`.
6. **Exact-path inert.** Only the mounted consent layer and its ancestor path
   are excluded. Late mislabeled `data-modal-layer="consent"` portals and
   SVG/foreign-element siblings are inerted. Prior `inert` / `aria-hidden`
   values are restored exactly.
7. **Owning session fail-closed.** Empty or missing `request.session_id` does
   not persist the focused fallback and does not render as a known owner.
   A claimed session that does not match the host owner is not persisted.

## Allowlist (exact)

- `desktop/src/components/PermissionModal.tsx`
- `desktop/src/components/PermissionModal.test.tsx`
- `desktop/src/lib/operatorConsentPresentation.ts` (new)
- `desktop/src/lib/operatorConsentPresentation.test.ts` (new)
- `desktop/src/App.tsx`
- `desktop/src/App.operatorUx.test.tsx` (new)
- `desktop/src/styles/app.css`
- `docs/OPERATOR_UX_PACKAGED_ACCEPTANCE_EVIDENCE.md` (new)

Zero other changed or untracked paths. No Rust, provider, orchestration, store,
ActionGrant, ComputerRun, external-worker, Semantic Help, VM/helper, schema,
manifest, lockfile, or Actions edits.

## Reproduction before edit (HEAD `8ad3be07`)

Observed in the selected tree, before this draft:

- Permission dialog declared `role="dialog"` but did not focus Deny, trap Tab,
  restore the opener, or inert non-consent siblings.
- App capture-phase shortcuts (`⌘1–⌘6`, `⌘\`, `⌘B`, `⌘⌥B`, `⌘⇧L`, `⌘⌥←/→`)
  stayed live under a pending prompt.
- Escape was unbound; clicks could invoke `onRespond` more than once.
- `api.permissionRespond` rejection left no `response unconfirmed` lock; the
  modal could be activated again. Deny history was written *before* the host
  call, which could claim an outcome the host never acknowledged.
- Queue harness advanced immediately from the renderer click.
- Visible UI rendered `request.summary`, `request.tool_name`, truncated session
  ids, `data-request-id` / `data-session-id`, and `JSON.stringify(request.detail)`.
- Always Allow was always enabled. This head has no host-authored bounded scope,
  lifetime, or revision; those facts were not invented here.
- Consent CSS lacked dedicated visible-focus, forced-colors, reduced-motion,
  200%–400% text, narrow-window, and 44px touch rules.

## Pending packaged acceptance script

Run only on a signed desktop build with VoiceOver, keyboard, zoom, and
forced-colors. Record pass/fail. A blank cell is **not** a pass.

1. **Keyboard — initial focus.** Open a real permission prompt. Confirm focus
   lands on **Deny**, not Allow or the composer.
2. **Keyboard — Tab cycle.** Tab and Shift+Tab stay inside the dialog, including
   when focus has escaped to the document. The composer never receives Tab.
3. **Keyboard — opener restore.** Answer or unmount the dialog. Focus returns to
   the control that was focused before the prompt.
4. **Keyboard — Escape.** Escape sends Deny only before any submission.
   After submit (pending) or after `response unconfirmed`, Escape does not send
   another decision.
5. **Shortcuts.** With the prompt open, `⌘1–⌘6`, `⌘\`, `⌘B`, `⌘⌥B`, `⌘⇧L`, and
   `⌘⌥←/→` do not change docks or chrome.
6. **Inert background.** Titlebar, sidebar, composer, and status bar are
   non-interactive (`inert` / `aria-hidden`) while consent exists.
7. **Acknowledgement.** Double-activate Allow. The renderer sends once. If the
   host drops or rejects the ack, the screen locks **response unconfirmed**,
   does not retry, does not become Deny, and does not show the next queued
   request.
8. **VoiceOver.** The alert announces that a tool is blocked using closed
   labels only. No request id, session id, path, command, token, or raw JSON
   is spoken. Recovery copy must not say the action succeeded or that retry is
   safe.
9. **Zoom 200% and 400%.** All consent copy wraps. Deny and Allow remain
   visible and usable. No horizontal clip of the decision row.
10. **Narrow window (~320–480 CSS px).** Actions stack. Touch targets stay at
    least 44px. Queue and recovery copy remain readable.
11. **Forced colors / Windows high contrast.** Dialog, risk, recovery, and
    focus rings remain distinguishable.
12. **Reduced motion.** Consent layer does not animate or transition.
13. **Always Allow.** The control is absent. Copy reports scope, lifetime, and
    revision **unavailable**. Do not enable it from untrusted detail fields.

Result log (pending — not filled by JSDOM):

| Step | Result | Notes |
| --- | --- | --- |
| 1 initial Deny focus | pending | packaged AT not run |
| 2 Tab cycle | pending | packaged AT not run |
| 3 opener restore | pending | packaged AT not run |
| 4 Escape phases | pending | packaged AT not run |
| 5 shortcut suppression | pending | packaged AT not run |
| 6 inert background | pending | packaged AT not run |
| 7 lost/rejected ack | pending | packaged AT not run |
| 8 VoiceOver | pending | packaged AT not run |
| 9 zoom 200–400% | pending | packaged AT not run |
| 10 narrow / touch | pending | packaged AT not run |
| 11 forced colors | pending | packaged AT not run |
| 12 reduced motion | pending | packaged AT not run |
| 13 Always Allow unavailable | pending | packaged AT not run |

## Source gates (not packaged AT)

Lint and Playwright are **not** claimed; neither exists in this desktop package.

| Command | Result |
| --- | --- |
| `npm ci` | pass |
| `npx tsc --noEmit` | pass |
| focused Vitest (`operatorConsentPresentation`, `PermissionModal`, `App.operatorUx`) | 37/37 pass |
| `npx vitest run` | 50 files / 316 tests pass |
| `npx vite build` | pass (client production; chunk-size warning only) |
| `npm run verify:public` | pass (regression only; public bundle + consumer fixture) |
| `npm audit --omit=dev` | pass (0 production vulnerabilities) |
| `git diff --check` | clean |
| privacy scan | no live credentials or private identity paths; tests use absence needles only (`/etc/passwd`, `/opt/x`, `src/main.rs`, `git status`, `tok_opaque_9f3a`, bidi/Cf controls, prototype keys, `/Users/secret`) |
| GitHub `Desktop / desktop` on `f39d4628` | **fail (parent lock residual)** — `Typecheck + unit tests` passed; `Cargo tests (desktop)` failed `cargo test --locked` because `desktop/src-tauri/Cargo.lock` does not record path dep `grokptah-agent-sdk` already present in `grokptah-agent-bridge` at `8ad3be07`. This draft does not alter lockfiles. The unchanged parent lock mismatch remains an inherited hosted blocker. |

JSDOM printed `HTMLCanvasElement.prototype.getContext` when `App.operatorUx.test.tsx` imported `App.tsx` (xterm). Tests still passed. That is not packaged AT.

GitHub Desktop CI cargo failure is inherited from the selected parent. Reproducing `cargo metadata --locked` in `desktop/src-tauri` on the clean parent tree yields the same error. Fixing it requires a lockfile edit, which this allowlist forbids.

## Residuals

- Packaged keyboard / VoiceOver / zoom / forced-colors remain **pending**.
- This slice does not change backend permission authority.
- Always Allow stays unavailable until a later host-authored scope, lifetime,
  and revision exist. Inventing those fields here would be a backend change.
- Public-package tokens, Help trapping, Computer Use cockpit, and
  `workspaceShortcuts.ts` / `overlayA11y.ts` from donor `b456178e` were **not**
  landed.
- Overlapping drafts on other bases (including Help trap PR #392 and operator
  chrome a11y PR #377, plus donor branch `claude/grokptah-audit-repairs-83sffv`)
  must not be merged into this allowlist.
