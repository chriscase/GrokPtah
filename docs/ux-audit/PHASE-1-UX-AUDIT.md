# GrokPtah issue #308 — Phase 1 current-state UX audit

Date: 2026-08-17  
Build exercised: local GrokPtah desktop build at `desktop/src-tauri/target/release/bundle/macos/GrokPtah.app`  
Scope: product-research walkthrough only. No production code was changed and no new agent turn was sent.

## Executive summary

GrokPtah already exposes a wide range of agent-product capabilities: coding builds, chats, multiple open session zones, a Live rail, Computer Run, persistent agents, task runs, Git/worktrees, MCP, settings, authentication/provider profiles, search, Browse, archive controls, terminals, and recovery actions.

The current experience is correspondingly powerful but overloaded. The most important UX risk is not a missing control; it is that the product’s visible information architecture is still session-first while the intended product model is identity-first plus work-context-first:

- The Sessions sidebar is the dominant navigation surface and reports 102–103 sessions.
- The Live rail exposes 8 open sessions at once.
- Persistent agents are nested under Tools and, during this pass, reported that none existed because the orchestration store was locked.
- A lane can be invalid, archived, open, focused, live, or docked, but these distinctions are not consistently named or visually separated.
- The app can show an unavailable lane and a valid lane in two zones while the shared Tools panel reports status for the wrong context.

The most urgent quality issue is the way infrastructure failures are surfaced. Computer Use and orchestration both showed raw lock errors containing internal storage paths and `os error 35`. These errors left the product in contradictory states such as “No persistent agents yet” beside “Could not refresh persistent agents,” and “Computer Run Off” beside an enabled top-level Computer control and an open cockpit.

The strongest design direction for the next phase is to make Durable Agents and disposable Lanes first-class concepts, then reduce the default workspace to one clear active lane. Advanced surfaces—Live, Tools, Computer Run, terminals, worktrees, MCP, and task runs—should remain available but become contextual expansions rather than simultaneous peer panels.

## Method and evidence boundaries

I used Computer Use against the real locally built application and refreshed the accessibility tree after each interaction. I inspected visible UI, accessibility labels, transient menus, state text, and screenshots. The walkthrough included:

- local session selection, missing-workspace recovery, a valid demo workspace, a second open lane, Live sessions, terminals, Git/worktrees, task runs, MCP, settings, Computer Run, persistent agents, Search, Browse, archive filters, layout toggles, and session actions;
- completed transcript evidence already present in the app for inspect → test → edit → test → diff review;
- read-only state exploration only. I did not send a new coding prompt, enable MCP, archive a real session, delete anything, change permissions, or modify production code.

Not observed in this pass: a real hosted/cloud session, a reconnect after network loss, a second-device handoff, a successful Computer Use capture, a live approval modal for a new mutating action, or a full narrow-window resize test. Those are explicit follow-up gaps, not assumptions about behavior.

## Product model used for the audit

The current product/backlog direction distinguishes:

| Concept | Intended meaning | Evidence in current UI |
|---|---|---|
| Durable Agent | A long-lived identity with role, policy, memory, routines, and checkpoints | A `Persistent agents` panel exists under Tools; it says durable Build identities and verified checkpoints |
| Lane | A high-turnover work context with objective, workspace, branch/worktree, current run, progress, changes, tests, blockers, and result | Current UI primarily calls these sessions/builds; session actions include Resume, Fork, Rewind, Archive, and Delete |
| Session | The current visible/transcript-oriented representation of work | Sessions are the main navigation surface; Browse lists inbox items by title, kind, message count, date, and path |
| Live session | An open/focused/docked view of a session | Live rail reports 8 open sessions with focus/dock instructions |
| Archive | A way to remove a lane from active navigation while retaining recoverable history | Browse has Active, Archive, and All tabs; empty archive showed `Unarchive selected` disabled |

The audit treats “Agent” and “Lane” as separate design objects even where the current UI uses “session” for both the durable record and the active work context.

## Observed current state

### 1. The default workspace is a dense multi-panel cockpit

Observed facts:

- The top bar contains independent toggles for Sessions, Tools, Live, and Computer, plus Open folder, account/auth, and Settings.
- The normal layout can show a Sessions sidebar, a central work area with multiple zones, and a Tools panel simultaneously.
- The session area reported 102–103 items; the Live rail reported 8 open sessions.
- The tab/docking instructions use “click to focus,” “Alt-click / double-click to dock,” “Undock zone,” “ZONE 1,” “ZONE 2,” and a dock capacity of `3/3` in Computer Run.
- Session titles are frequently long task prompts truncated with ellipses. Several visible entries are called “New session” or `.tmp…` and have zero messages.
- Hiding Sessions and Tools reduces visible chrome, but the central area can still retain two simultaneously open zones.

