import type { ReactNode } from "react";

export type StateCardVariant = "empty" | "loading" | "error" | "stale" | "archived";

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
  return (
    <div className={`state-card state-card-${variant}`}>
      <div className="state-card-mark" aria-hidden>
        {variant === "error" ? "!" : variant === "loading" ? "…" : variant === "archived" ? "◌" : "•"}
      </div>
      <div className="state-card-body">
        <div role={variant === "error" ? "alert" : undefined}>
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
