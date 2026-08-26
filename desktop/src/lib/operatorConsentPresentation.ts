/**
 * Operator-consent presentation helpers.
 *
 * Display-only. Every request/error field is untrusted input. Closed labels and
 * bounded values never become host authority, standing grants, or a backend
 * outcome. Client redaction is defense-in-depth.
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
  unsafeRetry:
    "Do not retry from an indeterminate state. A later retry is only safe after a new host-authored prompt and a confirmed stopped run.",
} as const;

export const STANDING_GRANT_FACTS = {
  offered: false,
  scope: "unavailable",
  lifetime: "unavailable",
  revision: "unavailable",
} as const;

const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/g;
const PRIVILEGED_TEXT =
  /(?:\/(?:users|private|var|tmp|home|volumes)\/|(?:[a-z]:\\users\\|\\\\)|https?:\/\/|(?:^|[\s=:])(authorization|bearer|api[_ -]?key|xai_api_key|grokptah_home|clipboard|private[_ -]?key|password|cookie|session[_ -]?token|secret(?:[_ -]?key)?)(?:[\s=:]|$))/i;
const PATH_OR_COMMAND =
  /(?:\.\.|~\/|\$[A-Z_]+|[;&|`$<>]|-rf\b|\bsudo\b|\bcurl\b|\bwget\b|\bchmod\b)/i;
const IDENTIFIER =
  /\b(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[a-z]{0,12}[-_][a-z0-9][-_a-z0-9]{6,}|sess(?:ion)?[-_][a-z0-9-]+|req[-_][a-z0-9-]+)\b/i;
const RAW_TOOL_TOKEN =
  /\b(?:run_terminal_cmd|write_files?|read_file|grep_search|glob_search|[a-z]+_[a-z0-9_]+)\b/;

const CLOSED_TOOL_LABELS: Record<string, string> = {
  run_terminal_cmd: "Terminal command",
  write_file: "Write a file",
  write_files: "Write files",
  read_file: "Read a file",
  grep_search: "Search files",
  glob_search: "Find files",
};

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], summary, [tabindex]:not([tabindex="-1"])';

const DEFAULT_ACK_TIMEOUT_MS = 8_000;
const DISPLAY_BOUND = 96;

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

export function permissionQueueAfterAcknowledgement(
  queue: PermissionRequest[],
  requestId: string,
  ack: ConsentAcknowledgement,
): PermissionRequest[] {
  if (ack !== "acknowledged") return queue;
  return dequeuePermission(queue, requestId);
}

export async function settleOperatorConsentAcknowledgement(
  send: () => Promise<unknown>,
  timeoutMs: number = DEFAULT_ACK_TIMEOUT_MS,
): Promise<ConsentAcknowledgement> {
  let timedOut = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const sent = Promise.resolve()
      .then(send)
      .then((value): ConsentAcknowledgement => {
        if (value === "rejected" || value === "lost") return value;
        return "acknowledged";
      });
    const timeout = new Promise<ConsentAcknowledgement>((resolve) => {
      timer = setTimeout(() => {
        timedOut = true;
        resolve("lost");
      }, timeoutMs);
    });
    const winner = await Promise.race([sent, timeout]);
    if (timedOut) return "lost";
    return winner;
  } catch {
    return timedOut ? "lost" : "rejected";
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export function closedToolLabel(toolName: unknown): string {
  if (typeof toolName !== "string") return CONSENT_COPY.toolUnknown;
  const mapped = CLOSED_TOOL_LABELS[toolName];
  if (mapped) return mapped;
  return CONSENT_COPY.toolUnknown;
}

export function closedRiskLabel(tier: unknown): string {
  if (tier === "deny") return CONSENT_COPY.riskDeny;
  if (tier === "ask") return CONSENT_COPY.riskAsk;
  return CONSENT_COPY.riskUnknown;
}

export function redactUntrustedDisplay(value: unknown, bound = DISPLAY_BOUND): string {
  if (typeof value !== "string") return "";
  const stripped = value.replace(CONTROL_CHARS, "").trim();
  if (!stripped) return "";
  if (
    PRIVILEGED_TEXT.test(stripped) ||
    PATH_OR_COMMAND.test(stripped) ||
    IDENTIFIER.test(stripped) ||
    RAW_TOOL_TOKEN.test(stripped)
  ) {
    return "";
  }
  return stripped.length > bound ? `${stripped.slice(0, bound)}…` : stripped;
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

function detailRecord(detail: unknown): Record<string, unknown> {
  return typeof detail === "object" && detail !== null && !Array.isArray(detail)
    ? (detail as Record<string, unknown>)
    : {};
}

export function presentOperatorConsent(input: {
  request: PermissionRequest;
  queuedBehind?: number;
  denyHistory?: DenyHistoryEntry[];
  phase: ConsentPhase;
  fallbackSessionId?: string | null;
}): OperatorConsentPresentation {
  const queuedBehind = input.queuedBehind ?? 0;
  const detail = detailRecord(input.request.detail);
  const toolLabel = closedToolLabel(input.request.tool_name);
  const riskLabel = closedRiskLabel(detail.risk_tier);
  const boundedRisk = redactUntrustedDisplay(detail.risk);
  const boundedSummary = redactUntrustedDisplay(input.request.summary);
  const sessionFact = input.request.session_id
    ? CONSENT_COPY.sessionKnown
    : input.fallbackSessionId
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
  const liveStatus =
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
    riskNote: boundedRisk || "No host risk prose was provided.",
    summary:
      boundedSummary ||
      `${toolLabel} is waiting. Untrusted summary text is hidden.`,
    sessionFact,
    queueCopy,
    standingGrant,
    details,
    denyHistory: (input.denyHistory ?? []).slice(0, 8).map((entry) => ({
      toolLabel: closedToolLabel(entry.tool_name),
      riskLabel: closedRiskLabel(entry.risk_tier),
      summary:
        redactUntrustedDisplay(entry.summary) ||
        "A previous denial is on record. Untrusted detail is hidden.",
    })),
    liveAlert: [
      CONSENT_COPY.blockedAlert,
      `Tool class: ${toolLabel}.`,
      `Risk class: ${riskLabel}.`,
      queuedBehind > 0 ? CONSENT_COPY.queued : "",
    ]
      .filter(Boolean)
      .join(" "),
    liveStatus,
    recovery,
    nextAction:
      input.phase === "idle"
        ? CONSENT_COPY.idleNext
        : input.phase === "pending"
          ? CONSENT_COPY.pending
          : CONSENT_COPY.unconfirmed,
    offerStandingGrant: false,
  };
}

export function presentDeniedPermissionRecord(
  request: PermissionRequest,
  sessionId: string,
): Omit<DenyHistoryEntry, "at"> {
  const detail = detailRecord(request.detail);
  return {
    tool_name:
      typeof request.tool_name === "string" && CLOSED_TOOL_LABELS[request.tool_name]
        ? request.tool_name
        : "unknown_tool",
    summary: redactUntrustedDisplay(request.summary) || closedToolLabel(request.tool_name),
    session_id: sessionId ? "owning-session" : "",
    risk: redactUntrustedDisplay(detail.risk) || undefined,
    risk_tier:
      detail.risk_tier === "deny" || detail.risk_tier === "ask"
        ? detail.risk_tier
        : undefined,
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

export function inertNonConsentSiblings(layer: HTMLElement | null): () => void {
  const shell = layer?.parentElement;
  if (!layer || !shell) return () => {};
  const siblings = Array.from(shell.children).filter(
    (child): child is HTMLElement =>
      child !== layer &&
      child instanceof HTMLElement &&
      child.dataset.modalLayer !== "consent",
  );
  const previous = siblings.map((element) => ({
    element,
    ariaHidden: element.getAttribute("aria-hidden"),
    inert: element.hasAttribute("inert"),
  }));
  siblings.forEach((element) => {
    element.setAttribute("inert", "");
    element.setAttribute("aria-hidden", "true");
  });
  return () => {
    previous.forEach(({ element, ariaHidden, inert }) => {
      if (inert) element.setAttribute("inert", "");
      else element.removeAttribute("inert");
      if (ariaHidden === null) element.removeAttribute("aria-hidden");
      else element.setAttribute("aria-hidden", ariaHidden);
    });
  };
}

export function presentationContainsForbiddenRaw(
  text: string,
  needles: readonly string[],
): string[] {
  return needles.filter((needle) => needle && text.includes(needle));
}
