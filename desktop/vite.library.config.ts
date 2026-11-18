import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
export default defineConfig({
  build: {
    lib: { entry: { public: fileURLToPath(new URL("./src/lib/public.ts", import.meta.url)), "ui-core": fileURLToPath(new URL("./src/lib/uiCore.ts", import.meta.url)) }, formats: ["es"], fileName: (_format, name) => `${name}.js` },
    outDir: "dist/public", emptyOutDir: true, sourcemap: false, minify: false,
    rollupOptions: { external: [], output: { chunkFileNames: "shared.js" } },
  },
});
