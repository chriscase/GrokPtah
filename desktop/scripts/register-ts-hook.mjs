/** Registers the extensionless-TypeScript resolver hook for offline tooling. */
import { register } from "node:module";
register("./ts-resolve-hook.mjs", import.meta.url);
