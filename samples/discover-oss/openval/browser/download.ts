import type { PropertyPayload } from "./seed";
import { runBrowserEngine } from "./browser-engine";

export async function downloadWorkbook(payload: PropertyPayload): Promise<void> {
  const bytes = await runBrowserEngine({ kind: "export", payload });
  if (!(bytes instanceof ArrayBuffer)) throw new Error("Workbook generation failed.");
  const blob = new Blob([bytes], { type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  const name = String(payload.name || "openval-workbook").replace(/[^a-zA-Z0-9_-]+/g, "_");
  link.href = url;
  link.download = `${name || "openval-workbook"}.xlsx`;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}
