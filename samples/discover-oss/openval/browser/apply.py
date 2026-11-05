#!/usr/bin/env python3
"""Apply the browser-only adapter to the pinned OpenVal frontend."""

from pathlib import Path
import shutil
import sys


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: apply.py OPENVAL_SOURCE_ROOT")
    root = Path(sys.argv[1])
    web = root / "web"
    overlay = Path(__file__).resolve().parent
    workbook = root / "src/openval/io/workbook.py"
    workbook_text = workbook.read_text()
    old = 'if vac_kv.get("opex_gross_up_at_occupancy_pct") not in (None, "")'
    new = ('if pd.notna(vac_kv.get("opex_gross_up_at_occupancy_pct")) '
           'and vac_kv.get("opex_gross_up_at_occupancy_pct") != ""')
    if workbook_text.count(old) != 1:
        raise ValueError("unexpected upstream optional gross-up handling")
    workbook.write_text(workbook_text.replace(old, new))
    api = web / "lib/api.ts"
    original = api.read_text()
    marker = "export async function fetchCashflow("
    if original.count(marker) != 1 or 'fetch(url, {' not in original:
        raise ValueError("unexpected upstream cashflow API")
    prefix = original.split(marker, 1)[0]
    api.write_text('import { runBrowserEngine } from "./browser-engine";\n' + prefix + '''
export async function fetchCashflow(
  payload: PropertyPayload,
  signal?: AbortSignal,
  frequency: Frequency = "annual"
): Promise<CashflowReport> {
  return await runBrowserEngine({ kind: "cashflow", payload, frequency }, signal) as CashflowReport;
}
''')
    for name in ("browser-engine.ts", "upload.ts", "download.ts"):
        shutil.copyfile(overlay / name, web / "lib" / name)
    public = web / "public"
    public.mkdir(exist_ok=True)
    shutil.copyfile(overlay / "openval-worker.js", public / "openval-worker.js")

    page = web / "app/page.tsx"
    text = page.read_text()
    start = text.index("  const run = useCallback(")
    end = text.index("\n  return (", start)
    text = text[:start] + '''  // A cancelled calculation must never replace a newer result.
  useEffect(() => {
    if (!hydrated) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      setLoading(true);
      setError(null);
      try {
        const result = await fetchCashflow(payload, controller.signal, frequency);
        if (!controller.signal.aborted) setReport(result);
      } catch (error) {
        if (!controller.signal.aborted) setError((error as Error).message);
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    }, 300);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [payload, frequency, hydrated]);
''' + text[end:]
    needle = '      <div className="flex-1 min-w-0">\n        <CashflowTable'
    replacement = '''      <div className="flex-1 min-w-0">
        <div className="px-4 py-2 text-xs bg-amber-100 text-amber-950" role="note">
          Pre-alpha preview. Calculations are not validated for real underwriting. Deal data is stored in this browser.
          {" "}<a className="underline" href="/source.zip" download>Source and notices</a>
        </div>
        <CashflowTable'''
    if text.count(needle) != 1:
        raise ValueError("unexpected upstream page layout")
    page.write_text(text.replace(needle, replacement))


if __name__ == "__main__":
    main()
