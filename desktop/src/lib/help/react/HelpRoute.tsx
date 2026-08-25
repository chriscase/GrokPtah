/**
 * The dedicated Help route and command palette.
 *
 * One searchable surface, embeddable by the desktop and by any consumer, with
 * the accessibility behavior that a modal search surface has to get right:
 *
 *   - **Inert background.** While the palette is open the rest of the page is
 *     marked `aria-hidden` and `inert`, so a screen reader cannot walk into
 *     content the user cannot see and Tab cannot land behind the overlay.
 *   - **Focus is restored.** Whatever was focused before opening is refocused
 *     on close, including when the close came from Escape rather than a click.
 *   - **Status is announced.** One polite live region carries result counts,
 *     spelling corrections, redaction notices, and the offline/provider state.
 *   - **Forced colors and reduced motion.** State is carried by ARIA and text
 *     rather than color, and no transition is applied when the user has asked
 *     for reduced motion.
 *   - **400% zoom and narrow viewports.** Sizing is in relative units with no
 *     fixed pixel heights, `overflow: hidden`, or `nowrap`, so the surface
 *     reflows instead of clipping.
 *
 * The route renders nothing about the provider unless one is configured, and
 * says so plainly when none is: offline retrieval is the product, not a
 * degraded mode.
 */
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { createHelpSearchController, type HelpSearchState } from "../consumer";
import type { HelpCitation, HelpRetrievalOptions, HelpRetrievalResult } from "../retrieval/hybrid";
import { HelpResults, HelpSearchInput, useHelpSearch } from "./primitives";

export type HelpProviderState =
  | { readonly kind: "offline" }
  | { readonly kind: "configured"; readonly providerLabel: string }
  | { readonly kind: "unavailable"; readonly providerLabel: string; readonly detail: string };

export type HelpRouteProps = {
  readonly open: boolean;
  readonly onClose: () => void;
  /** Where the user is, so the empty state can suggest something relevant. */
  readonly context?: { readonly label: string; readonly seedQuery?: string };
  readonly provider?: HelpProviderState;
  readonly retrieval?: HelpRetrievalOptions;
  readonly onActivate?: (result: HelpRetrievalResult) => void;
  readonly hrefFor?: (citation: HelpCitation) => string | undefined;
  /** Element made inert while the palette is open. Defaults to the app root. */
  readonly backgroundRef?: { current: HTMLElement | null };
  readonly showScoreComponents?: boolean;
};

function describeProvider(provider: HelpProviderState): string {
  switch (provider.kind) {
    case "configured":
      return `Answers can be drafted by ${provider.providerLabel}. Search itself runs offline.`;
    case "unavailable":
      return `${provider.providerLabel} is unavailable (${provider.detail}). Search still works offline.`;
    default:
      // Not an error state: retrieval is fully useful with no provider at all.
      return "No answer provider is configured. Help search runs entirely offline.";
  }
}

export function HelpRoute({
  open,
  onClose,
  context,
  provider = { kind: "offline" },
  retrieval,
  onActivate,
  hrefFor,
  backgroundRef,
  showScoreComponents = false,
}: HelpRouteProps): JSX.Element | null {
  const titleId = useId();
  const providerId = useId();
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const restoreFocusRef = useRef<Element | null>(null);
  const retrievalRef = useRef(retrieval);
  retrievalRef.current = retrieval;

  const controller = useMemo(() => createHelpSearchController(retrievalRef.current ?? {}), []);
  const { state } = useHelpSearch({}, controller);
  const [seeded, setSeeded] = useState(false);

  // Remember the opener before the dialog takes focus, so Escape and a click
  // on the backdrop both return the user where they were.
  useEffect(() => {
    if (!open) return undefined;
    restoreFocusRef.current = document.activeElement;
    return () => {
      const target = restoreFocusRef.current;
      if (target instanceof HTMLElement && document.contains(target)) target.focus();
    };
  }, [open]);

  // Mark the rest of the app inert. `inert` is what actually stops Tab and
  // assistive-technology traversal; `aria-hidden` alone leaves it tabbable.
  useEffect(() => {
    const background = backgroundRef?.current ?? document.getElementById("root");
    if (!open || !background) return undefined;
    background.setAttribute("aria-hidden", "true");
    background.setAttribute("inert", "");
    return () => {
      background.removeAttribute("aria-hidden");
      background.removeAttribute("inert");
    };
  }, [open, backgroundRef]);

  useEffect(() => {
    if (!open) {
      controller.clear();
      setSeeded(false);
      return;
    }
    if (!seeded && context?.seedQuery) {
      controller.search(context.seedQuery);
      setSeeded(true);
    }
  }, [open, seeded, context?.seedQuery, controller]);

  useEffect(() => () => controller.dispose(), [controller]);

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      // Contain focus. Without this, Tab from the last control escapes to the
      // browser chrome and the user cannot get back without a mouse.
      const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (!focusable || focusable.length === 0) return;
      const first = focusable[0]!;
      const last = focusable[focusable.length - 1]!;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    },
    [onClose],
  );

  if (!open) return null;

  return (
    <div
      className="help-route-backdrop"
      data-help-surface="route"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={providerId}
        className="help-route"
        onKeyDown={handleKeyDown}
      >
        <h1 id={titleId}>Help</h1>
        {context ? (
          <p data-help-part="context">
            {/* Context is shown as text so it survives forced-colors mode and
                is announced rather than implied by placement. */}
            Searching Help from {context.label}
          </p>
        ) : null}

        <HelpSearchInput controller={controller} state={state} label="Search Help" />

        <p id={providerId} data-help-part="provider" role="note">
          {describeProvider(provider)}
        </p>

        <HelpResults
          state={state}
          controller={controller}
          onActivate={onActivate}
          hrefFor={hrefFor}
          showScoreComponents={showScoreComponents}
        />

        <HelpRouteFooter state={state} />

        <button type="button" onClick={onClose} data-help-part="close">
          Close Help
        </button>
      </div>
    </div>
  );
}

/**
 * Provenance footer.
 *
 * Showing which corpus produced the results is what lets a user or a reviewer
 * tell a stale index from a current one without reading logs.
 */
export function HelpRouteFooter({ state }: { state: HelpSearchState }): JSX.Element {
  return (
    <p data-help-part="provenance">
      <span>Corpus {state.corpusDigest.slice(0, 19)}…</span>
      {state.redactedQuery ? (
        <span data-help-part="redaction-note">
          {" "}
          A credential in your query was removed before searching and was not sent anywhere.
        </span>
      ) : null}
    </p>
  );
}

/**
 * Keyboard shortcut hook for opening the palette.
 *
 * Bound on `keydown` at the document so it works regardless of focus, and it
 * declines to fire while the user is typing in another field.
 */
export function useHelpPaletteShortcut(onOpen: () => void, enabled = true): void {
  useEffect(() => {
    if (!enabled) return undefined;
    const handler = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "/") return;
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)
      ) {
        return;
      }
      event.preventDefault();
      onOpen();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onOpen, enabled]);
}
