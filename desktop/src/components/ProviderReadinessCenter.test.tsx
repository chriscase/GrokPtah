import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  ComputerUseTier,
  ProviderModelSummary,
  ProviderProfileSummary,
  ProviderQualificationReport,
  QualificationCheck,
} from "../lib/protocol";
import {
  computerUseTierLabel,
  deriveProviderReadiness,
  ProviderReadinessCenter,
  READINESS_VERDICT_LABEL,
  sanitizeReadinessText,
  type ProviderReadinessInput,
} from "./ProviderReadinessCenter";

const root = dirname(fileURLToPath(import.meta.url));

afterEach(cleanup);

const SECRET = "sk-secret-credential-value";
const KEY_PLACEHOLDER = "Saved (leave blank to keep)";

function check(
  status: QualificationCheck["status"],
  detail: string,
): QualificationCheck {
  return { status, detail };
}

function model(
  overrides: Partial<ProviderModelSummary> = {},
): ProviderModelSummary {
  return {
    id: "Team/Code:Cheap",
    displayName: "Team/Code:Cheap",
    supportsTools: false,
    supportsStream: false,
    supportsImageInput: false,
    computerUseTier: "none",
    computerCapabilitySource: "unknown",
    effortOptions: ["low"],
    capabilitySource: "unknown",
    ...overrides,
  };
}

function profile(
  overrides: Partial<ProviderProfileSummary> = {},
): ProviderProfileSummary {
  return {
    id: "company-gateway",
    label: "Company gateway",
    baseUrl: "https://gateway.example/v1",
    deadlineClass: "standard",
    credentialSet: true,
    managedByEnv: false,
    models: [model()],
    ...overrides,
  };
}

function report(
  overrides: Partial<ProviderQualificationReport> = {},
): ProviderQualificationReport {
  return {
    providerId: "company-gateway",
    modelId: "Team/Code:Cheap",
    basicGeneration: check("pass", "Basic generation passed"),
    nativeToolCall: check("fail", "No native tool call"),
    toolResultContinuation: check("fail", "Not attempted"),
    streaming: check("degraded", "JSON response"),
    codingReady: false,
    semanticObservation: check("fail", "Not attempted"),
    staleObservationRecovery: check("fail", "Not attempted"),
    computerUseTier: "none",
    ...overrides,
  };
}

function input(
  overrides: Partial<ProviderReadinessInput> = {},
): ProviderReadinessInput {
  const selected = profile();
  return {
    providerId: selected.id,
    providerLabel: selected.label,
    baseUrl: selected.baseUrl,
    modelId: selected.models[0].id,
    profile: selected,
    report: null,
    busy: false,
    error: null,
    saveDisabled: false,
    discoverDisabled: false,
    qualifyDisabled: false,
    ...overrides,
  };
}

const noop = {
  onSaveProvider: () => {},
  onDiscoverModels: () => {},
  onQualifyModel: () => {},
  onResolveCredentials: () => {},
};

function serialized(view: unknown): string {
  return JSON.stringify(view);
}

function expectSecretFree(text: string) {
  expect(text).not.toContain(SECRET);
  expect(text).not.toContain(KEY_PLACEHOLDER);
  expect(text).not.toContain("Provider key");
  expect(text).not.toContain("xai-…");
}

describe("sanitizeReadinessText", () => {
  it("redacts credential strings, headers, and key placeholders", () => {
    const raw = `${KEY_PLACEHOLDER} Provider key xai-abc123 Bearer tok authorization: secret api_key=${SECRET}`;
    const cleaned = sanitizeReadinessText(raw);
    expectSecretFree(cleaned);
    expect(cleaned).toContain("[redacted]");
  });
});

