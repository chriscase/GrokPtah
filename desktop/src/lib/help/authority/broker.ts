import {
  HELP_AUTHORITY_LIMITS,
  parseHelpAuthorityResponse,
  validateHelpAuthorityRequest,
  type HelpAuthorityRequest,
  type HelpAuthorityResponse,
} from "./contract";

export type HelpAuthorityBrokerOptions = {
  /** ContextDesk or another trusted broker origin; never a provider origin. */
  readonly baseUrl: string;
  readonly fetcher?: typeof fetch;
  readonly credentials?: RequestCredentials;
  /** Broker CSRF token; this is not a GrokPtah/provider bearer token. */
  readonly csrfToken?: string;
};

export class HelpAuthorityBrokerError extends Error {
  readonly status: number;
  readonly code: "invalid_request" | "unauthenticated" | "invalid_response" | "transport";

  constructor(
    status: number,
    code: HelpAuthorityBrokerError["code"],
    message: string,
  ) {
    super(message);
    this.name = "HelpAuthorityBrokerError";
    this.status = status;
    this.code = code;
  }
}

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

/**
 * Authenticated browser transport for the dedicated one-shot Help endpoint.
 *
 * It has no session/run/history/tool/workspace methods by construction. The
 * browser sends a strict source-bound request through cookies and CSRF only;
 * the trusted broker owns provider credentials and authority.
 */
export class HelpAuthorityBrokerClient {
  private readonly url: string;
  private readonly fetcher: typeof fetch;
  private readonly credentials: RequestCredentials;
  private readonly csrfToken: string | undefined;

  constructor(options: HelpAuthorityBrokerOptions) {
    const baseUrl = options.baseUrl.replace(/\/$/, "");
    this.url = baseUrl.endsWith("/api/grokptah/v1")
      ? `${baseUrl}/help/answer`
      : `${baseUrl}/api/grokptah/v1/help/answer`;
    this.fetcher = options.fetcher ?? globalThis.fetch.bind(globalThis);
    this.credentials = options.credentials ?? "include";
    const csrfToken = options.csrfToken?.trim();
    if (csrfToken && utf8Bytes(csrfToken) > 256) {
      throw new HelpAuthorityBrokerError(0, "invalid_request", "CSRF token is oversized");
    }
    this.csrfToken = csrfToken || undefined;
  }

  async answer(
    request: HelpAuthorityRequest,
    signal?: AbortSignal,
  ): Promise<HelpAuthorityResponse> {
    const requestValidation = validateHelpAuthorityRequest(request);
    if (!requestValidation.accepted) {
      throw new HelpAuthorityBrokerError(
        0,
        "invalid_request",
        `${requestValidation.reason}: ${requestValidation.detail}`,
      );
    }
    if (!this.csrfToken) {
      throw new HelpAuthorityBrokerError(
        0,
        "unauthenticated",
        "broker CSRF token is required for Help execution",
      );
    }
    const response = await this.fetcher(this.url, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": request.requestId,
        "X-CSRF-Token": this.csrfToken,
      },
      credentials: this.credentials,
      body: JSON.stringify(request),
      signal,
    }).catch((error: unknown) => {
      if (signal?.aborted) throw error;
      throw new HelpAuthorityBrokerError(
        0,
        "transport",
        error instanceof Error ? error.name : "unknown transport error",
      );
    });
    if (!response.ok) {
      throw new HelpAuthorityBrokerError(
        response.status,
        response.status === 401 ? "unauthenticated" : "transport",
        `Help broker request failed with HTTP ${response.status}`,
      );
    }
    const text = await response.text();
    if (utf8Bytes(text) > HELP_AUTHORITY_LIMITS.maxResponseBytes) {
      throw new HelpAuthorityBrokerError(0, "invalid_response", "Help broker response is oversized");
    }
    let raw: unknown;
    try {
      raw = JSON.parse(text);
    } catch {
      throw new HelpAuthorityBrokerError(0, "invalid_response", "Help broker response is not JSON");
    }
    const parsed = parseHelpAuthorityResponse(raw, request);
    if (!parsed) {
      throw new HelpAuthorityBrokerError(
        0,
        "invalid_response",
        "Help broker response failed strict validation",
      );
    }
    return parsed;
  }
}
