import { defineConfig } from "vite";
import { resolve } from "node:path";

/** Build the Tauri-free public surface for browser and trusted adapters. */
export default defineConfig({
  build: {
    lib: {
      entry: {
        "grokptah-public": resolve(process.cwd(), "src/lib/public.ts"),
        "ui-core": resolve(process.cwd(), "src/lib/uiCore.ts"),
        "help-react": resolve(process.cwd(), "src/lib/helpPublic.ts"),
      },
      formats: ["es"],
      fileName: (_format, entryName) => `${entryName}.js`,
    },
    outDir: "dist/public",
    emptyOutDir: false,
    rollupOptions: {
      // `grokptah-public` and `ui-core` import no React and stay
      // self-contained; listing React here only affects the `help-react`
      // entry, which must not bundle a second copy of the host's React.
      external: ["react", "react-dom", "react/jsx-runtime"],
    },
  },
});
