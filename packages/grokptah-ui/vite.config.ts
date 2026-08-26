import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";

export default defineConfig({
  plugins: [react()],
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
