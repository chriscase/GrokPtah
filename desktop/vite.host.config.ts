import { defineConfig } from "vite";
import { resolve } from "node:path";

/**
 * Build the trusted-host seam as its own rollup graph.
 *
 * It is deliberately a separate build from `vite.library.config.ts`: keeping
 * the graphs disjoint means the browser-safe entries can never acquire a
 * shared chunk that pulls bearer-capable modules along with it, and their
 * output stays byte-identical to a build that has no host entry at all.
 */
export default defineConfig({
  build: {
    lib: {
      entry: { "grokptah-host": resolve(process.cwd(), "src/lib/host.ts") },
      formats: ["es"],
      fileName: (_format, entryName) => `${entryName}.js`,
    },
    outDir: "dist/public",
    emptyOutDir: false,
    rollupOptions: {
      // The host seam is self-contained: no Tauri, React, or Node built-ins.
      external: [],
    },
  },
});
