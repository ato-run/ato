import { zipSync } from "fflate";
import files from "./source-manifest.json";

const content: Record<string, Uint8Array> = {};
const list = document.querySelector<HTMLElement>("#files")!;
for (const [path, encoded] of Object.entries(files)) {
  const bytes = atob(encoded);
  content[path] = Uint8Array.from(bytes, (byte) => byte.charCodeAt(0));
  const item = document.createElement("div");
  item.textContent = path;
  list.append(item);
}
document.querySelector<HTMLElement>("#status")!.textContent = `${Object.keys(content).length} ファイル`;
document.querySelector<HTMLButtonElement>("#download")!.addEventListener("click", () => {
  const archive = zipSync(content);
  const url = URL.createObjectURL(new Blob([archive], { type: "application/zip" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = "wakerun-browser-source.zip";
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
});
