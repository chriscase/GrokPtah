import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// vitest does not hand source modules a `file:` `import.meta.url`, so resolve
// repository paths from the vitest root (the `desktop/` project directory).
const repoPath = (relative: string) => resolve(process.cwd(), "..", relative);

import {
  ACCOUNT_READINESS_STATES,
  ACCOUNT_REFERENCE_SOURCES,
  CREDENTIAL_METHODS,
  EXPIRY_STATUSES,
  GROK_ACCOUNT_CONTRACT,
  GROK_ACCOUNT_SCHEMA_VERSION,
  MAX_ACCOUNT_REFERENCE_BYTES,
  READINESS_REASONS,
  absentGrokAccountFacts,
  canLaunchGrokBuild,
  credentialMethodLabel,
  formatSecondsRemaining,
  grokAccountNotice,
  parseGrokAccountFacts,
  parseRunAttribution,
  type GrokAccountFacts,
} from "./grokAccountFacts";

/** Fixed observation clock: 2026-08-25T00:00:00Z, matching the Rust tests. */
const VALID_EXPIRY = "2026-08-25T12:30:00Z";
const SECONDS_REMAINING = 45_000;
const SENTINEL_BEARER = "xai-SENTINEL-BEARER-DO-NOT-LEAK";

function facts(overrides: Partial<GrokAccountFacts> = {}): GrokAccountFacts {
  return {
    contract: GROK_ACCOUNT_CONTRACT,
    schemaVersion: GROK_ACCOUNT_SCHEMA_VERSION,
    credentialMethod: "grok_build_oidc",
    accountReference: { value: "usr-0a1b2c3d", source: "user_id" },
    expiry: { status: "valid", expiresAt: VALID_EXPIRY, secondsRemaining: SECONDS_REMAINING },
    readiness: "usable",
    readinessReason: "expiry_in_future",
    ...overrides,
  };
}

const EXPIRED = facts({
  expiry: { status: "expired", expiresAt: "2026-08-24T23:59:59Z", secondsRemaining: -1 },
  readiness: "unusable",
  readinessReason: "credential_expired",
});

const UNKNOWN_EXPIRY = facts({
  expiry: { status: "absent", expiresAt: null, secondsRemaining: null },
  readiness: "unknown",
  readinessReason: "expiry_not_provided",
});

describe("grok account facts parity with the v1 schema", () => {
  const schema = JSON.parse(
    readFileSync(repoPath("docs/schemas/grokptah-account.v1.schema.json"), "utf8"),
  );

  it("pins every closed vocabulary to the schema, in order", () => {
    expect(schema.$id).toBe("urn:grokptah:schema:account:v1");
    expect(schema.$defs.credentialMethod.enum).toEqual(CREDENTIAL_METHODS);
    expect(schema.$defs.accountReferenceSource.enum).toEqual(ACCOUNT_REFERENCE_SOURCES);
    expect(schema.$defs.expiryStatus.enum).toEqual(EXPIRY_STATUSES);
    expect(schema.$defs.accountReadiness.enum).toEqual(ACCOUNT_READINESS_STATES);
    expect(schema.$defs.readinessReason.enum).toEqual(READINESS_REASONS);
  });

  it("pins the account reference bound and charset to the schema", () => {
    const reference = schema.$defs.accountReference.properties.value;
    expect(reference.maxLength).toBe(MAX_ACCOUNT_REFERENCE_BYTES);
    expect(reference.pattern).toBe("^[A-Za-z0-9._:-]+$");
  });

  it("keeps the published facts object closed to unknown keys", () => {
    expect(schema.$defs.grokAccountFacts.additionalProperties).toBe(false);
    expect(schema.$defs.runAttribution.additionalProperties).toBe(false);
    expect(schema.$defs.accountReference.additionalProperties).toBe(false);
    expect(Object.keys(schema.$defs.grokAccountFacts.properties).sort()).toEqual(
      Object.keys(facts()).sort(),
    );
  });

  it("keeps run attribution optional in the run schema so v1 receipts still decode", () => {
    const runSchema = JSON.parse(
      readFileSync(repoPath("docs/schemas/grokptah-run.v1.schema.json"), "utf8"),
    );
    const durableRun = runSchema.$defs.durableRun;
    expect(durableRun.properties.attribution).toBeDefined();
    expect(durableRun.required).not.toContain("attribution");
    expect(runSchema.$defs.credentialMethod.enum).toEqual(CREDENTIAL_METHODS);
  });
});

