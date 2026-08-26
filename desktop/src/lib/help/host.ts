/**
 * The renderer's entire reach into Help authority.
 *
 * Four calls, and every one of them sends only an opaque session handle, an
 * opaque ask handle, a question, and a locale. There is no function here that
 * constructs a grant, an admission, a manifest, a principal, a capability, a
 * route, or a transport, because the host exposes no command that would accept
 * one — the corresponding Rust types are `Serialize`-only, so they cannot
 * arrive over IPC at all.
 *
 * The types below are imported from the generated contract rather than
 * declared here. A hand-written mirror is a second definition that drifts;
 * `help-codegen --verify` keeps the generated one honest.
 */

import { invoke } from "@tauri-apps/api/core";

import type {
  HelpAsk,
  HelpBoundsProjection,
  HelpCancelRequest,
  HelpCorpus,
  HelpFollow,
  HelpProjection,
} from "./generated/contract";

/**
 * Ask Help a question.
 *
 * Returns a projection carrying an opaque handle. Everything about *why* an
 * ask was refused stays in the host: the projection carries at most a coarse
 * code, because a caller that could tell "revoked" from "no such source" could
 * use the difference to map what exists.
 */
export function helpAsk(ask: HelpAsk): Promise<HelpProjection> {
  return invoke<HelpProjection>("help_ask", { ask });
}

/** Poll an in-flight ask. */
export function helpFollow(follow: HelpFollow): Promise<HelpProjection> {
  return invoke<HelpProjection>("help_follow", { follow });
}

/**
 * Ask the host to stop an in-flight ask.
 *
 * Resolving here means the host recorded the request, not that the provider
 * stopped. The host reports a run as cancelled only once it has observed the
 * provider quiesce; until then the ask is still draining, and a surface that
 * claimed otherwise would be reporting an outcome nobody has seen.
 */
export function helpCancel(cancel: HelpCancelRequest): Promise<HelpProjection> {
  return invoke<HelpProjection>("help_cancel", { cancel });
}

/** The executor's fixed bounds, so a surface can render honest limits. */
export function helpBounds(): Promise<HelpBoundsProjection> {
  return invoke<HelpBoundsProjection>("help_bounds");
}

/**
 * The corpus this principal may see, filtered by the host.
 *
 * The renderer does not filter anything. It receives what it is entitled to
 * and renders that; content above its ceiling never crosses the boundary, so
 * there is nothing for a modified renderer to reveal.
 */
export function helpVisibleCorpus(session: string): Promise<HelpCorpus> {
  return invoke<HelpCorpus>("help_visible_corpus", { session });
}

/**
 * Fetch the opaque session token for this window.
 *
 * The renderer does not construct this and cannot usefully alter it: it names
 * a row in the host's session table, and a token the host does not recognise
 * resolves to nothing — which is a denial, not a promotion.
 */
export function helpSession(): Promise<string> {
  return invoke<string>("help_session");
}
