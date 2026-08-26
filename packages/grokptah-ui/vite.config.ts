import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export default defineConfig({
  plugins: [
    react(),
    {
      name: "grokptah-ui-theme-asset",
      generateBundle() {
        this.emitFile({
          type: "asset",
          fileName: "theme.css",
          source: readFileSync(resolve(process.cwd(), "src/theme.css"), "utf8"),
        });
      },
    },
  ],
  build: {
    lib: {
      entry: resolve(process.cwd(), "src/index.ts"),
      formats: ["es"],
      fileName: () => "grokptah-ui.js",
    },
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      external: (id) =>
        id === "react" || id === "react-dom" || id.startsWith("react/"),
      output: {
        assetFileNames: (assetInfo) =>
          assetInfo.name?.endsWith(".css")
            ? "theme.css"
            : "assets/[name][extname]",
      },
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    setupFiles: ["./src/test/setup.ts"],
  },
});