describe("parseGrokAccountFacts", () => {
  it("accepts a well-formed usable projection", () => {
    expect(parseGrokAccountFacts(facts())).toEqual(facts());
  });

  it("accepts expired, unknown, and absent projections", () => {
    expect(parseGrokAccountFacts(EXPIRED)).toEqual(EXPIRED);
    expect(parseGrokAccountFacts(UNKNOWN_EXPIRY)).toEqual(UNKNOWN_EXPIRY);
    expect(parseGrokAccountFacts(absentGrokAccountFacts())).toEqual(absentGrokAccountFacts());
  });

  it("rejects unknown keys rather than passing them through", () => {
    expect(parseGrokAccountFacts({ ...facts(), bearer: SENTINEL_BEARER })).toBeNull();
    expect(parseGrokAccountFacts({ ...facts(), refreshToken: "r" })).toBeNull();
    expect(parseGrokAccountFacts({ ...facts(), authMode: "oidc" })).toBeNull();
    expect(
      parseGrokAccountFacts({
        ...facts(),
        expiry: { status: "valid", expiresAt: VALID_EXPIRY, secondsRemaining: 1, extra: 1 },
      }),
    ).toBeNull();
    expect(
      parseGrokAccountFacts({
        ...facts(),
        accountReference: { value: "usr-1", source: "user_id", fingerprint: "abc" },
      }),
    ).toBeNull();
  });

  it("rejects out-of-vocabulary values", () => {
    expect(parseGrokAccountFacts(facts({ credentialMethod: "oidc" as never }))).toBeNull();
    expect(parseGrokAccountFacts(facts({ readiness: "probably" as never }))).toBeNull();
    expect(parseGrokAccountFacts(facts({ readinessReason: "vibes" as never }))).toBeNull();
    expect(
      parseGrokAccountFacts(facts({ expiry: { status: "soon" as never, expiresAt: null } })),
    ).toBeNull();
  });

  it("rejects a wrong contract or schema version", () => {
    expect(parseGrokAccountFacts(facts({ contract: "grokptah.account.v2" as never }))).toBeNull();
    expect(parseGrokAccountFacts(facts({ schemaVersion: 2 as never }))).toBeNull();
  });

  it("rejects out-of-bounds and non-opaque account references", () => {
    for (const value of [
      "",
      "usr with space",
      "usr/../../etc/passwd",
      "<script>alert(1)</script>",
      "operator@example.test",
      "u".repeat(MAX_ACCOUNT_REFERENCE_BYTES + 1),
    ]) {
      expect(parseGrokAccountFacts(facts({ accountReference: { value, source: "user_id" } }))).toBeNull();
    }
    expect(
      parseGrokAccountFacts(facts({ accountReference: { value: "u".repeat(MAX_ACCOUNT_REFERENCE_BYTES), source: "user_id" } })),
    ).not.toBeNull();
  });

  it("rejects a timestamp that is not a normalized UTC instant", () => {
    for (const expiresAt of [
      "2026-08-25T12:30:00+02:00",
      "2026-08-25 12:30:00Z",
      "not-a-timestamp",
      "\"><script>alert(1)</script>",
    ]) {
      expect(
        parseGrokAccountFacts(facts({ expiry: { status: "valid", expiresAt, secondsRemaining: 1 } })),
      ).toBeNull();
    }
  });

  it("refuses to let absent or unparseable expiry smuggle a timestamp back in", () => {
    expect(
      parseGrokAccountFacts(
        facts({
          expiry: { status: "absent", expiresAt: VALID_EXPIRY, secondsRemaining: 1 },
          readiness: "unknown",
          readinessReason: "expiry_not_provided",
        }),
      ),
    ).toBeNull();
    expect(
      parseGrokAccountFacts(
        facts({
          expiry: { status: "unparseable", expiresAt: null, secondsRemaining: 5 },
          readiness: "unknown",
          readinessReason: "expiry_unparseable",
        }),
      ),
    ).toBeNull();
  });

  it("rejects a readiness verdict that does not follow from the evidence", () => {
    // A tampered producer must not talk the editor into launching an expired session.
    expect(
      parseGrokAccountFacts({ ...EXPIRED, readiness: "usable", readinessReason: "expiry_in_future" }),
    ).toBeNull();
    expect(
      parseGrokAccountFacts(facts({ readiness: "unusable", readinessReason: "credential_expired" })),
    ).toBeNull();
    expect(
      parseGrokAccountFacts({
        ...absentGrokAccountFacts(),
        readiness: "usable",
        readinessReason: "expiry_in_future",
      }),
    ).toBeNull();
  });

  it("maps an unrecognized route to unknown, never usable and never blocked", () => {
    const unrecognized = facts({
      credentialMethod: "unknown",
      readiness: "unknown",
      readinessReason: "method_unrecognized",
    });
    expect(parseGrokAccountFacts(unrecognized)).toEqual(unrecognized);
    expect(canLaunchGrokBuild(unrecognized)).toBe(true);
    // The same evidence claiming "usable" is refused.
    expect(parseGrokAccountFacts(facts({ credentialMethod: "unknown" }))).toBeNull();
  });

  it("rejects non-objects", () => {
    for (const value of [null, undefined, 42, "facts", [], true]) {
      expect(parseGrokAccountFacts(value)).toBeNull();
    }
  });
});

