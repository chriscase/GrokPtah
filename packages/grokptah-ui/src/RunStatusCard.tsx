import { useId } from "react";

/**
 * The only data contract accepted by RunStatusCard.
 *
 * The optional progress object intentionally contains no run identity,
 * request content, result, error, event, path, URL, callback, or action.
 * Producers with a wider structural projection may pass it directly; this
 * component reads only state and the two bounded round-budget numbers.
 */
export type RunStatusSnapshot = {
  readonly state:
    | "queued"
    | "running"
    | "completed"
    | "failed"
    | "cancelled"
    | "interrupted"
    | "limit_reached";
  readonly progress?: Readonly<{
    readonly round: number;
    readonly maxRounds: number;
  }> | null;
};

type RunStatusState = RunStatusSnapshot["state"];

type StatusCopy = {
  readonly label: string;
  readonly description: string;
  readonly live: string;
};

const MAX_ROUNDS = 100;
const RUN_STATUS_STATES = [
  "queued",
  "running",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "limit_reached",
] as const satisfies readonly RunStatusState[];

const STATUS_COPY: Record<RunStatusState, StatusCopy> = {
  queued: {
    label: "Queued",
    description: "The run is waiting to start.",
    live: "Run is queued.",
  },
  running: {
    label: "Running",
    description: "The run is in progress.",
    live: "Run is running.",
  },
  completed: {
    label: "Completed",
    description: "The run has completed.",
    live: "Run completed.",
  },
  failed: {
    label: "Failed",
    description: "The run failed.",
    live: "Run failed.",
  },
  cancelled: {
    label: "Cancelled",
    description: "The run was cancelled.",
    live: "Run was cancelled.",
  },
  interrupted: {
    label: "Interrupted",
    description: "The run was interrupted.",
    live: "Run was interrupted.",
  },
  limit_reached: {
    label: "Limit reached",
    description: "The run reached its configured limit.",
    live: "Run reached its configured limit.",
  },
};

const UNAVAILABLE_COPY: StatusCopy = {
  label: "Unavailable",
  description: "The current run status is unavailable.",
  live: "Run status is unavailable.",
};

type NormalizedRoundBudget = {
  readonly round: number;
  readonly maxRounds: number;
};

function isRunStatusState(value: unknown): value is RunStatusState {
  return (
    typeof value === "string" &&
    (RUN_STATUS_STATES as readonly string[]).includes(value)
  );
}

function normalizeRoundBudget(
  progress: RunStatusSnapshot["progress"] | undefined,
): NormalizedRoundBudget | null {
  if (
    progress === undefined ||
    progress === null ||
    typeof progress !== "object" ||
    Array.isArray(progress)
  ) {
    return null;
  }

  const { round, maxRounds } = progress;
  if (
    !Number.isSafeInteger(round) ||
    !Number.isSafeInteger(maxRounds) ||
    round < 0 ||
    maxRounds < 1 ||
    round > maxRounds ||
    maxRounds > MAX_ROUNDS
  ) {
    return null;
  }

  return {
    round,
    maxRounds,
  };
}

export function RunStatusCard({
  snapshot,
}: {
  readonly snapshot: RunStatusSnapshot;
}) {
  const id = useId();
  const titleId = `${id}-title`;
  const descriptionId = `${id}-description`;
  const stateValue =
    snapshot !== null &&
    typeof snapshot === "object" &&
    "state" in snapshot
      ? snapshot.state
      : undefined;
  const state = isRunStatusState(stateValue) ? stateValue : null;
  const copy = state === null ? UNAVAILABLE_COPY : STATUS_COPY[state];
  const roundBudget = normalizeRoundBudget(
    snapshot !== null &&
      typeof snapshot === "object" &&
      "progress" in snapshot
      ? snapshot.progress
      : undefined,
  );

  return (
    <article
      className="gpt-ui-run-status-card"
      data-state={state ?? "unavailable"}
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
    >
      <header className="gpt-ui-run-status-card__header">
        <h2 id={titleId} className="gpt-ui-run-status-card__title">
          Run status
        </h2>
        <span className="gpt-ui-run-status-card__state">{copy.label}</span>
      </header>
      <p id={descriptionId} className="gpt-ui-run-status-card__description">
        {copy.description}
      </p>
      <p
        className="gpt-ui-run-status-card__live"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {copy.live}
      </p>
      {roundBudget !== null && (
        <>
          <p className="gpt-ui-run-status-card__budget">
            Round {roundBudget.round} of {roundBudget.maxRounds} maximum
          </p>
          <meter
            className="gpt-ui-run-status-card__budget-meter"
            aria-label="Round budget used"
            min={0}
            max={roundBudget.maxRounds}
            value={roundBudget.round}
          />
        </>
      )}
    </article>
  );
}
