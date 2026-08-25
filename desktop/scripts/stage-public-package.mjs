import { copyFile } from "node:fs/promises";

await copyFile(
  new URL("../public-package.json", import.meta.url),
  new URL("../dist/public/package.json", import.meta.url),
);
console.log("staged public package manifest: @grokptah/client@0.1.0");