describe("deriveProviderReadiness", () => {
  it("blocks a measured model when the credential is missing", () => {
    const measured = model({
      capabilitySource: "measured",
      supportsTools: true,
      supportsStream: true,
      computerUseTier: "none",
      computerCapabilitySource: "measured",
    });
    const view = deriveProviderReadiness(
      input({
        profile: profile({ credentialSet: false, models: [measured] }),
      }),
    );
    expect(view.verdict).toBe("credential_missing");
    expect(view.verdictLabel).toBe(READINESS_VERDICT_LABEL.credential_missing);
    expect(view.announce).toBe("alert");
    expect(view.nextAction.id).toBe("resolve_credentials");
    expect(view.nextAction.label).toBe("Add provider key");
    expect(view.blockingReasons.map((item) => item.id)).toContain("credential");
    expect(view.summary).toMatch(/measured/i);
    expect(view.summary).not.toBe(READINESS_VERDICT_LABEL.ready);
    expectSecretFree(serialized(view));
  });

  it("never treats unknown or declared capabilities as coding-ready", () => {
    const unknown = deriveProviderReadiness(input());
    expect(unknown.verdict).toBe("not_qualified");
    expect(unknown.nextAction.id).toBe("qualify_model");

    const declaredModel = model({
      capabilitySource: "declared",
      supportsTools: true,
      supportsStream: true,
      computerUseTier: "semantic_act",
      computerCapabilitySource: "declared",
    });
    const declared = deriveProviderReadiness(
      input({ profile: profile({ models: [declaredModel] }) }),
    );
    expect(declared.verdict).toBe("not_qualified");
    expect(declared.verdictLabel).toBe(READINESS_VERDICT_LABEL.not_qualified);
    expect(declared.codingProvenance).toBe("declared");
    expect(declared.computerUse.source).toBe("declared");
    expect(declared.computerUse.summary).toMatch(/not a measured qualification/i);
    expect(declared.checks.find((item) => item.id === "native_tool_call")?.status).toBe(
      "declared",
    );
  });

  it("keeps native coding ready when the report says codingReady despite degraded streaming", () => {
    const view = deriveProviderReadiness(
      input({
        report: report({
          nativeToolCall: check("pass", "Native tool call passed"),
          toolResultContinuation: check("pass", "Continuation passed"),
          streaming: check("degraded", "JSON response"),
          codingReady: true,
          computerUseTier: "none",
        }),
      }),
    );
    expect(view.verdict).toBe("ready");
    expect(view.verdictLabel).toBe(READINESS_VERDICT_LABEL.ready);
    expect(view.codingReady).toBe(true);
    expect(view.streamingNote).toMatch(/unchanged/i);
    expect(view.streamingNote).toMatch(/JSON response/);
    expect(view.computerUse.tier).toBe("none");
    expect(view.computerUse.summary).toMatch(/independent/i);
  });

  it("uses discussion only with the exact tool or continuation failure", () => {
    const tools = deriveProviderReadiness(input({ report: report() }));
    expect(tools.verdict).toBe("discussion_only");
    expect(tools.blockingReasons.map((item) => item.label)).toEqual(
      expect.arrayContaining(["Native tool calls", "Tool-result continuation"]),
    );
    expect(tools.blockingReasons.some((item) => item.summary.includes("No native tool call"))).toBe(
      true,
    );

    const continuation = deriveProviderReadiness(
      input({
        report: report({
          nativeToolCall: check("pass", "Native tool call passed"),
          toolResultContinuation: check("fail", "Continuation rejected"),
          codingReady: false,
        }),
      }),
    );
    expect(continuation.verdict).toBe("discussion_only");
    expect(
      continuation.blockingReasons.some((item) =>
        item.summary.includes("Continuation rejected"),
      ),
    ).toBe(true);
    expect(continuation.blockingReasons.some((item) => item.id === "native_tool_call")).toBe(
      false,
    );
  });

  it.each([
    ["none", "None"],
    ["observe", "Observation only"],
    ["semantic_act", "Semantic act"],
    ["visual_fallback_act", "Visual fallback act"],
  ] as const)("shows Computer Use tier %s independently", (tier, label) => {
    expect(computerUseTierLabel(tier as ComputerUseTier)).toBe(label);
    const view = deriveProviderReadiness(
      input({
        report: report({
          nativeToolCall: check("pass", "Native tool call passed"),
          toolResultContinuation: check("pass", "Continuation passed"),
          streaming: check("pass", "Streaming passed"),
          codingReady: true,
          semanticObservation: check("pass", "Exact element selected"),
          staleObservationRecovery: check("pass", "Replacement frame used"),
          computerUseTier: tier,
        }),
      }),
    );
    expect(view.verdict).toBe("ready");
    expect(view.computerUse.tier).toBe(tier);
    expect(view.computerUse.label).toBe(label);
    expect(view.computerUse.summary).toMatch(/independent/i);
    expect(view.summary).not.toMatch(/coding tools and/i);
  });

  it("keeps environment-managed profiles read-only while still exposing evidence", () => {
    const measured = model({
      capabilitySource: "measured",
      supportsTools: true,
      computerCapabilitySource: "measured",
      computerUseTier: "observe",
    });
    const view = deriveProviderReadiness(
      input({
        profile: profile({
          managedByEnv: true,
          models: [measured],
        }),
        qualifyDisabled: true,
        saveDisabled: true,
        discoverDisabled: true,
      }),
    );
    expect(view.verdict).toBe("ready");
    expect(view.managedByEnv).toBe(true);
    expect(view.nextAction.id).toBe("none");
    expect(view.nextAction.description).toMatch(/environment/i);
    expect(view.computerUse.tier).toBe("observe");
  });

  it("recomputes from the selected model when no live report is supplied", () => {
    const chatOnly = model({
      id: "chat-only",
      displayName: "chat-only",
      capabilitySource: "measured",
      supportsTools: false,
    });
    const readyModel = model({
      id: "code-ready",
      displayName: "code-ready",
      capabilitySource: "measured",
      supportsTools: true,
    });
    const selected = profile({ models: [chatOnly, readyModel] });
    const stale = deriveProviderReadiness(
      input({
        profile: selected,
        modelId: chatOnly.id,
        report: report({ modelId: chatOnly.id, codingReady: false }),
      }),
    );
    const switched = deriveProviderReadiness(
      input({
        profile: selected,
        modelId: readyModel.id,
        report: null,
      }),
    );
    expect(stale.verdict).toBe("discussion_only");
    expect(switched.verdict).toBe("ready");
    expect(switched.hasLiveReport).toBe(false);
    expect(switched.modelId).toBe("code-ready");
  });

  it("does not invent codingReady without a report and keeps busy/error from props", () => {
    const view = deriveProviderReadiness(
      input({
        busy: true,
        error: `upstream 401 api_key=${SECRET} ${KEY_PLACEHOLDER}`,
      }),
    );
    expect(view.codingReady).toBeNull();
    expect(view.busy).toBe(true);
    expect(view.error).not.toContain(SECRET);
    expect(view.error).not.toContain(KEY_PLACEHOLDER);
    expect(view.nextAction.id).toBe("qualify_model");
    expect(view.nextAction.disabled).toBe(true);
  });

  it("offers a single qualify retry after an error without flipping a measured ready verdict", () => {
    const view = deriveProviderReadiness(
      input({
        report: report({
          nativeToolCall: check("pass", "Native tool call passed"),
          toolResultContinuation: check("pass", "Continuation passed"),
          streaming: check("pass", "Streaming passed"),
          codingReady: true,
        }),
        error: `401 api_key=${SECRET}`,
      }),
    );
    expect(view.verdict).toBe("ready");
    expect(view.nextAction.id).toBe("qualify_model");
    expect(view.nextAction.disabled).toBe(false);
    expectSecretFree(serialized(view));
  });

  it("treats an unsaved or incomplete form as configuration incomplete", () => {
    const missing = deriveProviderReadiness(
      input({
        providerId: "",
        baseUrl: "",
        modelId: "",
        profile: null,
        saveDisabled: true,
      }),
    );
    expect(missing.verdict).toBe("configuration_incomplete");
    expect(missing.nextAction.id).toBe("save_provider");
    expect(missing.nextAction.disabled).toBe(true);

    const unsaved = deriveProviderReadiness(
      input({
        profile: null,
        saveDisabled: false,
      }),
    );
    expect(unsaved.verdict).toBe("configuration_incomplete");
    expect(unsaved.nextAction.id).toBe("save_provider");
    expect(unsaved.nextAction.disabled).toBe(false);
  });
});

