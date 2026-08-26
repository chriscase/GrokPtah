/**
 * Operator-consent presentation helpers.
 *
 * Display-only. This module never becomes host authority. At this head the
 * renderer projects a closed set of own-key labels and fixed copy. Untrusted
 * request, detail, and deny-history strings are not rendered.
 */
import type { DenyHistoryEntry } from "./denyHistory";
import { dequeuePermission } from "./permissionQueue";
import type { PermissionRequest } from "./protocol";

export type ConsentPhase = "idle" | "pending" | "unconfirmed";
export type ConsentAcknowledgement = "acknowledged" | "rejected" | "lost";
export type ConsentDecision = "allow" | "deny";

export const CONSENT_COPY = {
  title: "Needs your response",
  safestAction: "Deny",
  allowOnce: "Allow once",
  blockedAlert:
    "A tool is blocked until you answer. Deny is focused. Do not assume a result yet.",
  idleNext:
    "Choose Deny or Allow once. Deny is the safest action. This answer applies only to the request on this screen.",
  pending:
    "Your response was sent. Wait for the host to acknowledge it. Do not assume success, denial, or a safe retry. Escape does nothing now.",
  unconfirmed:
    "Response unconfirmed. The host did not acknowledge this answer. Do not retry, translate this into Deny, advance the queue, or treat any tool outcome as known.",
  queued:
    "More requests are waiting. They stay blocked. Answering this request does not approve the ones behind it.",
  standingGrant:
    "Always Allow is unavailable. Host-authored scope, lifetime, and revision are not present at this head. Do not invent a standing grant.",
  hiddenPayload: "Untrusted technical payload is hidden.",
  sessionKnown: "Owning session is known to the host.",
  sessionMissing: "Owning session was not provided.",
  toolUnknown: "Unknown tool",
  riskUnknown: "Unknown risk class",
  riskAsk: "Ask first",
  riskDeny: "Must deny",
  riskNote: "Tool safety gate only. Untrusted risk prose is hidden.",
  waiting: "A mapped tool class is waiting.",
  priorDenial: "A previous denial is on record.",
  unsafeRetry:
    "Do not retry from an indeterminate state. A later retry is only safe after a new host-authored prompt and a confirmed stopped run.",
} as const;

export const STANDING_GRANT_FACTS = {
  offered: false,
  scope: "unavailable",
  lifetime: "unavailable",
  revision: "unavailable",
} as const;

const CLOSED_TOOL_LABELS: Record<string, string> = Object.assign(
  Object.create(null) as Record<string, string>,
  {
    run_terminal_cmd: "Terminal command",
    write_file: "Write a file",
    write_files: "Write files",
    read_file: "Read a file",
    grep_search: "Search files",
    glob_search: "Find files",
  },
);

const CLOSED_RISK_LABELS: Record<string, string> = Object.assign(
  Object.create(null) as Record<string, string>,
  {
    deny: CONSENT_COPY.riskDeny,
    ask: CONSENT_COPY.riskAsk,
  },
);

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], summary, [tabindex]:not([tabindex="-1"])';

export function standingGrantFactsAtThisHead(): typeof STANDING_GRANT_FACTS & {
  explanation: string;
} {
  return {
    ...STANDING_GRANT_FACTS,
    explanation: CONSENT_COPY.standingGrant,
  };
}

export function consentBlocksWorkspaceShortcuts(consentOpen: boolean): boolean {
  return Boolean(consentOpen);
}

export function canSubmitConsent(phase: ConsentPhase): boolean {
  return phase === "idle";
}

export function applyConsentEscape(phase: ConsentPhase): "deny" | "suppress" {
  return phase === "idle" ? "deny" : "suppress";
}

export function phaseAfterAcknowledgement(
  ack: ConsentAcknowledgement,
): ConsentPhase {
  return ack === "acknowledged" ? "idle" : "unconfirmed";
}

export type ConsentLockState = {
  requestId: string;
  phase: ConsentPhase;
};

