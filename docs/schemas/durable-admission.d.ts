/**
 * Durable admission projections.
 *
 * The public shape of one admitted unit of agent work, shared by the SDK, the
 * MCP control plane, the web broker, and the desktop. This is a narrowing of
 * the durable ledger, never a re-serialization of it: execution input,
 * credential fingerprints, and internal identities are absent by construction.
 *
 * Kept in lockstep with `durable-admission.schema.json` and with
 * `grokptah-agent-bridge::orchestration::projection` by a test that fails if
 * any of the three names a field the others do not.
 */

export type RunState =
  | 'queued'
  | 'running'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'interrupted'
  | 'limit_reached';

/**
 * Public form of an attempt lease's state. `expired` is distinct from `held`
 * because it is the state a reconciler acts on.
 */
export type AttemptProjectionState = 'held' | 'released' | 'expired';

/**
 * What is durably known about whether the work reached the provider.
 *
 * `known_not_sent` and `uncertain` must never be collapsed by a consumer:
 * only the first is safe to attempt again. `uncertain` means the request may
 * have been accepted, billed, and acted on, and the outcome was never
 * observed.
 */
export type ProviderSendProjectionState =
  | 'known_not_sent'
  | 'sending'
  | 'uncertain'
  | 'sent';

export interface AdmissionProjection {
  projectionVersion: number;
  runId: string;
  sessionId: string;
  workspace: string;
  state: RunState;
  specKey: string | null;
  queuePosition: number | null;
  attempt: number | null;
  attemptState: AttemptProjectionState | null;
  providerSendState: ProviderSendProjectionState | null;
  promptPreview: string;
  terminalResult: string | null;
  errorCode: string | null;
  createdAt: string;
  updatedAt: string;
}

/**
 * A run may only be presented as successfully completed when its work is known
 * to have reached the provider. Anything else shown as success is fabricated.
 */
export function permitsCompletion(
  state: ProviderSendProjectionState | null,
): boolean {
  return state === 'sent';
}

/**
 * Only provably-unsent work may be carried into a new attempt without asking a
 * human first.
 */
export function permitsNewAttempt(
  state: ProviderSendProjectionState | null,
): boolean {
  return state === 'known_not_sent';
}
