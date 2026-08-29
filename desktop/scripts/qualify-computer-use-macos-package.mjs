#!/usr/bin/env node
/**
 * Packaged macOS Computer Use identity inspector.
 *
 * Inspects source topology and, when one is supplied, a real .app bundle. It
 * never signs, never opens the Keychain, never prompts for TCC, never kills
 * processes, and never treats simulator, ad-hoc, or unsigned identity as
 * packaged qualification.
 *
 * Two rules mirror the Rust authority in `grokptah-isolated-visual`, and this
 * file must keep agreeing with it:
 *
 *  1. Signing facts come from `codesign`/`spctl`, parsed only from anchored
 *     `Key=Value` lines, with negated values refused. A text file inside a
 *     bundle is never evidence about that bundle.
 *  2. Expectations come from an operator trust root outside the artifact, named
 *     by GROKPTAH_COMPUTER_USE_TRUST_ROOT. Without one, the verdict is
 *     `unavailable` -- not `partial`, and never `pass`.
 *
 * Usage:
 *   node desktop/scripts/qualify-computer-use-macos-package.mjs
 *   GROKPTAH_PACKAGE_APP=/path/to/GrokPtah.app \
 *   GROKPTAH_COMPUTER_USE_TRUST_ROOT=/path/to/trust-root.json \
 *     node desktop/scripts/qualify-computer-use-macos-package.mjs
 */

import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync,
  lstatSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const DESKTOP_DIR = resolve(SCRIPT_DIR, "..");
const REPO = resolve(DESKTOP_DIR, "..");
const IDENTITY_PATH = join(
  REPO,
  "docs/schemas/grokptah-computer-use-package-identity.v1.json",
);
const TAURI_CONF = join(DESKTOP_DIR, "src-tauri/tauri.conf.json");
const APP_ENTITLEMENTS = join(DESKTOP_DIR, "src-tauri/macos/GrokPtah.entitlements");
const HELPER_ENTITLEMENTS = join(
  DESKTOP_DIR,
  "src-tauri/macos/ComputerUseHelper.entitlements",
);
const HELPER_INFO = join(
  DESKTOP_DIR,
  "src-tauri/macos/ComputerUseHelper.Info.plist",
);
const REQUIRED_BASE = "67e29bd34dc64049432c715c93c2cef2185c63ea";
const CODESIGN_BIN = "/usr/bin/codesign";
const SPCTL_BIN = "/usr/sbin/spctl";

const FORBIDDEN_ENTITLEMENT_MARKERS = [
  "com.apple.security.app-sandbox",
  "com.apple.security.automation.apple-events",
  "keychain-access-groups",
  "com.apple.security.device.audio-input",
  "com.apple.security.device.camera",
];

/** Values carrying any of these cannot be read as a positive assertion. */
const NEGATION_TOKENS = ["not ", "no ", "never", "invalid", "failed", "rejected"];

// ---------------------------------------------------------------------------
// Digests. These must match `hash_file` / `hash_bundle_manifest` in Rust; the
// isolated-visual adversarial suite asserts the agreement.
// ---------------------------------------------------------------------------

function sha256File(path) {
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) throw new Error(`refusing to hash symlink: ${path}`);
  if (!stat.isFile()) throw new Error(`refusing to hash non-file path: ${path}`);
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** Digest a file, reporting null rather than throwing if it cannot be read. */
function safeSha256File(path) {
  try {
    return sha256File(path);
  } catch {
    return null;
  }
}

function hashBundleManifest(root) {
  const files = [];
  (function walk(current, relative) {
    const stat = lstatSync(current);
    if (stat.isSymbolicLink()) {
      throw new Error(`bundle member is a symlink: ${relative || current}`);
    }
    if (stat.isDirectory()) {
      for (const name of readdirSync(current).sort()) {
        walk(join(current, name), relative ? `${relative}/${name}` : name);
      }
      return;
    }
    if (!stat.isFile()) throw new Error(`bundle member is not a file: ${relative}`);
    files.push([relative, sha256File(current)]);
  })(root, "");
  files.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  const hasher = createHash("sha256");
  for (const [relative, digest] of files) {
    hasher.update(relative);
    hasher.update("\0");
    hasher.update(digest);
    hasher.update("\0");
  }
  return hasher.digest("hex");
}

// ---------------------------------------------------------------------------
// Code identity, read from the OS
// ---------------------------------------------------------------------------

