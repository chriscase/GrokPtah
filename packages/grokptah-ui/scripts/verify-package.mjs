import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(fileURLToPath(import.meta.url), "..", "..");
const repoRoot = resolve(packageRoot, "..", "..");
const packagePrefix = "packages/grokptah-ui/";

const expectedSourceFiles = new Set([
  ".gitignore",
  "README.md",
  "package.json",
  "package-lock.json",
  "tsconfig.json",
  "tsconfig.build.json",
  "vite.config.ts",
  "src/index.ts",
  "src/RunStatusCard.tsx",
  "src/theme.css",
  "src/RunStatusCard.test.tsx",
  "src/test/setup.ts",
  "scripts/verify-package.mjs",
  "scripts/run-contextdesk-consumer-smoke.mjs",
]);

function fail(message) {
  throw new Error(`@grokptah/ui verification failed: ${message}`);
}

function collectFiles(directory, relative = "", ignoredDirectories = new Set()) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const childRelative = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      if (!ignoredDirectories.has(entry.name)) {
        files.push(...collectFiles(resolve(directory, entry.name), childRelative, ignoredDirectories));
      }
    } else if (entry.isFile()) {
      files.push(childRelative);
    }
  }
  return files;
}

function read(relativePath) {
  const absolutePath = resolve(packageRoot, relativePath);
  if (!existsSync(absolutePath)) fail(`missing ${relativePath}`);
  return readFileSync(absolutePath, "utf8");
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) fail(`${command} could not run: ${result.error.message}`);
  if (result.status !== 0) {
    fail(`${command} ${args.join(" ")} failed:\n${result.stdout}\n${result.stderr}`);
  }
  return result.stdout;
}

const actualSourceFiles = new Set(
  collectFiles(packageRoot, "", new Set(["node_modules", "dist", "coverage"])),
);
const undeclaredFiles = [...actualSourceFiles].filter(
  (relativePath) => !expectedSourceFiles.has(relativePath),
);
if (undeclaredFiles.length > 0) {
  fail(`undeclared package files: ${undeclaredFiles.join(", ")}`);
}

const gitStatus = run("git", ["status", "--porcelain", "--untracked-files=all"], repoRoot);
const unexpectedGitPaths = gitStatus
  .split("\n")
  .filter(Boolean)
  .map((line) => line.slice(3).split(" -> ").at(-1))
  .filter((relativePath) => !expectedSourceFiles.has(relativePath.slice(packagePrefix.length)));
if (unexpectedGitPaths.length > 0) {
  fail(`unexpected changed paths: ${unexpectedGitPaths.join(", ")}`);
}

const manifest = JSON.parse(read("package.json"));
if (
  manifest.name !== "@grokptah/ui" ||
  manifest.version !== "0.0.0-development" ||
  manifest.private !== true ||
  manifest.type !== "module"
) {
  fail("manifest identity or private staging flag is incorrect");
}
if (
  manifest.peerDependencies?.react !== ">=18.0.0" ||
  manifest.peerDependencies?.["react-dom"] !== ">=18.0.0"
) {
  fail("React and ReactDOM must remain external peer dependencies");
}
if (
  JSON.stringify(manifest.sideEffects) !== JSON.stringify(["./dist/theme.css"])
) {
  fail("the stylesheet must be the only declared package side effect");
}
if (
  JSON.stringify(manifest.exports) !==
  JSON.stringify({
    ".": {
      types: "./dist/index.d.ts",
      import: "./dist/grokptah-ui.js",
    },
    "./theme.css": "./dist/theme.css",
  })
) {
  fail("manifest exports do not match the package contract");
}
if (
  JSON.stringify(manifest.files) !==
  JSON.stringify(["dist", "README.md", "package.json"])
) {
  fail("manifest files list is not minimal");
}

const indexSource = read("src/index.ts");
if (
  !indexSource.includes("export { RunStatusCard }") ||
  !indexSource.includes("export type { RunStatusSnapshot }")
) {
  fail("index source is missing the required component or structural type export");
}

const packageLock = JSON.parse(read("package-lock.json"));
if (
  packageLock.name !== "@grokptah/ui" ||
  packageLock.packages?.[""]?.name !== "@grokptah/ui" ||
  packageLock.packages?.[""]?.version !== "0.0.0-development"
) {
  fail("package lock does not describe the private package");
}