Interpretation:

The app is optimized for an operator who already understands its workspace mechanics. A new user is asked to reason about sessions, zones, focus, docking, Live, Tools, and composer ownership at the same time. This is especially risky for the intended durable-Agent / disposable-Lane model: the user’s long-lived identity is not the main orientation point, while transient execution surfaces dominate the screen.

### 2. Lane/session navigation is session-first and noisy

Observed facts:

- The sidebar is labeled `SESSIONS`, with `Builds` and `Chats` modes.
- The visible list mixes empty new sessions, disposable `.tmp…` workspaces, demo repositories, real project paths, and chat/build entries.
- Each row exposes an adjacent “Actions for …” control.
- Browse provides `Active`, `Archive`, and `All`, plus filters for chat/build, folder, and tag.
- Browse showed `INBOX 17` even though the sidebar reported 102–103 total items; the relationship between the sidebar count, inbox count, and Browse scope is not explained inline.
- The session action menu contains `Rename…`, `Open beside`, `Set working directory…`, `Resume (load history)`, `Fork`, three rewind variants, `Compact server context`, `Archive`, `Delete permanently…`, and `Browse all sessions…`.
- Search can search messages, titles, tags, and folders and can include archived items.

Interpretation:

The app has many of the correct primitives for Lane lifecycle management, but they are exposed as a long action list on a session row. Archive, fork, rewind, resume, and delete have very different consequences; presenting them as near-peer menu entries increases the chance of a wrong action. “Session” also does not communicate whether the user is managing an identity, a lane, a transcript, or a live view.

### 3. Durable Agents are present conceptually but not visible as a primary object

Observed facts:

- The Tools panel has an `agents` category labeled `Persistent agents`.
- Its explanatory text says: `Durable Build identities and verified checkpoints. Resume is always an explicit operator action.`
- During refresh, the panel showed both:
  - `Could not refresh persistent agents: orchestration store /Users/chriscase/.grokptah/orchestration is already open (Resource temporarily unavailable (os error 35))`
  - `No persistent agents yet. Complete a Build turn to create one.`
- There is no visible relationship map such as Agent → active lanes, archived lanes, current run, or last checkpoint.
- The initial sidebar had a project named `GrokPtah-persistent-agent-continuity`, but it appeared as a BUILD session/workspace row rather than as an Agent identity.

Interpretation:

The current surface makes an important product concept look like a tool diagnostic. It also presents an empty-state statement immediately after a refresh error, which makes “none exist” indistinguishable from “could not load.” The user cannot confidently tell whether a durable Agent exists, whether it is attached to a lane, or whether a lane is simply named after an Agent.

### 4. Side-by-side zones can make ownership and context unclear

Observed facts:

- The app showed a `.tmp28GFtd · list files` lane with `Workspace unavailable` and a missing saved project.
- At the same time, a second zone showed a valid demo lane with a full transcript, tools, tests, diff-review evidence, and a composer.
- The valid zone was marked `FOCUS`, and the composer said it was sending to the valid lane in `ZONE 2`.
- The shared Tools panel still showed `Status (empty) Diff (no diff)` and worktree controls while the visible valid lane’s transcript described an implementation and completed tests.
- Switching focus did not make the context relationship obvious in the surrounding chrome.

Interpretation:

This is a concrete ownership/state problem. The product may be technically tracking multiple contexts correctly, but the visual hierarchy does not establish which lane owns the Tools panel, Git controls, terminal, status, approvals, or composer. A user could inspect or act on the wrong lane while believing they are operating on the focused one.

### 5. Failure and recovery states expose internal implementation details

Observed facts:

- Computer Use settings showed:
  - Screen Recording: `Not granted`
  - Accessibility: `Not granted`
  - Request controls disabled
  - instructions to grant access specifically to GrokPtah and restart
  - `Computer Use storage is unavailable: computer-use store /Users/chriscase/.grokptah/computer-use is already open (Resource temporarily unavailable (os error 35))`
- The Computer Run cockpit showed `Computer Run needs attention` and repeated the same raw lock error.
- Persistent Agents and Task Runs repeated raw orchestration-store errors with a filesystem path and OS error number.
- The missing-workspace state was more actionable: `Saved project is missing. No tools or model turns can run until you choose a valid directory.` followed by `Choose folder`.
- The selected missing-workspace lane also showed `Completion unverified: 0 files changed, no tests observed` and `Set a working directory, then send a prompt.`

