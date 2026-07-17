# GrokPtah backlog audit (honest board)

**Date:** 2026-07-17  
**Tip at audit close:** see latest `main` commit that lands this file.

## Process rules (this goal)

- No mass closes; each close cites SHA + proof command.
- Partial work stays open or is residual-linked.
- False closes reopened with gap comments before feature work.

## Wave 0 — Reopened (false / over-closed)

| Issue | Why reopened |
|-------|----------------|
| **#52** | Background/scheduled panel is WalkDir demo, not real agent tasks |
| **#43** | Worktree **create** missing (list only) |
| **#38** | Fork works; **resume/continue** does not |
| **#93** | Offline smoke closed as CLI parity |
| **#122** | Disk fix only; no React.memo (since fixed in this goal) |
| **#123** | Stick only; jump-to-latest missing (since fixed in this goal) |

**Not reopened** (open successors already track work):

| Closed stub | Open tracker |
|-------------|--------------|
| #51 subagent feed | **#152** |
| #90 GP spawn | **#151** |
| #30 prompt queue | **#147** |
| #32 full slash | **#148** |
| #39 file rewind | **#146** |

**#77** Phase 15 epic: left closed as historical delivery snapshot; residual map commented on the issue (#151/#152/#93/#146–#149/#144/#145).

## Wave 0 — Epics

| Epic | Action |
|------|--------|
| #101–#109 | Bodies rewritten with real `#NNN` checkboxes matching open/closed |
| **#102** | **Closed honestly** — children #115–#119 all done |
| #103 | Open until #122/#123 complete (then can reassess) |
| #101, #104–#109 | Stay open with accurate children |

## Completed in this goal (close only when AC met)

| Issue | Proof |
|-------|--------|
| **#140** | npm ci + fmt/clippy -D warnings + src-tauri tests (`881e4a8`) |
| **#127** | You/Grok role labels + borders |
| **#142** | pty backlog tests + session_store unit tests in CI |
| **#130** | displaySessionTitle + pane wiring |
| **#122** | `memo()` + **stable** `onFocusSession`/`onClosePane` id handlers (not per-dock lambdas); SessionPane, MarkdownBody, ToolCallCard, FleetStrip; MD_COMPONENTS hoisted; `openTabIdsKey` persist; `npm test` `memoIsolation.test.ts` |
| **#123** | Stick hysteresis + **Jump to latest ↓** button in SessionPane; structural test |
| **#135** | Retain `Child`, kill+wait+drop FDs; natural exit reaps; unit `kill_unknown` |
| **#136** | `termEverOpened` + CSS `.is-collapsed`; no kill-on-unmount; reattach/list+backlog |
| **#137** | Listed FS/git/session cmds `async` + `spawn_blocking` via `run_blocking` |
| **#138** | Cap backlog, seq+watermark, UTF-8 split buffer, `pty://exit`; FE queue during apply |

## Still open (do not mass-close)

- Multi-agent: **#151**, **#152**
- Product honesty: **#52**, **#43**, **#38**, **#93**
- TUI parity: **#146–#149**
- CI residual: **#141**
- UX polish: **#126**, **#128–#129**, **#132–#134**, **#150**
- Architecture: **#144**, **#145**
- Permissions residual: **#114**
- Epic children residual under #106 if any remaining shell items

## Anti-patterns avoided

- No bulk “delivery complete” multi-issue close
- No closing #90-style stubs as full multi-agent
- Epics not closed while children open (except #102 where all children done)
