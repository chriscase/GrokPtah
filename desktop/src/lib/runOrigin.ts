import type { DurableRun, RunOrigin } from "./protocol";

/** Host-minted local desktop origin. The only origin that inherits local Apply/Keep. */
export const LOCAL_DESKTOP_RUN_CLIENT_ID = "local-desktop";
/** Primary MCP coordinator credential wire value. */
export const MCP_RUN_CLIENT_ID = "mcp";
/**
 * Historical local attribution string. Also a valid named-credential id, so it
 * is ambiguous and must fail closed rather than inherit local authority.
 */
export const LEGACY_DESKTOP_RUN_CLIENT_ID = "desktop";

export function isLocalDesktopRunOrigin(
  clientId: string | null | undefined,
): boolean {
  return clientId === LOCAL_DESKTOP_RUN_CLIENT_ID;
}

/**
 * Origins that may use the existing scoped durable approval path.
 *
 * MCP primary (`mcp`) and non-primary named credential ids qualify.
 * `local-desktop`, legacy `desktop`, missing, and empty origins do not.
 */
export function isScopedDurableApprovalOrigin(
  clientId: string | null | undefined,
): boolean {
  return (
    typeof clientId === "string" &&
    clientId.length > 0 &&
    clientId !== LOCAL_DESKTOP_RUN_CLIENT_ID &&
    clientId !== LEGACY_DESKTOP_RUN_CLIENT_ID
  );
}

/** Every origin except host-minted local-desktop must not inherit local Apply. */
export function runRequiresDurableApproval(
  clientId: string | null | undefined,
): boolean {
  return !isLocalDesktopRunOrigin(clientId);
}

/** Return the origin of the currently active run, if one exists. */
export function activeRunOrigin(runs: DurableRun[]): RunOrigin | null {
  const live = runs.find((run) => run.state === "running" || run.state === "queued");
  if (live?.clientId === MCP_RUN_CLIENT_ID) return "mcp";
  if (live?.clientId === LOCAL_DESKTOP_RUN_CLIENT_ID) return "desktop";
  return live ? "other" : null;
}