Interpretation:

The app distinguishes some user-recoverable states well, but storage/bridge failures are presented as developer diagnostics rather than product recovery. The raw path and OS error may be useful in an expandable technical detail section, but they should not be the primary message. The combination of an error plus an empty-state claim is particularly misleading.

### 6. Computer Run and Computer Use terminology/state is inconsistent

Observed facts:

- The top-level Computer control is labeled `Computer` and opens `Computer Run cockpit`.
- The Sessions sidebar contains a toggle labeled `Computer Run Off` with `Value: off` in the normal state.
- The Computer Run cockpit is titled `Simulator` and includes `Owned by list files`.
- The cockpit showed a tab count and docking capacity of `3/3`.
- Settings describe Computer Use as `Read-only macOS observation` and state that keyboard and pointer control are disabled.
- The top-level controls and the sidebar control do not use the same noun or clearly communicate whether they control a global mode, a lane capability, or a cockpit view.

Interpretation:

“Computer,” “Computer Run,” “Computer Use,” and “Simulator” may be distinct technical concepts, but the UI does not teach the distinction at the point of use. The `Off` label alongside a top-level `Value: off` control and an available cockpit makes the state especially easy to misread.

### 7. Tools, task runs, Git, MCP, and terminal are powerful but fragmented

Observed facts:

- Tools categories include `files`, `git`, `mcp`, `plugins`, `skills`, `agents`, `tasks`, and `rules`.
- Task Runs describe `Durable progress and verification from desktop and MCP activity`, expose a source filter, a `Watch live` checkbox, Refresh/Retry controls, and a `No durable Build runs for this session yet` empty state.
- Task Runs also contain separate sections for multi-agent children and background/scheduled work, with `Schedule scan` and `Schedule shell…` actions.
- Git shows Status, Diff, `Agent edit diffs`, `Open last edit`, `Export transcript`, worktree creation/open/remove, Stage all, and Commit controls. For the context mismatch described above it showed `Status (empty) Diff (no diff)` despite a visible completed transcript in another zone.
- MCP explains that repo-local `.mcp.json` only runs after project trust, shows a disabled filesystem server, an Enable control, a configuration doctor, transport, probe, and `npx available: true`.
- The terminal opens inline with a session tab, New tab, Close tab, and an input labeled `Terminal input`. It opened empty during this pass.

Interpretation:

The product is exposing implementation topology—bridge, transport, probe, worktrees, MCP trust, task sources, agent edit diffs, terminals, zones—in a single operator workspace. This may be appropriate for an expert diagnostic mode, but the default flow needs a smaller user-facing status model that answers: what is running, what can it touch, what needs approval, what changed, and what should I do next?

### 8. Settings are comprehensive but dense

Observed facts:

- Settings is a side panel over the main workspace with sections: Defaults, Permissions, Computer Use, Appearance, Auth, About.
- Permissions explicitly says the controls are soft agent-side gates and “not an OS sandbox”; it contains permission mode, safety profile, mutating subagent folder behavior, allow/deny patterns, and `deny wins` rules.
- Auth includes xAI login, account state, console link, sign-out/clear-key actions, masked API key controls, and OpenAI-compatible gateway profiles with profile ID, display name, base URL, model ID, supported effort values, request budget, provider API key, Save, Discover models, and Qualify model.
- Appearance includes theme, accent, density, type scale, live preview, and an accent/density/type explanation.

Interpretation:

Settings gives users meaningful control, but mixes end-user decisions with developer/operator concepts. The security caveat is valuable yet intimidating when presented as a paragraph beside low-level pattern controls. Auth combines sign-in, key management, and custom provider configuration in one dense surface, which becomes more important if local-first and hosted deployment are both supported.

### 9. Empty, archive, and search states are not consistently informative

Observed facts:

- Archive Browse state showed `No sessions match this view`, with `Unarchive selected` disabled.
- Search starts with a field and disabled Search button, then returns compact result buttons labeled with session kind, title, and result type such as `title` or `message:user`.
- Search results included the same short title (`list files`) multiple times, requiring the user to use surrounding snippets and session kind to distinguish them.
- The missing-workspace empty state includes a direct Choose folder action and a clear prohibition on running tools/model turns.
- Persistent Agents and Task Runs combine failure messages with “none yet” empty states.