const sourceToScan = [
  "README.md",
  "package.json",
  "src/index.ts",
  "src/RunStatusCard.tsx",
  "src/theme.css",
].map((relativePath) => [relativePath, read(relativePath)]);
const forbiddenMarkers = [
  "@tauri-apps",
  "src-tauri",
  "tauri://",
  "invoke(",
  "invoke<",
  "Authorization: Bearer",
  "Bearer ",
  "XAI_API_KEY",
  "API_KEY",
  "apiKey",
  "/Users/",
  "/private/",
  "\\Users\\",
  "native-path",
  "native_path",
  "nativePath",
  "credential",
];
for (const [relativePath, source] of sourceToScan) {
  const matches = forbiddenMarkers.filter((marker) => source.includes(marker));
  if (matches.length > 0) {
    fail(`${relativePath} contains forbidden privacy markers: ${matches.join(", ")}`);
  }
}

const runtimeSource = sourceToScan
  .filter(([relativePath]) => relativePath.startsWith("src/"))
  .map(([, source]) => source)
  .join("\n");
const sideEffectMarkers = [
  "document.",
  "window.",
  "globalThis",
  "localStorage",
  "sessionStorage",
  "fetch(",
  "XMLHttpRequest",
  "WebSocket",
  "console.",
  "addEventListener",
  "createRoot(",
  "appendChild(",
  "url(",
];
const sideEffectMatches = sideEffectMarkers.filter((marker) =>
  runtimeSource.includes(marker),
);
if (sideEffectMatches.length > 0) {
  fail(`runtime source contains unexpected side effects: ${sideEffectMatches.join(", ")}`);
}
if (/^(\s*)(:root|html|body)\b/m.test(read("src/theme.css"))) {
  fail("theme.css contains a document-level selector");
}

const distFiles = new Set(
  collectFiles(resolve(packageRoot, "dist"), "", new Set()),
);
const expectedDistFiles = new Set([
  "RunStatusCard.d.ts",
  "grokptah-ui.js",
  "index.d.ts",
  "theme.css",
]);
if (
  [...distFiles].some((relativePath) => !expectedDistFiles.has(relativePath)) ||
  [...expectedDistFiles].some((relativePath) => !distFiles.has(relativePath))
) {
  fail(`built dist inventory mismatch: ${[...distFiles].join(", ")}`);
}

const bundle = read("dist/grokptah-ui.js");
if (!/\bfrom\s+["']react(?:\/[^"']*)?["']/.test(bundle)) {
  fail("built ESM does not keep React runtime imports external");
}
const bundledReactMarkers = [
  "REACT_ELEMENT_TYPE",
  "react.production.min.js",
  "react-dom.production.min.js",
  "Symbol.for(\"react.element\")",
];
const bundledReactMatches = bundledReactMarkers.filter((marker) =>
  bundle.includes(marker),
);
if (bundledReactMatches.length > 0) {
  fail(`React appears bundled: ${bundledReactMatches.join(", ")}`);
}

const declarations = read("dist/index.d.ts");
if (
  !declarations.includes("RunStatusCard") ||
  !declarations.includes("RunStatusSnapshot")
) {
  fail("built declarations are missing the required exports");
}
const theme = read("dist/theme.css");
for (const requiredThemeMarker of [
  "--gpt-ui-",
  "@media (forced-colors: active)",
  "@media (prefers-reduced-motion: reduce)",
  "@container gpt-ui-run-status",
  "@media (max-width: 20rem)",
]) {
  if (!theme.includes(requiredThemeMarker)) {
    fail(`built theme is missing ${requiredThemeMarker}`);
  }
}
if (theme.includes("url(")) fail("theme must not reference external assets");

const packJson = JSON.parse(
  run("npm", ["pack", "--dry-run", "--ignore-scripts", "--json"], packageRoot),
);
const packedFiles = new Set(packJson[0]?.files?.map(({ path }) => path) ?? []);
const expectedPackedFiles = new Set([
  "README.md",
  "dist/RunStatusCard.d.ts",
  "dist/grokptah-ui.js",
  "dist/index.d.ts",
  "dist/theme.css",
  "package.json",
]);
if (
  [...packedFiles].some((relativePath) => !expectedPackedFiles.has(relativePath)) ||
  [...expectedPackedFiles].some((relativePath) => !packedFiles.has(relativePath))
) {
  fail(`npm pack inventory mismatch: ${[...packedFiles].join(", ")}`);
}

console.log(
  `@grokptah/ui verifier passed: ${[...packedFiles].sort().join(", ")}`,
);