describe("ProviderReadinessCenter", () => {
  it("renders a concise operator verdict with power-user evidence on demand", async () => {
    const user = userEvent.setup();
    const onQualify = vi.fn();
    render(
      <ProviderReadinessCenter
        {...input({
          report: report({
            nativeToolCall: check("fail", `No native tool call ${SECRET}`),
          }),
        })}
        {...noop}
        onQualifyModel={onQualify}
      />,
    );

    const region = screen.getByRole("region", { name: /Native coding readiness/i });
    const verdict = screen.getByTestId("readiness-verdict");
    expect(within(verdict).getByRole("status")).toHaveTextContent(
      READINESS_VERDICT_LABEL.discussion_only,
    );
    expect(screen.getByTestId("readiness-blocking-reasons")).toHaveTextContent(
      "No native tool call",
    );
    expect(screen.getByTestId("readiness-computer-tier")).toHaveTextContent("None");
    expect(screen.getByTestId("readiness-verdict")).not.toHaveTextContent("codingReady");
    expect(screen.getByTestId("readiness-details")).not.toHaveAttribute("open");

    await user.click(screen.getByText("Power-user evidence"));
    expect(screen.getByTestId("readiness-details")).toHaveTextContent("codingReady");
    expect(screen.getByTestId("readiness-details")).toHaveTextContent("Team/Code:Cheap");
    expect(screen.getByTestId("readiness-next-action")).toHaveAccessibleName("Qualify model");
    expectSecretFree(region.textContent ?? "");
    fireEvent.click(screen.getByTestId("readiness-next-action"));
    expect(onQualify).toHaveBeenCalledOnce();
  });

  it("emphasizes credential repair and focuses through the provided callback", async () => {
    const user = userEvent.setup();
    const onResolve = vi.fn();
    const measured = model({
      capabilitySource: "measured",
      supportsTools: true,
    });
    render(
      <ProviderReadinessCenter
        {...input({
          profile: profile({ credentialSet: false, models: [measured] }),
        })}
        {...noop}
        onResolveCredentials={onResolve}
      />,
    );

    expect(screen.getByTestId("readiness-verdict")).toHaveTextContent(
      READINESS_VERDICT_LABEL.credential_missing,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      READINESS_VERDICT_LABEL.credential_missing,
    );
    const action = screen.getByTestId("readiness-next-action");
    expect(action).toHaveAccessibleName("Add provider key");
    await user.tab();
    expect(action).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(onResolve).toHaveBeenCalledOnce();
  });

  it("keeps busy retry from starting a second qualification path", () => {
    const onQualify = vi.fn();
    render(
      <ProviderReadinessCenter
        {...input({ busy: true, report: report() })}
        {...noop}
        onQualifyModel={onQualify}
      />,
    );
    expect(screen.getByTestId("readiness-next-action")).toBeDisabled();
    fireEvent.click(screen.getByTestId("readiness-next-action"));
    expect(onQualify).not.toHaveBeenCalled();
    expect(screen.getAllByRole("status").some((node) =>
      node.textContent?.includes("not started again automatically"),
    )).toBe(true);
  });

  it("preserves exact opaque model IDs in the operator surface", () => {
    const opaque = " Team/Code:Cheap ";
    render(
      <ProviderReadinessCenter
        {...input({ modelId: opaque, report: report({ modelId: opaque }) })}
        {...noop}
      />,
    );
    expect(screen.getByTestId("readiness-model-id")).toHaveTextContent(
      opaque,
      { normalizeWhitespace: false },
    );
  });
});

