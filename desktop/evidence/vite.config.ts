import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/** Static build of the Help evidence harness; screenshotted over file://. */
export default defineConfig({
  root: __dirname,
  base: "./",
  plugins: [react()],
  build: { outDir: "../evidence-dist", emptyOutDir: true },
});
