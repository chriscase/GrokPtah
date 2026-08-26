/**
 * Durable admission projections.
 *
 * The public shape of one admitted unit of agent work, shared by the SDK, the
 * MCP control plane, the web broker, and the desktop. This is a narrowing of
 * the durable ledger, never a re-serialization of it: execution input,
 * credential material, and internal identities are absent by construction.
 *
 * This is a *declaration* file. It contains types and `declare`d signatures
 * only — no implementations. An earlier revision shipped function bodies here,
 * which is not valid in a `.d.ts` and would fail any consumer that type-checks
 * its dependencies. The runtime helpers live beside this file in the consuming
 * package; what is guaranteed here is their shape.
 *
 * Kept in lockstep with `durable-admission.schema.json` and with
 * `grokptah-agent-bridge::orchestration::projection` by a test that fails if
 * any of the three names a field the others do not.
 */

export type RunState =
  | 'queued'
  /**
   * The interval between an attempt taking this run's lease and its worker
   * acknowledging. A real state, not a transient: a crash inside it leaves a
   * lease held and work that may have begun.
   */
  | 'starting'
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
  /** Seconds since the holding attempt last heartbeat its lease. */
  heartbeatAgeSeconds: number | null;
  /** Seconds until the lease expires; negative once it has. */
  leaseExpiresInSeconds: number | null;
  /**
   * Fingerprint of the concrete provider, model, endpoint, and credential this
   * work is bound to. A fingerprint, so a route change is visible without any
   * endpoint or credential being published.
   */
  routeRevision: string | null;
  /**
   * A previous teardown could not be established. The run's lease and capacity
   * are fenced and no new attempt is authorized until it is resolved.
   */
  teardownUncertain: boolean;
  /** Bounded, redacted explanation of that uncertainty. */
  teardownDetail: string | null;
  /**
   * Whether a new attempt is currently permitted. False whenever the outcome
   * of previous work is unknown — the case where retrying risks doing it twice.
   */
  retryEligible: boolean;
  /** Whether this run currently occupies an admission slot. */
  capacityFenced: boolean;
  /** Remaining wall-clock budget from the sealed bounds. */
  remainingDurationMs: number | null;
  /** Round budget from the sealed bounds. */
  maxRounds: number;
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
export declare function permitsCompletion(
  state: ProviderSendProjectionState | null,
): boolean;

/**
 * Only provably-unsent work may be carried into a new attempt without asking a
 * human first.
 */
export declare function permitsNewAttempt(
  state: ProviderSendProjectionState | null,
): boolean;

/**
 * Whether a UI may offer a retry control for this run.
 *
 * Prefer this over inspecting `state`: a run can be terminal and still unsafe
 * to retry, because what is unknown is whether its previous work ran.
 */
export declare function canOfferRetry(
  projection: AdmissionProjection,
): boolean;
