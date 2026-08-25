import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts"
) {
  throw new Error("public package manifest does not expose the expected safe entry point");
}
const forbidden = ["@tauri-apps", "trusted.ts", "Authorization: Bearer"];
const leaked = forbidden.filter((needle) => bundle.includes(needle));
if (leaked.length > 0) {
  throw new Error(`public bundle contains forbidden authority markers: ${leaked.join(", ")}`);
}

const publicApi = await import(bundlePath.href);
const requiredExports = [
  "GrokPtahBrokerClient",
  "searchHelp",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