export type ConsentLockAction =
  | { type: "bind"; requestId: string }
  | { type: "submit"; requestId: string }
  | {
      type: "acknowledge";
      requestId: string;
      ack: ConsentAcknowledgement | null;
    };

/**
 * Request-keyed consent lock. Identity changes bind to idle. Submit and
 * acknowledgement for a different id are ignored so a stale update cannot
 * unlock the current head.
 */
export function reduceConsentLock(
  state: ConsentLockState,
  action: ConsentLockAction,
): ConsentLockState {
  switch (action.type) {
    case "bind":
      if (state.requestId === action.requestId) return state;
      return { requestId: action.requestId, phase: "idle" };
    case "submit":
      if (state.requestId !== action.requestId) {
        return { requestId: action.requestId, phase: "pending" };
      }
      if (!canSubmitConsent(state.phase)) return state;
      return { requestId: action.requestId, phase: "pending" };
    case "acknowledge":
      if (state.requestId !== action.requestId) return state;
      return {
        requestId: action.requestId,
        phase: phaseAfterAcknowledgement(action.ack ?? "lost"),
      };
    default:
      return state;
  }
}

/** Host-authored owner only. Empty, missing, or whitespace is unknown. */
export function owningSessionId(
  request: Pick<PermissionRequest, "session_id"> | null | undefined,
): string | null {
  if (!request || typeof request !== "object") return null;
  const raw = (request as { session_id?: unknown }).session_id;
  if (typeof raw !== "string" || raw.trim().length === 0) return null;
  return raw;
}

export function consentPhaseForRequest(
  state: ConsentLockState,
  requestId: string,
  gateRequestId: string | null,
): ConsentPhase {
  if (state.requestId === requestId) return state.phase;
  if (gateRequestId === requestId) return "pending";
  return "idle";
}

export function permissionQueueAfterAcknowledgement(
  queue: PermissionRequest[],
  requestId: string,
  ack: ConsentAcknowledgement,
): PermissionRequest[] {
  if (ack !== "acknowledged") return queue;
  return dequeuePermission(queue, requestId);
}

/** Closed ack tokens only. Void, objects, and arbitrary strings are not acknowledgements. */
export function readConsentAcknowledgement(
  value: unknown,
): ConsentAcknowledgement | null {
  if (value === "acknowledged" || value === "rejected" || value === "lost") {
    return value;
  }
  return null;
}

function ownMappedLabel(
  table: Record<string, string>,
  key: unknown,
  fallback: string,
): string {
  if (typeof key !== "string") return fallback;
  if (!Object.prototype.hasOwnProperty.call(table, key)) return fallback;
  const mapped = table[key];
  return typeof mapped === "string" && mapped.length > 0 ? mapped : fallback;
}

export function closedToolLabel(toolName: unknown): string {
  return ownMappedLabel(CLOSED_TOOL_LABELS, toolName, CONSENT_COPY.toolUnknown);
}

export function closedRiskLabel(tier: unknown): string {
  return ownMappedLabel(CLOSED_RISK_LABELS, tier, CONSENT_COPY.riskUnknown);
}

function ownToolKey(toolName: unknown): string {
  if (typeof toolName !== "string") return "unknown_tool";
  if (!Object.prototype.hasOwnProperty.call(CLOSED_TOOL_LABELS, toolName)) {
    return "unknown_tool";
  }
  return toolName;
}

function ownRiskTier(tier: unknown): "deny" | "ask" | undefined {
  if (tier === "deny" || tier === "ask") return tier;
  return undefined;
}

export type PresentedDenyHistoryItem = {
  toolLabel: string;
  riskLabel: string;
  summary: string;
};

export type OperatorConsentPresentation = {
  title: string;
  toolLabel: string;
  riskLabel: string;
  riskNote: string;
  summary: string;
  sessionFact: string;
  queueCopy: string | null;
  standingGrant: ReturnType<typeof standingGrantFactsAtThisHead>;
  details: string[];
  denyHistory: PresentedDenyHistoryItem[];
  liveAlert: string;
  liveStatus: string;
  recovery: string;
  nextAction: string;
  offerStandingGrant: false;
};