Interpretation:

The product has the basic empty-state patterns but lacks a shared state language. “No results,” “not configured,” “not loaded,” “not available,” “no runs,” and “no agents” need to be visibly different and to provide a next action appropriate to the state.

### 10. Accessibility and narrow-window observations

Observed facts:

- The accessibility tree exposes many useful labels and help strings for buttons, toggles, fields, and destructive actions.
- Several controls are icon-only or short labels whose meaning depends on Help text: `⚙`, `Term`, `Auto`, `Shared`, `Open beside`, `Undock zone`, and the Live focus/dock instruction.
- The primary content becomes very dense when all panels are open; hiding Sessions and Tools is possible through visible controls and keyboard shortcuts.
- A true narrow-window resize test was not completed in this pass. No conclusion is made about clipping, wrapping, or keyboard traversal at a reduced width.

Interpretation:

The semantic labeling foundation is better than the visual hierarchy. The next usability pass should test focus order, visible focus, keyboard access to session actions, destructive-action confirmation, reduced-width layouts, and whether tool/status ownership remains understandable when panels collapse.

## Prioritized findings

Priority definitions: P0 = blocks safe comprehension/recovery; P1 = major product comprehension or workflow risk; P2 = important polish/accessibility risk; P3 = follow-up refinement.

| ID | Priority | Finding | Evidence | Why it matters |
|---|---|---|---|---|
| F-01 | P0 | Store/bridge failures are shown as contradictory user states | `04-settings-computer-use.png`, `07-computer-cockpit.png`, `08-tools-agents.png`, `09-tools-tasks.png` | Users cannot tell whether the feature is empty, unavailable, loading, or broken; raw paths and OS errors leak internal details |
| F-02 | P1 | Agents and Lanes are not first-class navigation objects | `01-initial-builds.png`, `08-tools-agents.png`, `14-browse-sessions.png` | Durable identity work is hidden under Tools while disposable work dominates the sidebar; the intended product model is not discoverable |
| F-03 | P1 | Context ownership breaks across simultaneous zones | `18-valid-lane.png`, `19-terminal.png` | Tools/Git/status can appear to belong to a different lane than the focused composer/transcript, creating risk of wrong-lane actions |
| F-04 | P1 | Default information density is too high for a primary work surface | `01-initial-builds.png`, `12-live-rail.png`, `20-sessions-hidden.png` | 103 sessions, 8 Live sessions, multi-zone controls, and 8 Tools categories create a monitoring console rather than a clear coding workspace |
| F-05 | P1 | Lane lifecycle actions are too flat and too close in risk | Accessibility-only transient menu observation; `14-browse-sessions.png` | Resume, Fork, Rewind, Archive, and Delete permanently have different consequences but are exposed as peer actions |
| F-06 | P1 | Computer terminology and state model are inconsistent | `04-settings-computer-use.png`, `07-computer-cockpit.png` | Users cannot confidently distinguish observation, simulator/run cockpit, lane capability, or global mode |
| F-07 | P1 | Hosted/local distinction is not an obvious product-level choice | `06-settings-auth.png`, top-bar account/auth control | Auth and provider setup are visible, but the UI does not clearly explain where an Agent/Lane runs or how local and hosted operation relate |
| F-08 | P2 | Search results are hard to disambiguate when titles repeat | `16-search-empty.png`, `17-search-results.png` | Repeated short names like “list files” require users to infer identity from snippets and path fragments |
| F-09 | P2 | Security and MCP settings expose useful but intimidating internals | `03-settings-permissions.png`, `11-tools-mcp.png` | “Soft rails,” deny-wins patterns, stdio, probe, npx, and config paths are valuable diagnostics but poor default language |
| F-10 | P2 | Empty/loading/error/archive states lack a shared visual grammar | `08-tools-agents.png`, `09-tools-tasks.png`, `15-archive-empty.png`, `18-valid-lane.png` | Users must read long text to distinguish no data from failed refresh, missing workspace, or no matching archive items |
| F-11 | P2 | Accessibility labeling is stronger than visible discoverability | AX state across all screenshots; `20-sessions-hidden.png`, `21-panels-hidden.png` | Short/icon controls rely on Help strings; narrow-window and keyboard-only usability remain unverified |
| F-12 | P3 | Auth/provider configuration is too broad for a single settings section | `06-settings-auth.png` | Login, key storage, hosted gateways, model IDs, effort values, budgets, discovery, and qualification need clearer grouping |

## Task-flow inventory

