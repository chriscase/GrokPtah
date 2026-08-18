import type { ReactNode } from "react";

export type StateCardVariant =
  | "empty"
  | "loading"
  | "error"
  | "stale"
  | "disconnected"
  | "blocked"
  | "interrupted"
  | "queued"
  | "approval"
  | "archived"
  | "retired"
  | "proposed";

type StateCardTone = "neutral" | "info" | "attention" | "danger";

const STATE_PRESENTATION: Record<
  StateCardVariant,
  { mark: string; tone: StateCardTone; announce: "alert" | "status" | null }
> = {
  empty: { mark: "○", tone: "neutral", announce: null },
  loading: { mark: "…", tone: "info", announce: "status" },
  error: { mark: "!", tone: "danger", announce: "alert" },
  stale: { mark: "↻", tone: "attention", announce: "status" },
  disconnected: { mark: "⌁", tone: "attention", announce: "status" },
  blocked: { mark: "!", tone: "danger", announce: "alert" },
  interrupted: { mark: "Ⅱ", tone: "attention", announce: "status" },
  queued: { mark: "↥", tone: "info", announce: "status" },
  approval: { mark: "?", tone: "attention", announce: "status" },
  archived: { mark: "◌", tone: "neutral", announce: null },
  retired: { mark: "◇", tone: "neutral", announce: null },
  proposed: { mark: "✦", tone: "info", announce: null },
};

export type StateCardProps = {
  variant: StateCardVariant;
  title: string;
  description: string;
  actionLabel?: string;
  onAction?: () => void;
  technicalDetail?: string | null;
  children?: ReactNode;
};

/** Shared product-language state treatment for panels and drawers. */
export function StateCard({
  variant,
  title,
  description,
  actionLabel,
  onAction,
  technicalDetail,
  children,
}: StateCardProps) {
  const presentation = STATE_PRESENTATION[variant];
  return (
    <div
      className={`state-card state-card-${variant} state-card-tone-${presentation.tone}`}
      data-state={variant}
    >
      <div className="state-card-mark" aria-hidden>
        {presentation.mark}
      </div>
      <div className="state-card-body">
        <div
          role={presentation.announce ?? undefined}
          aria-live={presentation.announce === "status" ? "polite" : undefined}
        >
          <strong>{title}</strong>
          <p>{description}</p>
        </div>
        {children}
        {technicalDetail && (
          <details className="state-card-details">
            <summary>Technical details</summary>
            <pre>{technicalDetail}</pre>
          </details>
        )}
        {actionLabel && onAction && (
          <button type="button" onClick={onAction}>
            {actionLabel}
          </button>
        )}
      </div>
    </div>
  );
}
