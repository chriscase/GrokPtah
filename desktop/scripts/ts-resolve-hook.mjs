/**
 * Node module-resolution hook for the offline Help tooling.
 *
 * The desktop sources use bundler-style extensionless imports. The corpus
 * verifier, model builder, and retrieval eval must execute the *same*
 * TypeScript the app ships — re-implementing the tokenizer or the digest in a
 * script would let the checked-in artifacts drift from runtime behavior — so
 * this hook resolves `./x` to `./x.ts` and lets Node's built-in type stripping
 * run the real modules.
 */
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

export async function resolve(specifier, context, nextResolve) {
  if (specifier.startsWith(".") && !/\.[cm]?[jt]sx?$/.test(specifier)) {
    const parent = context.parentURL ? fileURLToPath(context.parentURL) : process.cwd();
    const base = new URL(specifier, pathToFileURL(parent));
    for (const extension of [".ts", ".tsx", "/index.ts"]) {
      const candidate = new URL(base.href + extension);
      if (existsSync(fileURLToPath(candidate))) {
        return { url: candidate.href, shortCircuit: true };
      }
    }
  }
  return nextResolve(specifier, context);
}

/**
 * Node 22.14 does not infer a load format for an explicitly resolved `.ts`
 * URL. Tell the built-in type-stripper to treat these source files as ESM.
 * Keeping this in the shared hook makes the corpus/model/eval scripts execute
 * exactly the modules that Vite and TypeScript ship.
 */
export async function load(url, context, nextLoad) {
  if (url.endsWith(".ts") || url.endsWith(".tsx")) {
    const source = await readFile(new URL(url), "utf8");
    return {
      format: "module",
      shortCircuit: true,
      source: stripTypeScriptTypes(source, { mode: "strip" }),
    };
  }
  return nextLoad(url, context);
}