| Flow | Current path exercised | Observed outcome | Confidence / gap |
|---|---|---|---|
| Start and choose work | Open app → Sessions → Build list → select row | 102–103 session items, many truncated/temporary titles; one selected lane had a missing workspace | High; representative local flow |
| Recover missing workspace | Select invalid lane → read status → Choose folder available | Clear next action; tools/model turns explicitly blocked | High; folder picker itself not opened |
| Continue a completed lane | Select valid demo lane → inspect transcript/status/composer | Transcript shows inspect/test/edit/test/diff sequence and idle state; second zone becomes focused | High; no new turn sent |
| Manage lane lifecycle | Open session actions | Rename, open beside, set cwd, resume, fork, rewind variants, compact, archive, delete | High; transient menu observed through AX, screenshot unavailable |
| Browse active/archive/all | Browse → Active/Archive/All | Active inbox 17; archive empty; archive actions disabled when empty | High |
| Search history | Search → enter `list files` → Search | Returns multiple session title/message hits with repeated titles and compact snippets | High |
| Run/live monitoring | Open Live rail | 8 open sessions, focus/dock instructions, one composer target, Model/Effort/Shared/Auto controls | High |
| Inspect persistent Agent | Tools → agents | Refresh failure from orchestration lock plus “No persistent agents yet” | High; no successful Agent record available |
| Inspect durable task runs | Tools → tasks | Refresh failure, source filter, Watch live, retry, no durable runs, multi-agent/background sections | High |
| Inspect Git/diffs/worktrees | Tools → git | Status/Diff empty for the shared Tools context; worktree controls available | High; context mismatch itself is the finding |
| Inspect terminal | Composer → Term | Terminal panel opens with tab, New tab, Close tab, input; no output | High; no command entered |
| Inspect permissions/approvals | Settings → Permissions; composer Auto/Shared | Permission mode, safety profile, allow/deny rules, Auto chip, Shared/Isolated wording visible | Medium; no live approval modal exercised |
| Inspect MCP | Tools → mcp | Project trust required, local/global config diagnostics, disabled filesystem server, Enable | High; no trust/enable action taken |
| Inspect Computer Use | Settings → Computer Use → Computer Run | macOS grants absent/disabled; storage lock; cockpit reports needs attention | High; successful capture/control not available |
| Inspect authentication/providers | Settings → Auth | xAI login/account, masked key, compatible gateway profiles and model controls | High; no credential changes |
| Archive/restore | Browse Archive and action menu | Archive view empty; Unarchive disabled; Archive action available on lane menu | Medium; no mutation performed |
| Interruption/recovery | Read Resume action, idle/stopped states, lock errors, missing workspace | Resume is explicit; error recovery is inconsistent | Medium; no active turn interrupted |
| Hosted/multi-device workflow | Look for connected hosted session or remote endpoint | No configured/observable hosted session in this build; only auth/provider setup was available | Low; must be a dedicated follow-up |

## Design recommendations for a later phase

These are recommendations, not changes made during Phase 1.

1. Make Agents and Lanes first-class navigation. Show durable Agent identities in one area, high-turnover Lanes in another, and make the relationship explicit: one Agent → active lanes, archived lanes, current run, checkpoint, and last known health.

2. Make the active Lane the single primary context. Every composer, Tool panel, terminal, Git view, approval, status card, and Computer Run surface should display the same lane identity and workspace summary. If multiple lanes are open, show an unmistakable context header on every contextual panel.

3. Replace raw infrastructure errors with a normalized state model. Primary copy should say what happened, whether data may be stale, and the next action. Put paths, bridge versions, transport, OS error numbers, and diagnostics behind “Technical details” or exportable support evidence.

4. Use progressive disclosure for operator surfaces. Default to one lane and one composer. Let users expand Live, Tools, terminal, Computer Run, task history, and MCP as contextual drawers or focused modes. Preserve the existing power features for expert workflows.

5. Separate lifecycle actions by intent and risk. Put Resume/Open/Fork in a continuation group; Rewind in a history group with impact copy; Archive in a reversible organization group; Delete permanently behind a clearly separated destructive affordance and confirmation.

6. Establish a shared state language and visual grammar: loading, ready, running, awaiting approval, blocked, disconnected, failed, unverified, archived, and empty. Each state should have a short explanation, an owner, and one next action.

7. Clarify local versus hosted operation at the product level. Provide a visible runtime/connection indicator for each Agent and Lane: Local, Hosted, Disconnected, Syncing, or Unknown. Explain whether history, memory, credentials, and workspace changes are local, cloud-resident, or selectively synchronized.

