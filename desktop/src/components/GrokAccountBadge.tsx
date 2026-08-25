import { useId, useState } from "react";

import {
  canLaunchGrokBuild,
  grokAccountNotice,
  type GrokAccountFacts,
} from "../lib/grokAccountFacts";

type Props = {
  /** Parsed account facts, or `null` when the host reported something invalid. */
  facts: GrokAccountFacts | null;
  /** Where the badge is rendered; only affects density, never the copy. */
  placement?: "composer" | "settings";
  /** Invoked by the re-authentication affordance when a launch is blocked. */
  onReauthenticate?: () => void;
};

/**
 * Safe route/account/method/expiry status for the Grok Build credential.
 *
 * Everything it renders comes from the bounded `grokptah.account.v1`
 * projection, so no bearer, refresh token, API key, credential reference, or
 * raw `auth_mode` can reach the DOM or the accessibility tree.
 *
 * Accessibility contract exercised by `GrokAccountBadge.test.tsx`:
 * - the trigger is a real `<button>`, so it is tabbable and Enter/Space work;
 * - `aria-expanded`/`aria-controls` tie it to the detail region it toggles;
 * - blocking states announce via `role="status"` + `aria-live="polite"`;
 * - the tone is carried by text as well as color, so forced-colors and
 *   monochrome renderings stay unambiguous;
 * - layout is em/ch-based so 200% text reflows instead of clipping.
 */
export function GrokAccountBadge({ facts, placement = "composer", onReauthenticate }: Props) {
  const [open, setOpen] = useState(false);
  const detailId = useId();
  const notice = grokAccountNotice(facts);
  const launchable = canLaunchGrokBuild(facts);

  return (
    <div className={`grok-account-badge is-${notice.tone} at-${placement}`}>
      <button
        type="button"
        className="grok-account-trigger"
        aria-expanded={open}
        aria-controls={detailId}
        onClick={() => setOpen((value) => !value)}
      >
        {/* Decorative: the tone is already carried by the text below. */}
        <span className="grok-account-dot" aria-hidden="true" />
        <span className="grok-account-tone">
          {notice.tone === "ready" ? "Ready" : notice.tone === "unknown" ? "Unknown" : "Blocked"}
        </span>
        <span className="grok-account-summary">{notice.summary}</span>
      </button>
      <div
        id={detailId}
        className="grok-account-detail"
        hidden={!open}
        // Blocking states are announced when they appear; a healthy account
        // is not worth interrupting a screen-reader user for.
        {...(notice.blocksLaunch ? { role: "status" as const, "aria-live": "polite" as const } : {})}
      >
        <p className="grok-account-detail-text">{notice.detail}</p>
        {notice.remedy && <p className="grok-account-remedy">{notice.remedy}</p>}
        {!launchable && onReauthenticate && (
          <button type="button" className="grok-account-reauth" onClick={onReauthenticate}>
            Refresh Grok Build sign-in
          </button>
        )}
        {!launchable && (
          <p className="grok-account-scope">
            New runs are disabled. Runs already recorded stay open for inspection.
          </p>
        )}
      </div>
    </div>
  );
}
