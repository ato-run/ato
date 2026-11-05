import { useEffect, useState } from "react";

// The hosted static surface serves the entry page at / and does not promise
// a server-side fallback for arbitrary paths. Keep navigation in the fragment.
function currentPath(): string {
  const fragment = window.location.hash.slice(1);
  return fragment.startsWith("/") ? fragment : "/";
}
export function usePath(): string {
  const [path, setPath] = useState(currentPath);
  useEffect(() => {
    const onChange = () => setPath(currentPath());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return path;
}
export function navigate(to: string): void {
  if (currentPath() !== to) window.location.hash = to;
}
