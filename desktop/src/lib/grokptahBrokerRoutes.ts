/**
 * Published ContextDesk broker route catalog for external workers.
 *
 * There is no in-tree HTTP broker. These templates are the production-grade
 * seam that `GrokPtahBrokerClient` and a ContextDesk server must share. The
 * client still uses the existing CSRF, idempotency, capability-class, and
 * fail-closed parser helpers; this module does not invent a second authority.
 */

export const EXTERNAL_WORKER_LIST_MAX_LIMIT = 100;
export const EXTERNAL_WORKER_LIST_DEFAULT_LIMIT = 20;
/** GrokPtah omitted value. Cursor REST defaults to true; brokers must send this flag. */
export const EXTERNAL_WORKER_LIST_INCLUDE_ARCHIVED_DEFAULT = false;

export type GrokPtahBrokerRouteClass = "read-only" | "execute";

export type GrokPtahBrokerExternalWorkerRouteId =
  | "launch"
  | "list"
  | "get"
  | "followUp"
  | "getRun"
  | "artifacts"
  | "cancel"
  | "archive"
  | "unarchive";

export type GrokPtahBrokerExternalWorkerRoute = {
  id: GrokPtahBrokerExternalWorkerRouteId;
  method: "GET" | "POST";
  path: string;
  defaultClass: GrokPtahBrokerRouteClass;
  csrf: boolean;
  idempotency: boolean;
  capability: "session.observe" | "run.execute";
};

export const GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES: readonly GrokPtahBrokerExternalWorkerRoute[] =
  Object.freeze([
    Object.freeze({
      id: "launch",
      method: "POST",
      path: "/bindings/{bindingId}/external-workers",
      defaultClass: "execute",
      csrf: true,
      idempotency: true,
      capability: "run.execute",
    }),
    Object.freeze({
      id: "list",
      method: "GET",
      path: "/bindings/{bindingId}/external-workers",
      defaultClass: "read-only",
      csrf: false,
      idempotency: false,
      capability: "session.observe",
    }),
    Object.freeze({
      id: "get",
      method: "GET",
      path: "/bindings/{bindingId}/external-workers/{agentId}",
      defaultClass: "read-only",
      csrf: false,
      idempotency: false,
      capability: "session.observe",
    }),
    Object.freeze({
      id: "followUp",
      method: "POST",
      path: "/bindings/{bindingId}/external-workers/{agentId}/runs",
      defaultClass: "execute",
      csrf: true,
      idempotency: true,
      capability: "run.execute",
    }),
    Object.freeze({
      id: "getRun",
      method: "GET",
      path: "/bindings/{bindingId}/external-workers/{agentId}/runs/{runId}",
      defaultClass: "read-only",
      csrf: false,
      idempotency: false,
      capability: "session.observe",
    }),
    Object.freeze({
      id: "artifacts",
      method: "GET",
      path: "/bindings/{bindingId}/external-workers/{agentId}/runs/{runId}/artifacts",
      defaultClass: "read-only",
      csrf: false,
      idempotency: false,
      capability: "session.observe",
    }),
    Object.freeze({
      id: "cancel",
      method: "POST",
      path: "/bindings/{bindingId}/external-workers/{agentId}/runs/{runId}/cancel",
      defaultClass: "execute",
      csrf: true,
      idempotency: true,
      capability: "run.execute",
    }),
    Object.freeze({
      id: "archive",
      method: "POST",
      path: "/bindings/{bindingId}/external-workers/{agentId}/archive",
      defaultClass: "execute",
      csrf: true,
      idempotency: true,
      capability: "run.execute",
    }),
    Object.freeze({
      id: "unarchive",
      method: "POST",
      path: "/bindings/{bindingId}/external-workers/{agentId}/unarchive",
      defaultClass: "execute",
      csrf: true,
      idempotency: true,
      capability: "run.execute",
    }),
  ]);

export function grokptahBrokerExternalWorkerRoute(
  id: GrokPtahBrokerExternalWorkerRouteId,
): GrokPtahBrokerExternalWorkerRoute {
  const route = GROKPTAH_BROKER_EXTERNAL_WORKER_ROUTES.find((item) => item.id === id);
  if (!route) {
    throw new Error(`unknown external-worker broker route: ${id}`);
  }
  return route;
}

/** Collection path used by launch (POST) and list (GET + query). */
export function grokptahBrokerExternalWorkerCollectionPath(bindingSegment: string): string {
  return `/bindings/${bindingSegment}/external-workers`;
}

/**
 * List always emits `includeArchived=true|false`. Omitted GrokPtah queries
 * serialize as false so a broker cannot inherit Cursor REST's default true.
 */
export function grokptahBrokerExternalWorkerListPath(
  bindingSegment: string,
  query: { limit?: number; cursor?: string; includeArchived?: boolean } = {},
): string {
  const params = new URLSearchParams();
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.cursor !== undefined) params.set("cursor", query.cursor);
  params.set(
    "includeArchived",
    query.includeArchived === true ? "true" : "false",
  );
  return `${grokptahBrokerExternalWorkerCollectionPath(bindingSegment)}?${params.toString()}`;
}

export function grokptahBrokerExternalWorkerArchivePath(
  bindingSegment: string,
  agentSegment: string,
): string {
  return `/bindings/${bindingSegment}/external-workers/${agentSegment}/archive`;
}

export function grokptahBrokerExternalWorkerUnarchivePath(
  bindingSegment: string,
  agentSegment: string,
): string {
  return `/bindings/${bindingSegment}/external-workers/${agentSegment}/unarchive`;
}
