#!/usr/bin/env node
/**
 * Packaged macOS Computer Use identity inspector (#444).
 *
 * Inspects source topology and, optionally, a provided .app. Never signs,
 * never opens Keychain, never prompts TCC, never kills processes, and never
 * treats simulator/ad-hoc/unsigned identity as packaged qualification.
 *
 * Usage:
 *   node desktop/scripts/qualify-computer-use-macos-package.mjs
 *   GROKPTAH_PACKAGE_APP=/path/to/GrokPtah.app node desktop/scripts/qualify-computer-use-macos-package.mjs
 */

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
const REQUIRED_HEAD = "67e29bd34dc64049432c715c93c2cef2185c63ea";
const FORBIDDEN_ENTITLEMENT_MARKERS = [
  "com.apple.security.app-sandbox",
  "com.apple.security.automation.apple-events",
  "keychain-access-groups",
  "com.apple.security.device.audio-input",
  "com.apple.security.device.camera",
];

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

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

function diskFreeGiB() {
  const raw = run("df", ["-k", "/"]);
  const lines = raw.split("\n");
  const parts = (lines[1] || "").split(/\s+/);
  const availableKb = Number(parts[3]);
  if (!Number.isFinite(availableKb)) return 0;
  return availableKb / (1024 * 1024);
}

function parseSigningClass(text) {
  const lower = text.toLowerCase();
  if (
    lower.includes("source=notarized developer id") ||
    (lower.includes("authority=developer id application") &&
      lower.includes("notarized"))
  ) {
    return "notarized_developer_id";
  }
  if (lower.includes("authority=developer id application")) return "developer_id";
  if (lower.includes("authority=apple development")) return "apple_development";
  if (
    lower.includes("flags=0x2(adhoc)") ||
    lower.includes("signature=adhoc") ||
    lower.includes("authority=adhoc")
  ) {
    return "ad_hoc";
  }
  if (
    lower.includes("code has no signature") ||
    lower.includes("code object is not signed") ||
    lower.includes("not signed")
  ) {
    return "unsigned";
  }
  return "uninspected";
}

function countsAsPackaged(signingClass) {
  return signingClass === "notarized_developer_id";
}

function inspectApp(appPath) {
  if (!appPath) {
    return {
      present: false,
      signingClass: "uninspected",
      helperAssembled: false,
      codesign: null,
      identifier: null,
    };
  }
  const app = resolve(appPath);
  if (!existsSync(app)) {
    return {
      present: false,
      signingClass: "unsigned",
      helperAssembled: false,
      codesign: `missing app: ${app}`,
      identifier: null,
    };
  }
  const codesign = run("codesign", ["-d", "--verbose=2", app]);
  const identifierMatch = /Identifier=(\S+)/.exec(codesign);
  const helperPath = join(
    app,
    "Contents/Helpers/GrokPtah Computer Use Helper.app",
  );
  return {
    present: true,
    signingClass: parseSigningClass(codesign),
    helperAssembled: existsSync(helperPath),
    codesign,
    identifier: identifierMatch ? identifierMatch[1] : null,
    appSha256: sha256File(app),
  };
}