describe("ProviderReadinessCenter layout contract", () => {
  it("keeps component CSS wrapping on narrow widths without shared-file edits", () => {
    const css = readFileSync(join(root, "ProviderReadinessCenter.css"), "utf8");
    expect(css).toMatch(/min-width:\s*0/);
    expect(css).toMatch(/overflow-wrap:\s*anywhere/);
    expect(css).toMatch(/max-width:\s*100%/);
    expect(css).not.toMatch(/white-space:\s*nowrap/);
    expect(css).not.toMatch(/overflow-x:\s*auto/);
  });

  it("collapses its grids by container width, not viewport width", () => {
    // The card renders in a ~260px docked Settings pane at a 1440px window,
    // so a viewport media query can never see the crush (PR #360 audit P2).
    const css = readFileSync(join(root, "ProviderReadinessCenter.css"), "utf8");

    // The card itself is the size container…
    expect(css).toMatch(
      /\.provider-readiness\s*{[^}]*container-type:\s*inline-size/s,
    );

    // …single column is the base (and the no-container-support fallback)…
    const labelColumn = /grid-template-columns:\s*minmax\(0,\s*9\.5rem\)/g;
    expect(css.match(labelColumn)).toHaveLength(1);

    // …and the label column only ever appears behind a container gate.
    const gate = css.indexOf("@container (min-width: 30rem)");
    expect(gate).toBeGreaterThan(-1);
    expect(css.indexOf("minmax(0, 9.5rem)")).toBeGreaterThan(gate);

    // No viewport-keyed grid collapse may sneak back in.
    const mediaBlocks = css.split("@media").slice(1);
    for (const block of mediaBlocks) {
      expect(block).not.toContain("grid-template-columns");
    }
  });

  it("colors verdict text with tokens that hold 4.5:1 in the light theme", () => {
    const css = readFileSync(join(root, "ProviderReadinessCenter.css"), "utf8");
    // Attention verdict: --state-attention (identical to --accent in dark,
    // 4.5:1+ in light, unlike --accent's ~2.9:1).
    expect(css).toMatch(
      /is-attention \.provider-readiness-verdict\s*{[^}]*var\(--state-attention\)/s,
    );
    expect(css).not.toMatch(
      /is-attention \.provider-readiness-verdict\s*{[^}]*var\(--accent\)/s,
    );
    // Ok verdict keeps dark --ok but darkens for light via a text mix.
    expect(css).toMatch(
      /\[data-theme="light"\][^{]*is-ok \.provider-readiness-verdict\s*{[^}]*color-mix\(in srgb, var\(--ok\)/s,
    );
  });
});

describe("blocking-reason summary copy", () => {
  it("separates multiple blocking failures instead of running them together", () => {
    const view = deriveProviderReadiness(input({ report: report() }));
    expect(view.verdict).toBe("discussion_only");
    // Two failed checks must not fuse into one unpunctuated sentence.
    expect(view.summary).toMatch(
      /No native tool call; Tool-result continuation:/,
    );
  });
});
