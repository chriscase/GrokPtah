import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build as viteBuild } from "vite";

const packageRoot = resolve(fileURLToPath(import.meta.url), "..", "..");
const repoRoot = resolve(packageRoot, "..", "..");
const reactHostArguments = process.argv.filter((argument) =>
  argument.startsWith("--react-host="),
);
const reactHost = reactHostArguments[0]?.slice("--react-host=".length);
const hostConfigurations = {
  "18": {
    root: resolve(repoRoot, "desktop"),
    expected: {
      react: "18.3.1",
      "react-dom": "18.3.1",
      "@types/react": "18.3.31",
      "@types/react-dom": "18.3.7",
      typescript: "5.9.3",
    },
  },
  "19": {
    root: packageRoot,
    expected: {
      react: "19.2.8",
      "react-dom": "19.2.8",
      "@types/react": "19.2.18",
      "@types/react-dom": "19.2.5",
      typescript: "7.0.2",
    },
  },
};
if (reactHostArguments.length !== 1 || !hostConfigurations[reactHost]) {
  throw new Error("usage: --react-host=18|19 (exactly one selector is required)");
}
const hostConfiguration = hostConfigurations[reactHost];
const clientArtifact = resolve(repoRoot, "desktop", "dist", "public");
const workspace = await mkdtemp(join(tmpdir(), "grokptah-ui-contextdesk-"));
const consumerRoot = join(workspace, "consumer");
const packDirectory = join(workspace, "pack");
const browserOutput = join(workspace, "browser-dist");
const moduleRecordPath = join(workspace, "rollup-module-ids.json");

function run(command, args, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, {
      ...options,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk;
    });
    child.stderr.on("data", (chunk) => {
      output += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) resolveRun(output);
      else reject(new Error(`${command} ${args.join(" ")} exited ${code}:\n${output}`));
    });
  });
}

function runJson(command, args, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, {
      ...options,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code !== 0) {
        reject(new Error(`${command} ${args.join(" ")} exited ${code}:\n${stderr}`));
      } else {
        try {
          resolveRun(JSON.parse(stdout));
        } catch (error) {
          reject(new Error(`could not parse ${command} JSON output: ${error.message}`));
        }
      }
    });
  });
}

async function collectTextFiles(directory) {
  const files = [];
  async function visit(currentDirectory) {
    for (const entry of await readdir(currentDirectory, { withFileTypes: true })) {
      const currentPath = join(currentDirectory, entry.name);
      if (entry.isDirectory()) await visit(currentPath);
      else if (/\.(css|html|js|mjs|map)$/.test(entry.name)) {
        files.push(currentPath);
      }
    }
  }
  await visit(directory);
  return files;
}

async function pack(cwd) {
  const before = new Set(await readdir(packDirectory));
  await run(
    process.platform === "win32" ? "npm.cmd" : "npm",
    ["pack", "--ignore-scripts", "--pack-destination", packDirectory],
    { cwd },
  );
  const created = (await readdir(packDirectory)).find(
    (name) => name.endsWith(".tgz") && !before.has(name),
  );
  if (!created) throw new Error(`npm pack produced no archive for ${cwd}`);
  return join(packDirectory, created);
}

