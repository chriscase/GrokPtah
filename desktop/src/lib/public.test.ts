import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  GrokPtahBrokerClient,
  HELP_CONTRACT,
  HELP_ANSWER_CONTRACT,
  HELP_AUTHORITY_ARTICLES,
  HELP_AUTHORITY_CONTRACT,
  HELP_AUTHORITY_DIGEST,
  HELP_AUTHORITY_MANIFEST,
  EXTERNAL_WORKER_CONTRACT,
  buildHelpAnswerRequest,
  checkHelpLink,
  createExternalWorkerMonitor,
  createHelpAuthority,
  parseCapabilitySet,
  promptQueueReducer,
  searchHelpAuthority,
} from "./public";

const libRoot = dirname(fileURLToPath(import.meta.url));

describe("public integration barrel", () => {
  it("exposes only transport-neutral consumer surfaces", () => {
    expect(typeof GrokPtahBrokerClient).toBe("function");
    expect(HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(typeof parseCapabilitySet).toBe("function");
    expect(typeof promptQueueReducer).toBe("function");
    expect(EXTERNAL_WORKER_CONTRACT).toBe("grokptah.external-workers.v1");
    expect(createExternalWorkerMonitor().lastSeq).toBe(-1);
  });

  it("publishes the canonical Help authority and its answer seam", () => {
    expect(HELP_AUTHORITY_CONTRACT).toBe("grokptah.help-authority.v1");
    expect(HELP_ANSWER_CONTRACT).toBe("grokptah.help-answer.v1");
    expect(typeof createHelpAuthority).toBe("function");
    expect(typeof searchHelpAuthority).toBe("function");
    expect(typeof buildHelpAnswerRequest).toBe("function");
    expect(HELP_AUTHORITY_MANIFEST.digest).toBe(HELP_AUTHORITY_DIGEST);
    expect(Object.isFrozen(HELP_AUTHORITY_ARTICLES)).toBe(true);
    expect(createHelpAuthority().verify().ok).toBe(true);
    expect(checkHelpLink("javascript:alert(1)").safe).toBe(false);
  });

  it("keeps the Help authority free of Tauri, transport, and bearer material", () => {
    // The published Help surface must not reach a native binding, a network
    // client, or a credential. This is asserted on the source of the modules
    // themselves; `npm run verify:public` re-asserts it on the built bundle.
    const forbidden = [
      "@tauri-apps", "invoke(", "fetch(", "XMLHttpRequest", "WebSocket",
      "Authorization", "Bearer", "apiKey", "XAI_API_KEY", "GROKPTAH_HOME",
      "localStorage", "sessionStorage", "process.env", "node:fs", "node:child_process",
    ];
    for (const file of ["helpAuthority.ts", "helpAnswer.ts"]) {
      const source = readFileSync(resolve(libRoot, file), "utf8");
      for (const needle of forbidden) {
        expect(source.includes(needle), `${file} must not reference ${needle}`).toBe(false);
      }
    }
  });

  it("keeps privileged operations out of Help retrieval results", () => {
    const result = searchHelpAuthority("promote a review through the gateway", {
      includeRestricted: true,
    });
    const serialized = JSON.stringify(result);
    // Credential-shaped material and host paths, not the words themselves:
    // Help content legitimately *warns* about bearer tokens and describes
    // token scope, and that guidance is exactly what it should carry.
    const credentialShaped: Array<[string, RegExp]> = [
      ["a bearer value", /Bearer\s+[A-Za-z0-9._-]{8,}/],
      ["an authorization header", /Authorization\s*:/i],
      ["an api key field", /\bapi[_-]?key\b/i],
      ["a provider key", /\bsk-[A-Za-z0-9]{8,}/],
      ["an environment secret", /XAI_API_KEY|GROKPTAH_HOME/],
      ["a host path", /\/Users\/|\/home\/[a-z]|\/private\//],
    ];
    for (const [label, pattern] of credentialShaped) {
      expect(pattern.test(serialized), `result must not carry ${label}`).toBe(false);
    }
    // Help names documented capabilities but never their live availability.
    for (const hit of result.hits) {
      expect(hit.article).not.toHaveProperty("available");
      expect(hit.article).not.toHaveProperty("granted");
      expect(hit.article).not.toHaveProperty("approval");
    }
  });
});