8. Treat search results as Lane records, not just text hits. Include a stable lane name, Agent identity, workspace/project, last activity, status, and a short matched excerpt so repeated titles are not ambiguous.

9. Keep security caveats but move implementation detail out of the first read. Explain the practical consequence first—what can run, where, and when approval is required—then provide the soft-rail caveat and raw patterns as an advanced view.

10. Make the responsive/keyboard contract explicit. Test at narrow widths, verify focus order and visible focus, ensure every icon-only control has a persistent accessible name, and confirm destructive actions are reachable without relying on pointer hover.

## Handoff brief for a design-capable Claude or Cursor model

Use this audit as research input. The next model should:

- preserve the current feature set and local-first capability;
- design around Durable Agents and disposable Lanes as separate objects;
- show how one Agent can own many active or archived Lanes;
- preserve Lane transcripts, runs, evidence, artifacts, tests, approvals, and history when a Lane is archived;
- make local versus hosted runtime and synchronization visible;
- prioritize one focused active Lane in the default view;
- provide an operator/advanced mode for Live, Tools, Computer Run, terminals, task runs, MCP, worktrees, and raw diagnostics;
- prototype states for ready, running, awaiting approval, blocked, missing workspace, disconnected, stale/refreshing, failed, archived, and no Agent/Lane;
- avoid inventing a successful hosted or Computer Use workflow—the current evidence does not establish either;
- keep observations and recommendations separate, and reference the screenshot IDs below.

## Screenshot index

All screenshots are in `docs/ux-audit/screenshots/`.

| ID | File | Surface / evidence |
|---|---|---|
| 01 | `01-initial-builds.png` | Initial three-panel workspace; 103 sessions; missing workspace; transcript and Live/tool surfaces |
| 02 | `02-settings.png` | Settings shell and Defaults section |
| 03 | `03-settings-permissions.png` | Permissions, safety profile, allow/deny rules, soft-rail caveat |
| 04 | `04-settings-computer-use.png` | Computer Use permissions and storage-lock failure |
| 05 | `05-settings-appearance.png` | Appearance controls and live preview |
| 06 | `06-settings-auth.png` | xAI login, masked key, compatible provider profiles |
| 07 | `07-computer-cockpit.png` | Computer Run cockpit, Simulator, dock capacity, lock failure |
| 08 | `08-tools-agents.png` | Persistent Agents panel, orchestration lock, misleading empty state |
| 09 | `09-tools-tasks.png` | Durable Task Runs, source filter, Watch live, retry/error, background sections |
| 10 | `10-tools-git.png` | Git status/diff/worktree/stage/commit controls |
| 11 | `11-tools-mcp.png` | MCP trust, config doctor, disabled filesystem server |
| 12 | `12-live-rail.png` | Live rail with 8 sessions, focus/dock model, composer ownership |
| 13 | — | Session action menu observed through accessibility state; transient menu did not provide a screenshot URL |
| 14 | `14-browse-sessions.png` | Browse Active view, Inbox 17, archive/delete row actions |
| 15 | `15-archive-empty.png` | Archive view with no matching sessions and disabled Unarchive |
| 16 | `16-search-empty.png` | Search shell before query; archive checkbox and filters |
| 17 | `17-search-results.png` | Search results for `list files`; repeated titles and message/title result types |
| 18 | `18-valid-lane.png` | Valid demo lane beside invalid lane; focused lane and shared-panel ambiguity |
| 19 | `19-terminal.png` | Terminal panel, tab controls, terminal input, composer relationship |
| 20 | `20-sessions-hidden.png` | Sessions sidebar hidden; multiple zones remain |
| 21 | `21-panels-hidden.png` | Sessions and Tools hidden; focused workspace still carries two zones |

## Phase 2 questions

Before implementing UI changes, answer these with a dedicated product/design pass:

1. What is the canonical object shown in the left navigation: Agent, Lane, transcript, or a selectable view over all three?
2. Can a Lane exist without a Durable Agent, and how is that represented visually?
3. What exactly is synchronized for a hosted Agent across devices: identity, memory, transcript, run state, workspace metadata, artifacts, or secrets?
4. What is the recovery owner for a locked orchestration/computer-use store: the user, another GrokPtah process, or an automatic repair path?
5. Which advanced surfaces should be default-visible for a new user versus an expert operator?
6. What is the minimum viable hosted workflow to exercise end-to-end in the next research pass?

