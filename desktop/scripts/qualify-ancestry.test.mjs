#!/usr/bin/env node
/**
 * Focused regression for base-ancestry reporting in the packaged inspector.
 *
 * The defect this guards against: `git merge-base --is-ancestor` exits
 * non-zero both when a commit genuinely is not an ancestor and when it is
 * merely missing from a truncated history. CI checks out a shallow merge ref,
 * so the second case is the normal one there, and reporting it as a definite
 * "does not descend" is a negative the tool has not earned.
 *
 * Two levels of coverage, because neither substitutes for the other:
 *   1. the decision table, driven through an injected runner;
 *   2. a real shallow clone of a real merge commit, driven through real git.
 *
 * Usage: node scripts/qualify-ancestry.test.mjs
 */

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { decideAncestry } from "./qualify-computer-use-macos-package.mjs";

const BASE = "67e29bd34dc64049432c715c93c2cef2185c63ea";
let failures = 0;

function check(name, condition, detail = "") {
  if (condition) {
    process.stdout.write(`ok   ${name}\n`);
  } else {
    process.stdout.write(`FAIL ${name}${detail ? ` — ${detail}` : ""}\n`);
    failures += 1;
  }
}

/** A runner that answers from a scripted table keyed by the git subcommand. */
function scripted(table) {
  return (_repo, args) => {
    if (args[0] === "merge-base") return table.isAncestor;
    if (args[0] === "rev-parse") return table.isShallow;
    if (args[0] === "cat-file") return table.basePresent;
    throw new Error(`unexpected git call: ${args.join(" ")}`);
  };
}

// --- 1. decision table ----------------------------------------------------

{
  // The CI case: shallow merge-ref checkout. is-ancestor exits 1 only because
  // the base is outside the fetched history.
  const got = decideAncestry(
    "/nonexistent",
    BASE,
    "3eecc7c5f45384f77c0c129ad398b5098737349f",
    scripted({
      isAncestor: { code: 1, stdout: "" },
      isShallow: { code: 0, stdout: "true" },
      basePresent: { code: 1, stdout: "" },
    }),
  );
  check(
    "shallow merge-ref checkout is indeterminate, never refuted",
    got.result === "indeterminate",
    `got ${got.result}`,
  );
  check(
    "indeterminate result explains itself",
    /shallow/i.test(got.reason),
    got.reason,
  );
}

{
  // Full history, base present, genuinely unrelated: this one has earned a no.
  const got = decideAncestry(
    "/nonexistent",
    BASE,
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    scripted({
      isAncestor: { code: 1, stdout: "" },
      isShallow: { code: 0, stdout: "false" },
      basePresent: { code: 0, stdout: "" },
    }),
  );
  check("full history with an absent ancestor is refuted", got.result === "refuted", got.result);
}

{
  // Full history but the base object was never fetched: still cannot answer.
  const got = decideAncestry(
    "/nonexistent",
    BASE,
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    scripted({
      isAncestor: { code: 1, stdout: "" },
      isShallow: { code: 0, stdout: "false" },
      basePresent: { code: 1, stdout: "" },
    }),
  );
  check("a missing base commit is indeterminate", got.result === "indeterminate", got.result);
}

{
  // git itself unusable (not a repo, git absent): cannot answer.
  const got = decideAncestry(
    "/nonexistent",
    BASE,
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    scripted({
      isAncestor: { code: 128, stdout: "" },
      isShallow: { code: 128, stdout: "" },
      basePresent: { code: 128, stdout: "" },
    }),
  );
  check("an unusable git is indeterminate", got.result === "indeterminate", got.result);
}

{
  const got = decideAncestry("/nonexistent", BASE, BASE, scripted({}));
  check("HEAD equal to the base is proven", got.result === "proven", got.result);
}

{
  const got = decideAncestry(
    "/nonexistent",
    BASE,
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    scripted({ isAncestor: { code: 0, stdout: "" } }),
  );
  check("a real descendant is proven", got.result === "proven", got.result);
}

// --- 2. a real shallow clone of a real merge commit ------------------------

