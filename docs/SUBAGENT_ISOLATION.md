# Subagent working folders

GrokPtah gives every mutating general-purpose subagent its own working folder
by default. It prefers a detached Git worktree under
`.grokptah/worktrees/sub-<id>` and falls back to a project copy when Git
worktrees are unavailable. The worktree is populated from the live project
snapshot, including uncommitted and untracked files, rather than from `HEAD`
alone.

Isolation is prepared before the child starts. If neither strategy succeeds,
the child is recorded as failed and does not run in the parent folder.

Plan and explore children remain in the parent working folder because they are
read-only. Their write, patch, shell, and memory mutators stay blocked by the
capability gate.

## Shared-folder opt-in

Settings > Permissions > Mutating subagent folders can explicitly switch
general-purpose children to the shared project folder. The child card then
shows `Shared / writes enabled` and a warning. This mode lets parallel children
and the parent edit the same files and should be used only when that
coordination risk is intentional.

`GROKPTAH_SUBAGENT_ISOLATION=worktree` (or `1`) forces worktree mode.
`GROKPTAH_SUBAGENT_ISOLATION=shared` is the only environment value that enables
shared-folder mutation. An environment override is visible and locks the
desktop setting.

Worktree separation prevents routine edit collisions; it is not an operating
system sandbox. Shell commands still run under the normal GrokPtah permission
and safety controls.