function capture(bin, args) {
  const result = spawnSync(bin, args, { encoding: "utf8" });
  const text = `${result.stdout || ""}\n${result.stderr || ""}`;
  return { text, ok: result.status === 0 };
}

/** First value for `key`, matched only as `key=` anchored at line start. */
function keyedValue(text, key) {
  return keyedValues(text, key)[0];
}

function keyedValues(text, key) {
  const prefix = `${key}=`;
  return text
    .split("\n")
    .map((line) => line.replace(/^\s+/, ""))
    .filter((line) => line.startsWith(prefix))
    .map((line) => line.slice(prefix.length).trim())
    .filter((value) => value.length > 0);
}

function positive(value) {
  if (!value || !value.trim()) return false;
  const lower = ` ${value.toLowerCase()} `;
  return !NEGATION_TOKENS.some((token) => lower.includes(token));
}

/** `codesign -d -r-` prints `designated => <requirement>`. */
function parseRequirement(text) {
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("designated =>")) {
      const value = line.slice("designated =>".length).trim();
      if (value) return value;
    }
  }
  return null;
}

function mentionsUnsigned(display) {
  return display.split("\n").some((raw) => {
    const line = raw.trim();
    return (
      line.endsWith("code object is not signed at all") ||
      line.endsWith("is not signed at all") ||
      line.endsWith("code has no signature")
    );
  });
}

function isAdHoc(display) {
  const signature = keyedValue(display, "Signature");
  if (signature && signature.toLowerCase() === "adhoc") return true;
  const flags = keyedValue(display, "CodeDirectory") || keyedValue(display, "flags");
  return Boolean(flags && flags.includes("(adhoc)"));
}

function classify(display, gatekeeper, verifyOk, gatekeeperOk) {
  const authorities = keyedValues(display, "Authority");
  if (!verifyOk) {
    if (authorities.length === 0 && mentionsUnsigned(display)) return "unsigned";
    return "uninspected";
  }
  if (authorities.some((v) => positive(v) && v.startsWith("Developer ID Application"))) {
    const source = keyedValue(gatekeeper, "source") || "";
    if (gatekeeperOk && positive(source) && source.toLowerCase() === "notarized developer id") {
      return "notarized_developer_id";
    }
    return "developer_id";
  }
  if (authorities.some((v) => positive(v) && v.startsWith("Apple Development"))) {
    return "apple_development";
  }
  if (isAdHoc(display)) return "ad_hoc";
  if (mentionsUnsigned(display)) return "unsigned";
  return "uninspected";
}

function countsAsPackaged(signingClass) {
  return signingClass === "notarized_developer_id";
}

function osProbeAvailable() {
  return (
    process.platform === "darwin" &&
    existsSync(CODESIGN_BIN) &&
    existsSync(SPCTL_BIN)
  );
}

function inspectBundle(bundlePath) {
  if (!osProbeAvailable()) {
    return { probed: false, reason: "no OS code-signing probe on this host" };
  }
  if (!existsSync(bundlePath)) {
    return { probed: false, reason: `missing bundle: ${bundlePath}` };
  }
  const display = capture(CODESIGN_BIN, ["-d", "--verbose=2", bundlePath]);
  const requirement = capture(CODESIGN_BIN, ["-d", "-r-", bundlePath]);
  const verify = capture(CODESIGN_BIN, ["--verify", "--deep", "--strict", bundlePath]);
  const gatekeeper = capture(SPCTL_BIN, ["--assess", "--type", "execute", "-vv", bundlePath]);
  return {
    probed: true,
    identifier: keyedValue(display.text, "Identifier") || null,
    teamId: keyedValue(display.text, "TeamIdentifier") || null,
    designatedRequirement: requirement.ok ? parseRequirement(requirement.text) : null,
    signingClass: classify(display.text, gatekeeper.text, verify.ok, gatekeeper.ok),
    gatekeeperAccepted: gatekeeper.ok,
    verifyOk: verify.ok,
    bundleManifestSha256: hashBundleManifest(bundlePath),
    captured: {
      display: display.text,
      requirement: requirement.text,
      gatekeeper: gatekeeper.text,
    },
  };
}

// ---------------------------------------------------------------------------
// Operator trust root
// ---------------------------------------------------------------------------

