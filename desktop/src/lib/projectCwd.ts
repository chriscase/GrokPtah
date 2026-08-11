import type { AgentStatus } from "./protocol";

/** Keep a just-selected folder visible while an older bridge snapshot catches up. */
export function preserveProjectCwd(
  status: AgentStatus,
  hint: string | null,
): AgentStatus {
  if (status.project_cwd || !hint) return status;
  return { ...status, project_cwd: hint };
}
