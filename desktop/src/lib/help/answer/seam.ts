/**
 * The seam between Help and a host authority.
 *
 * This lane does not decide who may ask a Help question, which provider
 * answers it, under what tenant or model identity, or whether a given source
 * may be surfaced. Those decisions belong to the reviewed authority spine,
 * which is not published yet.
 *
 * Building a second one here would be worse than leaving a gap. The product
 * would have two answers to the same question, and the one living in the
 * renderer is the one an attacker edits. The previous shape of this module
 * demonstrated exactly that: it minted a "route" by hashing the caller's own
 * `providerId`, `tenantId`, and `modelId`. The digest was self-consistent for
 * whatever values the caller chose, so it proved the fields had not been
 * edited *after* the caller picked them, and never that a host would allow
 * them. A caller wanting a different provider named one and hashed it.
 *
 * So this file declares a port and stops. What sits behind it is out of scope
 * for this branch by decision, not by omission — see
 * `docs/HELP_ANSWER_AUTHORITY_SEAM.md` for the exact handoff.
 *
 * ## What Help does on its side
 *
 * Shapes a bounded, tool-free, non-persistent request; digests it; validates
 * whatever comes back against that request; verifies every quote against the
 * corpus; binds every citation to a claim; refuses on any disagreement.
 *
 * ## What Help never does
 *
 * Choose a route. Mint or verify an admission. Hold key material. Talk to a
 * provider. Decide a principal's capabilities. Persist anything.
 *
 * ## Why the result is opaque
 *
 * `executionId` is a string this lane treats as an opaque label. Help does not
 * parse it, derive meaning from it, or check it — it carries it so an accepted
 * answer can name which execution produced it. Giving it structure here would
 * be the first step toward re-deriving authority from it, which is the thing
 * this seam exists to prevent.
 */
import type { HelpAnswerRequest } from "./contract";

/** One completed execution, as the authority reports it. */
export type HelpAnswerExecution = {
  /**
   * Opaque identity of this execution, minted by the authority.
   *
   * Never parsed here. Carried so an accepted answer names its execution.
   */
  readonly executionId: string;
  /**
   * The provider's raw reply, entirely untrusted.
   *
   * Deliberately `unknown`: the authority is a transport for it, not a
   * validator of it. Validation is Help's job and happens after this returns.
   */
  readonly reply: unknown;
};

/**
 * Why an execution did not happen.
 *
 * A small closed set, chosen for what a Help surface can honestly say to a
 * user. It is not a policy taxonomy — the authority's own reasons are richer
 * and are not this lane's to enumerate or to render. In particular there is no
 * variant distinguishing "you lack the capability" from "that does not exist",
 * because the difference is itself an information leak.
 */
export type HelpAnswerRefusal =
  | "unauthorized"
  | "unavailable"
  | "cancelled"
  | "timeout"
  | "internal";

export type HelpAnswerAuthorityResult =
  | { readonly kind: "executed"; readonly execution: HelpAnswerExecution }
  | { readonly kind: "refused"; readonly reason: HelpAnswerRefusal };

/**
 * The one call Help makes across the seam.
 *
 * There is no second method. No `authorize()` that returns a decision Help
 * then applies — applying a decision in the renderer means the renderer can
 * decline to apply it. The authority executes or refuses, and Help validates
 * what comes back.
 */
export type HelpAnswerAuthority = {
  readonly execute: (
    request: HelpAnswerRequest,
    signal: AbortSignal,
  ) => Promise<HelpAnswerAuthorityResult>;
};

/**
 * Nothing is bound until a host binds it.
 *
 * Not a stub that succeeds, and not a throw. With no authority, Help answering
 * is simply unavailable and offline retrieval — which is the product, not a
 * degraded mode — carries on untouched.
 */
export const HELP_NO_AUTHORITY: HelpAnswerAuthority | null = null;
