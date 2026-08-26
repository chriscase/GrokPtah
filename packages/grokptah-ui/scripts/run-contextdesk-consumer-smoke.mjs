import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(fileURLToPath(import.meta.url), "..", "..");
const clientArtifact = resolve(packageRoot, "..", "..", "desktop", "dist", "public");
const workspace = await mkdtemp(join(tmpdir(), "grokptah-ui-contextdesk-"));
const consumerRoot = join(workspace, "consumer");
const packDirectory = join(workspace, "pack");
const browserOutput = join(workspace, "browser-dist");

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
  const localPackages = [
    "react",
    "react-dom",
    "@types/react",
    "@types/react-dom",
    "typescript",
  ].map((name) => resolve(packageRoot, "node_modules", ...name.split("/")));
  for (const localPackage of localPackages) {
    if (!existsSync(join(localPackage, "package.json"))) {
      throw new Error(`package development dependency is unavailable: ${localPackage}`);
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
      ...localPackages,
    ],
    { cwd: consumerRoot },
  );

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
    [resolve(packageRoot, "node_modules/typescript/bin/tsc"), "--project", "tsconfig.json"],
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
import { RunStatusCard } from "@grokptah/ui";
import "@grokptah/ui/theme.css";

const root = document.getElementById("root");
if (!root) throw new Error("consumer root missing");
createRoot(root).render(createElement(RunStatusCard, {
  snapshot: { state: "running", progress: { round: 1, maxRounds: 2 } },
}));
`,
  );
  await run(
    process.execPath,
    [
      resolve(packageRoot, "node_modules/vite/bin/vite.js"),
      "build",
      consumerRoot,
      "--outDir",
      browserOutput,
      "--emptyOutDir",
    ],
    { cwd: consumerRoot },
  );

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
    "Authorization: Bearer",
    "Bearer ",
    "XAI_API_KEY",
    "API_KEY",
    "apiKey",
    "/Users/",
    "/private/",
    "\\Users\\",
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
console.log("SSR consumer render passed");
`,
  );
  await run(process.execPath, ["ssr.mjs"], { cwd: consumerRoot });
  console.log("ContextDesk-shaped @grokptah/ui consumer smoke passed");
} finally {
  await rm(workspace, { recursive: true, force: true });
}