describe("parseRunAttribution", () => {
  it("accepts bounded attribution with and without an account reference", () => {
    expect(
      parseRunAttribution({
        credentialMethod: "grok_build_oidc",
        accountReference: { value: "usr-0a1b2c3d", source: "user_id" },
      }),
    ).toEqual({
      credentialMethod: "grok_build_oidc",
      accountReference: { value: "usr-0a1b2c3d", source: "user_id" },
    });
    expect(parseRunAttribution({ credentialMethod: "api_key" })).toEqual({
      credentialMethod: "api_key",
      accountReference: null,
    });
  });

  it("never accepts a balance, quota, or certification claim", () => {
    for (const extra of [
      { balance: 100 },
      { quota: "unlimited" },
      { credits: 5 },
      { certified: true },
      { entitlement: "pro" },
      { plan: "team" },
    ]) {
      expect(parseRunAttribution({ credentialMethod: "api_key", ...extra })).toBeNull();
    }
  });

  it("rejects credential-shaped keys outright", () => {
    for (const extra of [
      { bearer: SENTINEL_BEARER },
      { refreshToken: "r" },
      { apiKey: "k" },
      { credentialRef: "keychain:provider/xai/api-key" },
      { fingerprint: "deadbeef" },
    ]) {
      expect(parseRunAttribution({ credentialMethod: "api_key", ...extra })).toBeNull();
    }
  });
});

describe("negative serialization scan", () => {
  it("keeps every published projection free of credential needles", () => {
    for (const projection of [facts(), EXPIRED, UNKNOWN_EXPIRY, absentGrokAccountFacts()]) {
      const encoded = JSON.stringify(projection);
      for (const needle of [
        SENTINEL_BEARER,
        "refreshToken",
        "refresh_token",
        "bearer",
        "Bearer",
        "apiKey",
        "XAI_API_KEY",
        "credentialRef",
        "keychain:",
        "fingerprint",
        "authMode",
        "auth_mode",
        "@example.test",
        "/Users/",
        "/private/",
      ]) {
        expect(encoded).not.toContain(needle);
      }
    }
  });

  it("keeps every notice string free of credential needles and raw timestamps", () => {
    for (const projection of [facts(), EXPIRED, UNKNOWN_EXPIRY, absentGrokAccountFacts(), null]) {
      const notice = grokAccountNotice(projection);
      const text = `${notice.summary} ${notice.detail} ${notice.remedy ?? ""}`;
      for (const needle of [SENTINEL_BEARER, "bearer", "Bearer", "apiKey", "keychain:", "@example.test"]) {
        expect(text).not.toContain(needle);
      }
    }
  });
});

describe("launch gating", () => {
  it("blocks only on positive negative-evidence", () => {
    expect(canLaunchGrokBuild(facts())).toBe(true);
    expect(canLaunchGrokBuild(UNKNOWN_EXPIRY)).toBe(true);
    expect(canLaunchGrokBuild(EXPIRED)).toBe(false);
    expect(canLaunchGrokBuild(absentGrokAccountFacts())).toBe(false);
    // Facts this build cannot validate are not vouched for.
    expect(canLaunchGrokBuild(null)).toBe(false);
  });
});