function main() {
  const identity = JSON.parse(readFileSync(IDENTITY_PATH, "utf8"));
  const tauri = JSON.parse(readFileSync(TAURI_CONF, "utf8"));
  const appEntitlements = readFileSync(APP_ENTITLEMENTS, "utf8");
  const helperEntitlements = readFileSync(HELPER_ENTITLEMENTS, "utf8");
  const helperInfo = readFileSync(HELPER_INFO, "utf8");
  const head = git(["rev-parse", "HEAD"]);
  const branch = git(["branch", "--show-current"]);
  let sourceGate = head === REQUIRED_HEAD;
  try {
    execFileSync("git", ["-C", REPO, "merge-base", "--is-ancestor", REQUIRED_HEAD, "HEAD"], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    sourceGate = true;
  } catch {
    sourceGate = head === REQUIRED_HEAD;
  }
  const diskFree = diskFreeGiB();
  const packageApp = process.env.GROKPTAH_PACKAGE_APP || "";
  const inspected = inspectApp(packageApp || null);

  const failures = [];
  if (tauri.identifier !== identity.app.bundleId) {
    failures.push(
      `tauri identifier ${tauri.identifier} != ${identity.app.bundleId}`,
    );
  }
  if (tauri.productName !== identity.app.productName) {
    failures.push("tauri productName mismatch");
  }
  if (tauri.version !== identity.app.version) {
    failures.push("tauri version mismatch");
  }
  if (
    tauri.bundle?.macOS?.entitlements !== "macos/GrokPtah.entitlements"
  ) {
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
      if (body.includes(marker)) {
        failures.push(`${label} entitlements contain ${marker}`);
      }
    }
  }
  if (inspected.identifier && inspected.identifier !== identity.app.bundleId) {
    failures.push(
      `inspected app identifier ${inspected.identifier} != ${identity.app.bundleId}`,
    );
  }

  const signingClass = inspected.present ? inspected.signingClass : "uninspected";
  const helperAssembled = inspected.helperAssembled;
  const reasons = [];
  if (diskFree < 20) reasons.push(`disk_below_20_gib:${diskFree.toFixed(1)}`);
  if (!countsAsPackaged(signingClass)) {
    reasons.push(`signing_class_${signingClass}_is_not_packaged_qualification`);
  }
  reasons.push("executor_is_not_the_packaged_helper");
  if (!helperAssembled) reasons.push("helper_binary_not_assembled_in_bundle");
  reasons.push("packaged_tcc_grants_not_proven_for_helper_identity");
  reasons.push("real_packaged_semantic_hardware_action_did_not_run");
  reasons.push("simulator_or_synthetic_fixture_is_not_packaged_qualification");

  const evidence = {
    schema: "grokptah-computer-use-package-authority.v1",
    sourceHead: head,
    branch,
    requiredBase: REQUIRED_HEAD,
    sourceGateOk: sourceGate,
    appBundleId: identity.app.bundleId,
    helperBundleId: identity.helper.bundleId,
    appVersion: identity.app.version,
    helperVersion: identity.helper.version,
    signingClass,
    executorKind: "in_process_host",
    helperAssembled,
    realTccHardwareActionRan: false,
    diskFreeGibMilli: Math.round(diskFree * 1000),
    os: run("sw_vers", ["-productVersion"]),
    hardware: run("sysctl", ["-n", "hw.model"]),
    artifactHashes: {
      identityJson: sha256File(IDENTITY_PATH),
      tauriConf: sha256File(TAURI_CONF),
      appEntitlements: sha256File(APP_ENTITLEMENTS),
      helperEntitlements: sha256File(HELPER_ENTITLEMENTS),
      helperInfoPlist: sha256File(HELPER_INFO),
    },
    commands: [
      "git rev-parse HEAD",
      "df -k /",
      packageApp ? `codesign -d --verbose=2 ${packageApp}` : "(no package app inspected)",
    ],
    eligibility: {
      packagedQualification: false,
      reasons,
    },
    topologyFailures: failures,
    verdict: failures.length ? "fail_closed" : "partial",
  };

  const out = process.env.GROKPTAH_PACKAGE_EVIDENCE_OUT;
  if (out) {
    writeFileSync(out, `${JSON.stringify(evidence, null, 2)}\n`);
  }
  process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
  if (failures.length) {
    process.stderr.write(
      `qualify-computer-use-macos-package: topology failures:\n${failures.map((item) => `- ${item}`).join("\n")}\n`,
    );
    process.exit(2);
  }
  process.stderr.write(
    "qualify-computer-use-macos-package: PARTIAL — synthetic/source identity only; packaged/hardware remains unqualified.\n",
  );
}

main();
