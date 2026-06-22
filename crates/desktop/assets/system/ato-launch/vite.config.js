import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  base: "./",
  resolve: {
    alias: {
      "@ato/ato-ui": path.resolve(__dirname, "../ato-ui/src/index.js"),
    },
  },
  build: {
    outDir: "dist",
    // vite-plugin-singlefile inlines all assets into one HTML
    assetsInlineLimit: Infinity,
    cssCodeSplit: false,
  },
});