export function presentOperatorConsent(input: {
  request: PermissionRequest;
  queuedBehind?: number;
  denyHistory?: DenyHistoryEntry[];
  phase: ConsentPhase;
  fallbackSessionId?: string | null;
}): OperatorConsentPresentation {
  const queuedBehind = input.queuedBehind ?? 0;
  const detail =
    typeof input.request.detail === "object" &&
    input.request.detail !== null &&
    !Array.isArray(input.request.detail)
      ? (input.request.detail as Record<string, unknown>)
      : {};
  const toolLabel = closedToolLabel(input.request.tool_name);
  const riskLabel = closedRiskLabel(detail.risk_tier);
  const sessionFact = owningSessionId(input.request)
    ? CONSENT_COPY.sessionKnown
    : CONSENT_COPY.sessionMissing;
  const standingGrant = standingGrantFactsAtThisHead();
  const queueCopy = queuedBehind > 0 ? CONSENT_COPY.queued : null;
  const recovery =
    input.phase === "pending"
      ? CONSENT_COPY.pending
      : input.phase === "unconfirmed"
        ? CONSENT_COPY.unconfirmed
        : CONSENT_COPY.idleNext;
  const details = [
    `Tool class: ${toolLabel}`,
    `Risk class: ${riskLabel}`,
    `Scope: ${standingGrant.scope}`,
    `Lifetime: ${standingGrant.lifetime}`,
    `Revision: ${standingGrant.revision}`,
    CONSENT_COPY.hiddenPayload,
    CONSENT_COPY.unsafeRetry,
  ];
  return {
    title: CONSENT_COPY.title,
    toolLabel,
    riskLabel,
    riskNote: CONSENT_COPY.riskNote,
    summary: CONSENT_COPY.waiting,
    sessionFact,
    queueCopy,
    standingGrant,
    details,
    denyHistory: (input.denyHistory ?? []).slice(0, 8).map((entry) => ({
      toolLabel: closedToolLabel(entry.tool_name),
      riskLabel: closedRiskLabel(entry.risk_tier),
      summary: CONSENT_COPY.priorDenial,
    })),
    liveAlert: [
      CONSENT_COPY.blockedAlert,
      `Tool class: ${toolLabel}.`,
      `Risk class: ${riskLabel}.`,
      queuedBehind > 0 ? CONSENT_COPY.queued : "",
    ]
      .filter(Boolean)
      .join(" "),
    liveStatus: recovery,
    recovery,
    nextAction: recovery,
    offerStandingGrant: false,
  };
}

export function presentDeniedPermissionRecord(
  request: PermissionRequest,
  claimedSessionId?: string | null,
): Omit<DenyHistoryEntry, "at"> | null {
  const owner = owningSessionId(request);
  if (!owner) return null;
  if (
    typeof claimedSessionId === "string" &&
    claimedSessionId.length > 0 &&
    claimedSessionId !== owner
  ) {
    return null;
  }
  const detail =
    typeof request.detail === "object" &&
    request.detail !== null &&
    !Array.isArray(request.detail)
      ? (request.detail as Record<string, unknown>)
      : {};
  return {
    tool_name: ownToolKey(request.tool_name),
    summary: closedToolLabel(request.tool_name),
    session_id: owner,
    risk_tier: ownRiskTier(
      typeof detail.risk_tier === "string" ? detail.risk_tier : undefined,
    ),
  };
}

export function focusableConsentControls(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) =>
      !element.closest("[inert]") &&
      element.getAttribute("aria-hidden") !== "true" &&
      !element.hasAttribute("disabled") &&
      element.tabIndex !== -1,
  );
}

