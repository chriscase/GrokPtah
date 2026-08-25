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