const work = mkdtempSync(join(tmpdir(), "grokptah-ancestry-"));
try {
  const origin = join(work, "origin");
  const g = (repo, ...args) =>
    execFileSync("git", ["-C", repo, ...args], {
      encoding: "utf8",
      env: {
        ...process.env,
        GIT_AUTHOR_NAME: "t",
        GIT_AUTHOR_EMAIL: "t@example.invalid",
        GIT_COMMITTER_NAME: "t",
        GIT_COMMITTER_EMAIL: "t@example.invalid",
      },
    }).trim();

  execFileSync("git", ["init", "-q", "-b", "main", origin]);
  writeFileSync(join(origin, "a.txt"), "base\n");
  g(origin, "add", "a.txt");
  g(origin, "commit", "-qm", "base");
  const realBase = g(origin, "rev-parse", "HEAD");

  // A feature branch and a merge, so HEAD is a merge commit like CI's.
  g(origin, "checkout", "-q", "-b", "feature");
  writeFileSync(join(origin, "b.txt"), "feature\n");
  g(origin, "add", "b.txt");
  g(origin, "commit", "-qm", "feature");
  g(origin, "checkout", "-q", "main");
  writeFileSync(join(origin, "c.txt"), "main\n");
  g(origin, "add", "c.txt");
  g(origin, "commit", "-qm", "main moves on");
  g(origin, "merge", "-q", "--no-ff", "-m", "merge feature", "feature");
  const mergeHead = g(origin, "rev-parse", "HEAD");

  // Full clone: ancestry is provable, and it is proven.
  const full = join(work, "full");
  execFileSync("git", ["clone", "-q", origin, full]);
  const fullResult = decideAncestry(full, realBase, g(full, "rev-parse", "HEAD"));
  check(
    "full clone of a merge commit proves ancestry",
    fullResult.result === "proven",
    `${fullResult.result}: ${fullResult.reason}`,
  );

  // Shallow clone: the base is outside the fetched history. This is the exact
  // shape CI produces, and it must not read as refuted.
  const shallow = join(work, "shallow");
  execFileSync("git", ["clone", "-q", "--depth", "1", `file://${origin}`, shallow]);
  check(
    "the shallow clone really is shallow",
    g(shallow, "rev-parse", "--is-shallow-repository") === "true",
  );
  check(
    "the shallow clone really is at the merge commit",
    g(shallow, "rev-parse", "HEAD") === mergeHead,
  );
  const shallowResult = decideAncestry(
    shallow,
    realBase,
    g(shallow, "rev-parse", "HEAD"),
  );
  check(
    "shallow clone reports indeterminate, not refuted",
    shallowResult.result === "indeterminate",
    `${shallowResult.result}: ${shallowResult.reason}`,
  );
  check(
    "shallow clone never reports proven either",
    shallowResult.result !== "proven",
    shallowResult.result,
  );
} finally {
  rmSync(work, { recursive: true, force: true });
}

// --- 3. the projection carries no filesystem paths ------------------------

{
  const encoded = JSON.stringify({
    baseAncestry: decideAncestry("/nonexistent", BASE, BASE, scripted({})),
  });
  check(
    "the ancestry projection carries no filesystem path",
    !/[/\\](tmp|home|Users|private|github)[/\\]/.test(encoded),
    encoded,
  );
  const shapes = [
    decideAncestry("/nonexistent", BASE, BASE, scripted({})),
    decideAncestry(
      "/nonexistent",
      BASE,
      "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
      scripted({
        isAncestor: { code: 1, stdout: "" },
        isShallow: { code: 0, stdout: "true" },
        basePresent: { code: 1, stdout: "" },
      }),
    ),
    decideAncestry(
      "/nonexistent",
      BASE,
      "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
      scripted({
        isAncestor: { code: 1, stdout: "" },
        isShallow: { code: 0, stdout: "false" },
        basePresent: { code: 0, stdout: "" },
      }),
    ),
  ];
  check(
    "every ancestry result is one of the three named states, never a boolean",
    shapes.every(
      (s) =>
        typeof s.result === "string" &&
        ["proven", "refuted", "indeterminate"].includes(s.result) &&
        typeof s.reason === "string" &&
        s.reason.length > 0,
    ),
    JSON.stringify(shapes),
  );
  check(
    "the retired boolean field is gone",
    !/"(descendsFromRequiredBase)"/.test(encoded),
    encoded,
  );
}

process.stdout.write(
  failures === 0
    ? "\nancestry regression: all checks passed\n"
    : `\nancestry regression FAILED: ${failures} check(s)\n`,
);
process.exit(failures === 0 ? 0 : 1);