try {
  if (!existsSync(join(clientArtifact, "package.json"))) {
    throw new Error(
      "desktop/dist/public is missing; run the existing public-package build first",
    );
  }
  const clientManifest = JSON.parse(
    await readFile(join(clientArtifact, "package.json"), "utf8"),
  );
  if (clientManifest.name !== "@grokptah/client") {
    throw new Error("desktop/dist/public is not the generated @grokptah/client artifact");
  }

  await mkdir(packDirectory, { recursive: true });
  await mkdir(consumerRoot, { recursive: true });

  const uiArchive = await pack(packageRoot);
  const clientArchive = await pack(clientArtifact);
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const hostPackages = [
    "react",
    "react-dom",
    "@types/react",
    "@types/react-dom",
    "typescript",
  ].map((name) => resolve(hostConfiguration.root, "node_modules", ...name.split("/")));
  const hostLock = JSON.parse(
    await readFile(join(hostConfiguration.root, "package-lock.json"), "utf8"),
  );
  for (const [packageName, localPackage] of [
    ["react", hostPackages[0]],
    ["react-dom", hostPackages[1]],
    ["@types/react", hostPackages[2]],
    ["@types/react-dom", hostPackages[3]],
    ["typescript", hostPackages[4]],
  ]) {
    if (!existsSync(join(localPackage, "package.json"))) {
      throw new Error(`selected host package is unavailable: ${localPackage}`);
    }
    const installedVersion = JSON.parse(
      await readFile(join(localPackage, "package.json"), "utf8"),
    ).version;
    const lockedVersion = hostLock.packages?.[`node_modules/${packageName}`]?.version;
    const expectedVersion = hostConfiguration.expected[packageName];
    if (
      installedVersion !== expectedVersion ||
      lockedVersion !== expectedVersion
    ) {
      throw new Error(
        `${reactHost} host ${packageName} is not lock-backed at ${expectedVersion}: installed=${installedVersion}, locked=${lockedVersion}`,
      );
    }
  }

  await writeFile(
    join(consumerRoot, "package.json"),
    JSON.stringify(
      {
        name: "grokptah-ui-contextdesk-consumer",
        private: true,
        type: "module",
      },
      null,
      2,
    ),
  );
  await run(
    npm,
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--no-package-lock",
      "--no-save",
      "--prefix",
      consumerRoot,
      uiArchive,
      clientArchive,
      ...hostPackages,
    ],
    { cwd: consumerRoot },
  );

  const dependencyTree = await runJson(
    npm,
    ["ls", "--json", "--all", "--silent", "--prefix", consumerRoot],
    { cwd: consumerRoot },
  );
  function dependencyVersions(tree, packageName, found = []) {
    for (const [name, dependency] of Object.entries(tree.dependencies ?? {})) {
      if (name === packageName && dependency.version) {
        found.push(dependency.version);
      }
      dependencyVersions(dependency, packageName, found);
    }
    return found;
  }
  for (const packageName of ["react", "react-dom"]) {
    const versions = dependencyVersions(dependencyTree, packageName);
    if (
      versions.length !== 1 ||
      versions[0] !== hostConfiguration.expected[packageName]
    ) {
      throw new Error(
        `${reactHost} host must have one deduplicated ${packageName} at ${hostConfiguration.expected[packageName]}, found ${versions.join(", ")}`,
      );
    }
  }

  await writeFile(
    join(consumerRoot, "parser-fixtures.mjs"),
    `import { parseBrokerRunProjection } from "@grokptah/client";

const positive = {
  brokerRunId: "parser-positive-run",
  bindingId: "parser-positive-binding",
  state: "running",
  promptPreview: "redacted",
  createdAt: "2026-08-26T00:00:00Z",
  updatedAt: "2026-08-26T00:00:01Z",
  progress: {
    round: 2,
    maxRounds: 4,
    detail: "ignored",
    updatedAt: "2026-08-26T00:00:01Z",
  },
};
if (parseBrokerRunProjection(positive)?.state !== "running") {
  throw new Error("positive parser fixture failed");
}
if (parseBrokerRunProjection({ ...positive, privileged: "hidden" }) !== null) {
  throw new Error("privileged parser fixture did not fail closed");
}
if (parseBrokerRunProjection({
  ...positive,
  progress: { round: "bad", maxRounds: 4, detail: "ignored", updatedAt: positive.updatedAt },
}) !== null) {
  throw new Error("malformed parser fixture did not fail closed");
}
console.log("parser fixtures passed");
`,
  );
  await run(process.execPath, ["parser-fixtures.mjs"], { cwd: consumerRoot });

  await writeFile(
    join(consumerRoot, "styles.d.ts"),
    'declare module "*.css" { const stylesheet: string; export default stylesheet; }\n',
  );
  await writeFile(
    join(consumerRoot, "tsconfig.json"),
    JSON.stringify(
      {
        compilerOptions: {
          target: "ES2022",
          module: "ESNext",
          moduleResolution: "Bundler",
          jsx: "react-jsx",
          strict: true,
          skipLibCheck: true,
          noEmit: true,
        },
        include: ["consumer.tsx", "styles.d.ts"],
      },
      null,
      2,
    ),
  );
  await writeFile(
    join(consumerRoot, "consumer.tsx"),
    `import { createElement } from "react";
import { parseBrokerRunProjection } from "@grokptah/client";
import { RunStatusCard, type RunStatusSnapshot } from "@grokptah/ui";
import "@grokptah/ui/theme.css";

const projection = parseBrokerRunProjection({
  brokerRunId: "consumer-run",
  bindingId: "consumer-binding",
  state: "running",
  promptPreview: "redacted",
  createdAt: "2026-08-26T00:00:00Z",
  updatedAt: "2026-08-26T00:00:01Z",
  progress: {
    round: 3,
    maxRounds: 12,
    detail: "ignored",
    updatedAt: "2026-08-26T00:00:01Z",
  },
});
if (!projection) throw new Error("client projection parser rejected consumer fixture");
const snapshot: RunStatusSnapshot = projection;
export const element = createElement(RunStatusCard, { snapshot });
`,
  );
  await run(
    process.execPath,
    [resolve(hostPackages[4], "bin/tsc"), "--project", "tsconfig.json"],
    { cwd: consumerRoot },
  );

  await writeFile(
    join(consumerRoot, "index.html"),
    `<!doctype html>
<html lang="en">
  <head><meta charset="UTF-8"><title>Consumer</title></head>
  <body><div id="root"></div><script type="module" src="/browser-entry.js"></script></body>
</html>
`,
  );
  await writeFile(
    join(consumerRoot, "browser-entry.js"),
    `import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { parseBrokerRunProjection } from "@grokptah/client";
import { RunStatusCard } from "@grokptah/ui";
import "@grokptah/ui/theme.css";

const root = document.getElementById("root");
if (!root) throw new Error("consumer root missing");
const projection = parseBrokerRunProjection({
  brokerRunId: "browser-consumer-run",
  bindingId: "browser-consumer-binding",
  state: "running",
  promptPreview: "redacted",
  createdAt: "2026-08-26T00:00:00Z",
  updatedAt: "2026-08-26T00:00:01Z",
  progress: {
    round: 1,
    maxRounds: 2,
    detail: "ignored",
    updatedAt: "2026-08-26T00:00:01Z",
  },
});
if (!projection) throw new Error("browser consumer parser rejected its fixture");
createRoot(root).render(createElement(RunStatusCard, {
  snapshot: projection,
}));
`,
  );
  const rollupModuleIds = new Set();
  const rollupCssModuleIds = new Set();
  const recordModuleId = (moduleId) => {
    if (!moduleId) return;
    const normalizedId = moduleId.replaceAll("\\", "/");
    rollupModuleIds.add(normalizedId);
    if (normalizedId.includes(".css")) rollupCssModuleIds.add(normalizedId);
  };
  await viteBuild({
    root: consumerRoot,
    configFile: false,
    plugins: [
      {
        name: "record-contextdesk-rollup-module-ids",
        async resolveId(source, importer, options) {
          const resolved = await this.resolve(source, importer, {
            ...options,
            skipSelf: true,
          });
          if (resolved?.id) recordModuleId(resolved.id);
          return resolved;
        },
        transform(_code, id) {
          recordModuleId(id);
          return null;
        },
        async generateBundle(_outputOptions, bundle) {
          const jsModuleIds = Object.values(bundle)
            .filter((asset) => asset.type === "chunk")
            .flatMap((chunk) => Object.keys(chunk.modules))
            .map((moduleId) => {
              recordModuleId(moduleId);
              return moduleId;
            });
          const assetNames = Object.values(bundle).map((asset) => asset.fileName);
          await writeFile(
            moduleRecordPath,
            JSON.stringify(
              {
                moduleIds: [...new Set([...rollupModuleIds, ...jsModuleIds])],
                cssModuleIds: [...rollupCssModuleIds],
                assetNames,
              },
              null,
              2,
            ),
          );
        },
      },
    ],
    build: {
      outDir: browserOutput,
      emptyOutDir: true,
    },
  });

  const moduleRecord = JSON.parse(await readFile(moduleRecordPath, "utf8"));
  const moduleIds = moduleRecord.moduleIds.map((moduleId) =>
    moduleId.replaceAll("\\", "/"),
  );
  for (const requiredModuleId of [
    "/node_modules/@grokptah/client/grokptah-public.js",
    "/node_modules/@grokptah/ui/dist/grokptah-ui.js",
    "/browser-entry.js",
  ]) {
    if (!moduleIds.some((moduleId) => moduleId.endsWith(requiredModuleId))) {
      throw new Error(`Rollup graph did not record ${requiredModuleId}`);
    }
  }
  const forbiddenModuleMarkers = [
    "@tauri-apps",
    "src-tauri",
    "tauri://",
    "invoke",
    "/credentials/",
    "credential",
    "/desktop/",
    "transport",
    "/mcp/",
  ];
  const forbiddenModuleIds = moduleIds.filter((moduleId) =>
    forbiddenModuleMarkers.some((marker) => moduleId.toLowerCase().includes(marker)),
  );
  if (forbiddenModuleIds.length > 0) {
    throw new Error(
      `Rollup graph recorded forbidden module IDs: ${forbiddenModuleIds.join(", ")}`,
    );
  }

  const browserText = (
    await Promise.all(
      (await collectTextFiles(browserOutput)).map((filePath) =>
        readFile(filePath, "utf8"),
      ),
    )
  ).join("\n");
  const forbiddenGraphMarkers = [
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
    "@grokptah/client/credentials",
    "/credentials/",
    "credentials.ts",
    "credentials.js",
    "credentialStore",
  ];
  const leakedGraphMarkers = forbiddenGraphMarkers.filter((marker) =>
    browserText.includes(marker),
  );
  if (leakedGraphMarkers.length > 0) {
    throw new Error(
      `browser consumer graph contains forbidden platform or credential markers: ${leakedGraphMarkers.join(", ")}`,
    );
  }
  if (!browserText.includes("--gpt-ui-")) {
    throw new Error("browser consumer build did not include @grokptah/ui/theme.css");
  }
  if (!moduleRecord.assetNames.some((assetName) => assetName.endsWith(".css"))) {
    throw new Error("Rollup graph did not record a stylesheet asset");
  }
  if (
    !moduleRecord.cssModuleIds.some((moduleId) =>
      moduleId.includes("@grokptah/ui/dist/theme.css"),
    )
  ) {
    throw new Error("Rollup graph did not record the UI theme CSS module ID");
  }

  await writeFile(
    join(consumerRoot, "ssr.mjs"),
    `import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { RunStatusCard } from "@grokptah/ui";

const markup = renderToStaticMarkup(createElement(RunStatusCard, {
  snapshot: { state: "failed", progress: { round: 2, maxRounds: 4 } },
}));
if (!markup.includes("<article")) throw new Error("SSR did not render the status article");
if (!markup.includes("Run status")) throw new Error("SSR omitted the accessible title");
if (!markup.includes('aria-live="polite"')) throw new Error("SSR omitted polite live semantics");
if (!markup.includes("Run failed.")) throw new Error("SSR omitted fixed present-state copy");
if (!markup.includes("Round budget used")) throw new Error("SSR omitted round-meter semantics");
if (!markup.includes("Round 2 of 4 maximum")) throw new Error("SSR omitted round truth");
for (const forbidden of ["consumer-run", "consumer-binding", "redacted", "/private/"]) {
  if (markup.includes(forbidden)) throw new Error("SSR rendered an identity, prompt, or path");
}
console.log("SSR consumer render passed");
`,
  );
  await run(process.execPath, ["ssr.mjs"], { cwd: consumerRoot });
  console.log(`React ${reactHost} host fixture passed`);
  console.log("ContextDesk-shaped @grokptah/ui consumer smoke passed");
} finally {
  await rm(workspace, { recursive: true, force: true });
}
