import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const appDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoots = [
  path.resolve(appDir, "../../../../"),
  path.resolve(appDir, "../../"),
];

function resolveWorkspacePath(...segments: string[]) {
  for (const root of workspaceRoots) {
    const candidate = path.join(root, ...segments);
    if (existsSync(candidate)) {
      return candidate;
    }
  }

  return path.join(workspaceRoots[0], ...segments);
}

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      "@ato/dock-domain": resolveWorkspacePath(
        "packages",
        "dock-domain",
        "src",
        "index.ts",
      ),
      "@ato/dock-react": resolveWorkspacePath(
        "packages",
        "dock-react",
        "src",
        "index.tsx",
      ),
      "lucide-react": path.join(appDir, "node_modules", "lucide-react"),
      react: path.join(appDir, "node_modules", "react"),
      "react-dom": path.join(appDir, "node_modules", "react-dom"),
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
