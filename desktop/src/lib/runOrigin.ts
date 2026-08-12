import type { DurableRun, RunOrigin } from "./protocol";

/** Return the origin of the currently active run, if one exists. */
export function activeRunOrigin(runs: DurableRun[]): RunOrigin | null {
  const live = runs.find((run) => run.state === "running" || run.state === "queued");
  if (live?.clientId === "mcp") return "mcp";
  if (live?.clientId === "desktop") return "desktop";
  return live ? "other" : null;
}
