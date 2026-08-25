import { defineConfig } from "vite";
import { resolve } from "node:path";

/** Build the Tauri-free public surface for browser and trusted adapters. */
export default defineConfig({
  build: {
    lib: {
      entry: {
        "grokptah-public": resolve(process.cwd(), "src/lib/public.ts"),
        "ui-core": resolve(process.cwd(), "src/lib/uiCore.ts"),
      },
      formats: ["es"],
      fileName: (_format, entryName) => `${entryName}.js`,
    },
    outDir: "dist/public",
    emptyOutDir: false,
    rollupOptions: {
      // The public entry is intentionally self-contained and has no Tauri or
      // React runtime dependency. Keep browser consumers dependency-free.
      external: [],
    },
  },
});