function loadTrustRoot(appPath) {
  const path = process.env.GROKPTAH_COMPUTER_USE_TRUST_ROOT;
  if (!path) {
    return { present: false, error: "GROKPTAH_COMPUTER_USE_TRUST_ROOT is not set" };
  }
  try {
    const resolved = resolve(path);
    if (appPath && resolved.startsWith(`${resolve(appPath)}/`)) {
      return {
        present: false,
        error: "trust root lives inside the bundle it would authorize",
      };
    }
    const stat = lstatSync(resolved);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      return { present: false, error: "trust root must be a regular file" };
    }
    return { present: true, root: JSON.parse(readFileSync(resolved, "utf8")), path: resolved };
  } catch (error) {
    return { present: false, error: `trust root unreadable: ${error.message}` };
  }
}

/** Requirement strings differ only in incidental whitespace. */
function normalizeRequirement(value) {
  return String(value || "").split(/\s+/).filter(Boolean).join(" ");
}

function admitAgainstTrustRoot(observed, anchor) {
  const denials = [];
  if (!observed.probed) {
    denials.push(observed.reason);
    return denials;
  }
  if (!observed.identifier) denials.push("OS reported no code-signing identifier");
  if (!observed.teamId) denials.push("OS reported no Team Identifier");
  if (!observed.designatedRequirement) {
    denials.push("OS derived no designated requirement; one is never synthesized");
  }
  if (observed.identifier && observed.identifier !== anchor.bundleId) {
    denials.push(`identifier ${observed.identifier} != trust root ${anchor.bundleId}`);
  }
  if (observed.teamId && observed.teamId !== anchor.teamId) {
    denials.push("Team ID does not match the trust root");
  }
  if (
    observed.designatedRequirement &&
    normalizeRequirement(observed.designatedRequirement) !==
      normalizeRequirement(anchor.designatedRequirement)
  ) {
    denials.push("designated requirement does not match the trust root");
  }
  if (!countsAsPackaged(observed.signingClass)) {
    denials.push(`signing class ${observed.signingClass} is not notarized Developer ID`);
  }
  if (!observed.gatekeeperAccepted || !observed.verifyOk) {
    denials.push("Gatekeeper assessment or codesign verification is incomplete");
  }
  return denials;
}

// ---------------------------------------------------------------------------

function run(cmd, args, extra = {}) {
  try {
    return execFileSync(cmd, args, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      ...extra,
    }).trim();
  } catch (error) {
    const stderr = error.stderr ? String(error.stderr).trim() : "";
    const stdout = error.stdout ? String(error.stdout).trim() : "";
    return [stdout, stderr, error.message].filter(Boolean).join("\n");
  }
}

function git(args) {
  return run("git", ["-C", REPO, ...args]);
}

/**
 * Run a git command and report its exit status separately from its output.
 * `run` above folds failures into the returned string, which is fine for
 * display but useless for a decision that turns on the exit code.
 */
function gitStatus(repo, args) {
  const result = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8" });
  return {
    code: result.status === null ? -1 : result.status,
    stdout: (result.stdout || "").trim(),
  };
}

/**
 * Ancestry of HEAD relative to the required base, as a three-valued result.
 *
 * `git merge-base --is-ancestor` exits non-zero for two very different
 * reasons: the commit genuinely is not an ancestor, and the commit is simply
 * absent from a truncated history. CI checks out a shallow merge ref, so the
 * second case is the common one — and reporting it as "does not descend" is a
 * definite negative the tool has not earned.
 *
 * So a non-zero exit is only read as `refuted` when the history is complete
 * enough to have answered: the clone is not shallow and the base commit is
 * present locally. Otherwise the answer is `indeterminate`, which every
 * consumer must treat as "not proven" rather than as either verdict.
 *
 * `runner` is injectable so the shallow case can be exercised deterministically.
 */
export function decideAncestry(repo, requiredBase, head, runner = gitStatus) {
  // Probes actually executed, so the evidence record cannot claim a command
  // that a short-circuit skipped.
  const probes = [];
  const probe = (args, label) => {
    probes.push(label);
    return runner(repo, args);
  };
  const done = (result, reason) => ({ result, reason, probes });

  if (head && head === requiredBase) {
    return done("proven", "HEAD is the required base commit");
  }

  const ancestor = probe(
    ["merge-base", "--is-ancestor", requiredBase, "HEAD"],
    "git merge-base --is-ancestor <required-base> HEAD",
  );
  if (ancestor.code === 0) {
    return done("proven", "HEAD descends from the required base");
  }

  const shallow = probe(
    ["rev-parse", "--is-shallow-repository"],
    "git rev-parse --is-shallow-repository",
  );
  if (shallow.code !== 0) {
    return done(
      "indeterminate",
      "cannot determine whether the checkout is shallow",
    );
  }
  if (shallow.stdout === "true") {
    return done(
      "indeterminate",
      "shallow checkout: the required base is outside the fetched history, " +
        "so ancestry cannot be proven or refuted here",
    );
  }

  const present = probe(
    ["cat-file", "-e", `${requiredBase}^{commit}`],
    "git cat-file -e <required-base>^{commit}",
  );
  if (present.code !== 0) {
    return done(
      "indeterminate",
      "the required base commit is not present locally, so ancestry cannot " +
        "be proven or refuted here",
    );
  }

  return done("refuted", "HEAD does not descend from the required base");
}

