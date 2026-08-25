import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { extractPublicTokens } from "./extractPublicTokens.mjs";

await copyFile(
  new URL("../public-package.json", import.meta.url),
  new URL("../dist/public/package.json", import.meta.url),
);

// The shared visual layer is derived from the desktop stylesheet, never
// hand-maintained beside it: one source keeps desktop and browser from drifting.
const css = await readFile(
  new URL("../src/styles/app.css", import.meta.url),
  "utf8",
);
await mkdir(new URL("../dist/public/styles/", import.meta.url), {
  recursive: true,
});
await writeFile(
  new URL("../dist/public/styles/tokens.css", import.meta.url),
  extractPublicTokens(css),
);

console.log("staged public package manifest: @grokptah/client@0.1.0 (+ styles/tokens.css)");