describe("grokAccountNotice", () => {
  it("distinguishes expired (blocking) from unknown (non-blocking)", () => {
    const expired = grokAccountNotice(EXPIRED);
    expect(expired.tone).toBe("blocked");
    expect(expired.blocksLaunch).toBe(true);
    expect(expired.remedy).toBeTruthy();
    expect(expired.detail).toContain("Existing runs remain readable");

    const unknown = grokAccountNotice(UNKNOWN_EXPIRY);
    expect(unknown.tone).toBe("unknown");
    expect(unknown.blocksLaunch).toBe(false);
    expect(unknown.remedy).toBeNull();
    expect(unknown.detail).toContain("unknown, not expired");
  });

  it("offers a recovery path exactly when a launch is blocked", () => {
    for (const projection of [facts(), UNKNOWN_EXPIRY]) {
      const notice = grokAccountNotice(projection);
      expect(notice.blocksLaunch).toBe(false);
      expect(notice.remedy).toBeNull();
    }
    for (const projection of [EXPIRED, absentGrokAccountFacts(), null]) {
      const notice = grokAccountNotice(projection);
      expect(notice.blocksLaunch).toBe(true);
      expect(notice.remedy).toBeTruthy();
    }
  });

  it("never claims balance, quota, or certification", () => {
    for (const projection of [facts(), EXPIRED, UNKNOWN_EXPIRY, absentGrokAccountFacts(), null]) {
      const notice = grokAccountNotice(projection);
      const text = `${notice.summary} ${notice.detail} ${notice.remedy ?? ""}`.toLowerCase();
      for (const claim of ["balance", "quota", "credits", "entitl", "certified", "certification"]) {
        expect(text).not.toContain(claim);
      }
    }
  });

  it("says the ready state reflects local state only", () => {
    expect(grokAccountNotice(facts()).detail).toContain("local credential state only");
  });

  it("names the account without exposing personal data", () => {
    expect(grokAccountNotice(facts()).summary).toContain("usr-0a1b2c3d");
    expect(grokAccountNotice(facts({ accountReference: null })).summary).toContain(
      "Grok Build sign-in",
    );
    expect(grokAccountNotice(facts({ accountReference: null })).summary).toContain(
      "account not identified",
    );
    expect(grokAccountNotice(facts({ accountReference: null })).detail).toContain(
      "for an unidentified account",
    );
    // The sentence must read correctly in both branches.
    expect(grokAccountNotice(facts()).detail).toContain("for account usr-0a1b2c3d is valid");
  });
});

describe("presentation helpers", () => {
  it("labels every credential method in the closed vocabulary", () => {
    for (const method of CREDENTIAL_METHODS) {
      const label = credentialMethodLabel(method);
      expect(label.length).toBeGreaterThan(0);
      expect(label).not.toContain("_");
    }
  });

  it("formats durations deterministically in both directions", () => {
    expect(formatSecondsRemaining(SECONDS_REMAINING)).toBe("in 12h 30m");
    expect(formatSecondsRemaining(-1)).toBe("1s ago");
    expect(formatSecondsRemaining(0)).toBe("0s ago");
    expect(formatSecondsRemaining(90)).toBe("in 1m");
    expect(formatSecondsRemaining(200_000)).toBe("in 2d 7h");
    expect(formatSecondsRemaining(-7_200)).toBe("2h 0m ago");
  });
});

describe("shared golden fixtures", () => {
  const fixtures = JSON.parse(
    readFileSync(repoPath("docs/schemas/grokptah-account.v1.fixtures.json"), "utf8"),
  );

  it("uses the same fixed observation clock as the Rust tests", () => {
    expect(fixtures.observedAtUnix).toBe(1_787_616_000);
  });

  it("accepts every golden case and agrees about launch gating", () => {
    expect(fixtures.accepted.length).toBeGreaterThanOrEqual(8);
    for (const testCase of fixtures.accepted) {
      const parsed = parseGrokAccountFacts(testCase.facts);
      expect(parsed, `${testCase.name} should parse`).not.toBeNull();
      expect(canLaunchGrokBuild(parsed), `${testCase.name} gating`).toBe(testCase.permitsLaunch);
      expect(grokAccountNotice(parsed).blocksLaunch).toBe(!testCase.permitsLaunch);
    }
  });

  it("fails closed on every golden rejection", () => {
    expect(fixtures.rejected.length).toBeGreaterThanOrEqual(8);
    for (const testCase of fixtures.rejected) {
      expect(parseGrokAccountFacts(testCase.facts), `${testCase.name} must fail closed`).toBeNull();
      // A projection this build refuses is never launchable.
      expect(canLaunchGrokBuild(parseGrokAccountFacts(testCase.facts))).toBe(false);
    }
  });
});
