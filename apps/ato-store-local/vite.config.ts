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

function resolveFirstExistingPath(candidates: string[]) {
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }

  return candidates[0];
}

function resolveWorkspacePath(...segments: string[]) {
  return resolveFirstExistingPath(
    workspaceRoots.map((root) => path.join(root, ...segments)),
  );
}

function resolvePackageEntrypoint(packageName: string, entrypoint: string) {
  return resolveFirstExistingPath([
    resolveWorkspacePath("packages", packageName, entrypoint),
    path.join(appDir, "node_modules", "@ato", packageName, entrypoint),
  ]);
}

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      "@ato/dock-domain": resolvePackageEntrypoint(
        "dock-domain",
        path.join("src", "index.ts"),
      ),
      "@ato/dock-react": resolvePackageEntrypoint(
        "dock-react",
        path.join("src", "index.tsx"),
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
