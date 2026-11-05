import type { PropertyPayload } from "./seed";
import { runBrowserEngine } from "./browser-engine";

export async function uploadWorkbook(file: File): Promise<PropertyPayload> {
  if (file.size === 0 || file.size > 10 * 1024 * 1024) {
    throw new Error("Workbook must be between 1 byte and 10 MiB.");
  }
  return await runBrowserEngine({ kind: "parse", bytes: await file.arrayBuffer() }) as PropertyPayload;
}
