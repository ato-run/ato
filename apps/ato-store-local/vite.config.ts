import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      "@ato/dock-domain": new URL(
        "../../../../packages/dock-domain/src/index.ts",
        import.meta.url,
      ).pathname,
      "@ato/dock-react": new URL(
        "../../../../packages/dock-react/src/index.tsx",
        import.meta.url,
      ).pathname,
      "lucide-react": new URL("./node_modules/lucide-react", import.meta.url)
        .pathname,
      react: new URL("./node_modules/react", import.meta.url).pathname,
      "react-dom": new URL("./node_modules/react-dom", import.meta.url).pathname,
    },
  },
  server: {
    proxy: {
      "/v1": {
        target: "http://localhost:8080",
        changeOrigin: true,
      },
    },
  },
});