function diskFreeGiB() {
  const parts = (run("df", ["-k", "/"]).split("\n")[1] || "").split(/\s+/);
  const availableKb = Number(parts[3]);
  return Number.isFinite(availableKb) ? availableKb / (1024 * 1024) : 0;
}

function main() {
  const identity = JSON.parse(readFileSync(IDENTITY_PATH, "utf8"));
  const tauri = JSON.parse(readFileSync(TAURI_CONF, "utf8"));
  const appEntitlements = readFileSync(APP_ENTITLEMENTS, "utf8");
  const helperEntitlements = readFileSync(HELPER_ENTITLEMENTS, "utf8");
  const helperInfo = readFileSync(HELPER_INFO, "utf8");
  const head = git(["rev-parse", "HEAD"]);
  const branch = git(["branch", "--show-current"]);

  const ancestry = decideAncestry(REPO, REQUIRED_BASE, head);

  // --- source topology --------------------------------------------------
  const failures = [];
  if (tauri.identifier !== identity.app.bundleId) {
    failures.push(`tauri identifier ${tauri.identifier} != ${identity.app.bundleId}`);
  }
  if (tauri.productName !== identity.app.productName) failures.push("tauri productName mismatch");
  if (tauri.version !== identity.app.version) failures.push("tauri version mismatch");
  if (tauri.bundle?.macOS?.entitlements !== "macos/GrokPtah.entitlements") {
    failures.push("tauri macOS entitlements path is not wired");
  }
  if (!helperInfo.includes(identity.helper.bundleId)) {
    failures.push("helper Info.plist missing declared helper bundle id");
  }
  if (!helperInfo.includes(identity.helper.executable)) {
    failures.push("helper Info.plist missing declared helper executable");
  }
  for (const [label, body] of [
    ["app", appEntitlements],
    ["helper", helperEntitlements],
  ]) {
    for (const marker of FORBIDDEN_ENTITLEMENT_MARKERS) {
      if (body.includes(marker)) failures.push(`${label} entitlements contain ${marker}`);
    }
  }

  // --- packaged identity ------------------------------------------------
  const packageApp = process.env.GROKPTAH_PACKAGE_APP || "";
  const trust = loadTrustRoot(packageApp || null);
  const helperPath = packageApp
    ? join(packageApp, identity.helper.nestedPath)
    : null;

  let appObserved = { probed: false, reason: "no .app was supplied" };
  let helperObserved = { probed: false, reason: "no .app was supplied" };
  if (packageApp) {
    appObserved = inspectBundle(resolve(packageApp));
    helperObserved = existsSync(helperPath)
      ? inspectBundle(helperPath)
      : { probed: false, reason: "helper bundle is not assembled in the .app" };
  }

  const denials = [];
  if (!osProbeAvailable()) denials.push("no OS code-signing probe on this host");
  if (!trust.present) denials.push(trust.error);
  if (!packageApp) denials.push("no packaged .app was supplied for inspection");
  // Unproven ancestry is never treated as proven. Refuted is an active
  // failure; indeterminate merely cannot decide, and both stop short of
  // `partial` rather than being waved through.
  if (ancestry.result !== "proven") {
    denials.push(`base ancestry ${ancestry.result}: ${ancestry.reason}`);
  }
  if (trust.present && packageApp) {
    denials.push(...admitAgainstTrustRoot(appObserved, trust.root.app).map((d) => `app: ${d}`));
    denials.push(
      ...admitAgainstTrustRoot(helperObserved, trust.root.helper).map((d) => `helper: ${d}`),
    );
  }

  // Verdict vocabulary, deliberately capped. `pass` is unreachable from this
  // script: it inspects, and never observes TCC grants or a hardware action.
  let verdict;
  if (failures.length > 0 || ancestry.result === "refuted") {
    // Refuted ancestry is a definite negative: this tree is not the reviewed
    // base, and that is a failure rather than a gap in what we could observe.
    verdict = "fail_closed";
  } else if (
    !osProbeAvailable() ||
    !trust.present ||
    !packageApp ||
    ancestry.result === "indeterminate"
  ) {
    // Indeterminate ancestry sits with the other "could not establish it"
    // cases. It can never reach `partial`.
    verdict = "unavailable";
  } else if (denials.length > 0) {
    verdict = "fail_closed";
  } else {
    verdict = "partial";
  }

  const evidence = {
    schema: "grokptah-computer-use-package-authority.v1",
    kind: "source_topology_and_code_identity_inspector",
    sourceHead: head,
    branch,
    requiredBase: REQUIRED_BASE,
    // Three-valued on purpose. There is deliberately no boolean here: a
    // shallow checkout that cannot answer must not read as "does not descend".
    baseAncestry: { result: ancestry.result, reason: ancestry.reason },
    verdict,
    appBundleId: identity.app.bundleId,
    helperBundleId: identity.helper.bundleId,
    appVersion: identity.app.version,
    helperVersion: identity.helper.version,
    trustRoot: {
      present: trust.present,
      // Identified by digest, not by location. An operator can confirm which
      // trust root was used by hashing their own copy; the path would only
      // disclose their machine layout.
      sha256: trust.present ? safeSha256File(trust.path) : null,
      issuer: trust.present ? trust.root.issuer ?? null : null,
      error: trust.error || null,
    },
    osProbeAvailable: osProbeAvailable(),
    app: appObserved.probed
      ? {
          identifier: appObserved.identifier,
          teamId: appObserved.teamId,
          signingClass: appObserved.signingClass,
          designatedRequirement: appObserved.designatedRequirement,
          gatekeeperAccepted: appObserved.gatekeeperAccepted,
          bundleManifestSha256: appObserved.bundleManifestSha256,
        }
      : { probed: false, reason: appObserved.reason },
    helper: helperObserved.probed
      ? {
          identifier: helperObserved.identifier,
          teamId: helperObserved.teamId,
          signingClass: helperObserved.signingClass,
          designatedRequirement: helperObserved.designatedRequirement,
          gatekeeperAccepted: helperObserved.gatekeeperAccepted,
          bundleManifestSha256: helperObserved.bundleManifestSha256,
        }
      : { probed: false, reason: helperObserved.reason },
    helperAssembled: helperObserved.probed,
    // Nothing below was observed by this script, and it says so rather than
    // leaving the reader to infer it.
    tccGrantsObserved: false,
    notarizationPerformed: false,
    virtualizationFrameworkObserved: false,
    guestBootObserved: false,
    framesObserved: false,
    inputDispatched: false,
    hardwareActionObserved: false,
    soakObserved: false,
    diskFreeGibMilli: Math.round(diskFreeGiB() * 1000),
    os: run("sw_vers", ["-productVersion"]),
    hardware: run("sysctl", ["-n", "hw.model"]),
    artifactHashes: {
      identityJson: sha256File(IDENTITY_PATH),
      tauriConf: sha256File(TAURI_CONF),
      appEntitlements: sha256File(APP_ENTITLEMENTS),
      helperEntitlements: sha256File(HELPER_ENTITLEMENTS),
      helperInfoPlist: sha256File(HELPER_INFO),
    },
    // What was run, not where. Interpolating the supplied paths here would
    // put the operator's filesystem layout into a shareable record.
    commands: [
      "git rev-parse HEAD",
      ...ancestry.probes,
      "df -k /",
      packageApp
        ? "codesign -d --verbose=2 <packaged-app>"
        : "(no package app inspected)",
      packageApp
        ? "spctl --assess --type execute -vv <packaged-app>"
        : "(no package app inspected)",
    ],
    topologyFailures: failures,
    admissionDenials: denials,
  };

  const out = process.env.GROKPTAH_PACKAGE_EVIDENCE_OUT;
  if (out) writeFileSync(out, `${JSON.stringify(evidence, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);

  if (failures.length) {
    process.stderr.write(
      `qualify-computer-use-macos-package: topology failures:\n${failures
        .map((item) => `- ${item}`)
        .join("\n")}\n`,
    );
    process.exit(2);
  }
  process.stderr.write(
    `qualify-computer-use-macos-package: ${verdict.toUpperCase()} — ` +
      "source topology only; packaged signing, TCC, and hardware remain unqualified.\n",
  );
}

// Importable for the focused ancestry regression; only runs when invoked
// directly, so importing this module has no side effects.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