export function trapConsentTabKey(
  event: { key: string; shiftKey: boolean; preventDefault: () => void },
  root: HTMLElement | null,
): void {
  if (event.key !== "Tab" || !root) return;
  const nodes = focusableConsentControls(root);
  if (nodes.length === 0) return;
  const first = nodes[0];
  const last = nodes[nodes.length - 1];
  const active = document.activeElement;
  const outside = !(active instanceof Node) || !root.contains(active);
  if (event.shiftKey) {
    if (outside || active === first) {
      event.preventDefault();
      last.focus();
    }
    return;
  }
  if (outside || active === last) {
    event.preventDefault();
    first.focus();
  }
}

type InertSnapshot = {
  element: Element;
  ariaHidden: string | null;
  inert: boolean;
};

function parentElementOf(node: Element): Element | null {
  const parent = node.parentElement;
  if (parent) return parent;
  const raw = node.parentNode;
  return raw instanceof Element ? raw : null;
}

function mountedConsentPath(layer: HTMLElement): Set<Element> {
  const path = new Set<Element>([layer]);
  let node: Element | null = parentElementOf(layer);
  while (node && node !== document.documentElement) {
    path.add(node);
    if (node === document.body) break;
    node = parentElementOf(node);
  }
  return path;
}

/**
 * Every sibling off the exact mounted consent layer's ancestor path.
 * A `data-modal-layer="consent"` marker on any other node is not trusted.
 */
function ancestorSiblingTargets(layer: HTMLElement): Element[] {
  const protectedPath = mountedConsentPath(layer);
  const targets: Element[] = [];
  let node: Element = layer;
  while (node && node !== document.documentElement) {
    const parent = parentElementOf(node);
    if (!parent) break;
    for (const child of Array.from(parent.children)) {
      if (!(child instanceof Element)) continue;
      if (child === node) continue;
      if (protectedPath.has(child)) continue;
      if (child === layer) continue;
      if (child.tagName === "HEAD") continue;
      targets.push(child);
    }
    if (parent === document.body) break;
    node = parent;
  }
  return targets;
}

function applyInertSnapshot(
  seen: Map<Element, InertSnapshot>,
  element: Element,
): void {
  if (seen.has(element)) return;
  seen.set(element, {
    element,
    ariaHidden: element.getAttribute("aria-hidden"),
    inert: element.hasAttribute("inert"),
  });
  element.setAttribute("inert", "");
  element.setAttribute("aria-hidden", "true");
}

function restoreInertSnapshot(snapshot: InertSnapshot): void {
  const { element, ariaHidden, inert } = snapshot;
  if (inert) element.setAttribute("inert", "");
  else element.removeAttribute("inert");
  if (ariaHidden === null) element.removeAttribute("aria-hidden");
  else element.setAttribute("aria-hidden", ariaHidden);
}

/**
 * Inert every non-path ancestor sibling, including late HTML portals,
 * mislabeled consent markers, and SVG/foreign-element siblings. Restores
 * each element's exact prior inert/aria-hidden.
 */
export function observeNonConsentInert(layer: HTMLElement | null): () => void {
  if (!layer || !layer.isConnected) return () => {};
  const seen = new Map<Element, InertSnapshot>();
  const scan = () => {
    for (const target of ancestorSiblingTargets(layer)) {
      applyInertSnapshot(seen, target);
    }
  };
  scan();
  const root = document.body ?? layer.ownerDocument?.body;
  if (!root || typeof MutationObserver !== "function") {
    return () => {
      Array.from(seen.values()).forEach(restoreInertSnapshot);
      seen.clear();
    };
  }
  const observer = new MutationObserver(scan);
  observer.observe(root, { childList: true, subtree: true });
  return () => {
    observer.disconnect();
    Array.from(seen.values()).forEach(restoreInertSnapshot);
    seen.clear();
  };
}

export function presentationContainsForbiddenRaw(
  text: string,
  needles: readonly string[],
): string[] {
  return needles.filter((needle) => needle && text.includes(needle));
}
