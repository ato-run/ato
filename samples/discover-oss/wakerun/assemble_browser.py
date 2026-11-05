"""Apply the Wakerun browser adapter to a disposable, pinned upstream tree."""
import hashlib
import shutil
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
here = Path(__file__).resolve().parent
if (root / ".git").exists():
    raise SystemExit("Use a disposable copy, not a Git checkout")

hashes = {
    "src/lib/export.ts": "e889664d005fbb01ec596d659b14ac130d5f128dd8ac32ae33f15c91c34913c6",
    "src/lib/ai.ts": "3a32be6d4ea20bf36c24fbc1717d6744ad5b9113989473a434b326cab96cc886",
    "src/components/settings/BackupSettings.tsx": "b536c010492d90e4fd2bd9bc442cfbe2df429143da8e1837881659527793e715",
    "src/components/wizard/Step2FilingType.tsx": "cde8f25409cda6bc0de494c3c9da536f5209e13612d02f579c5d695b812cd660",
    "src/components/settings/ProfileSettings.tsx": "96f84fd34bda7ef31455a5055070895f3b9adc50f2eb3faa973c12a272106085",
    "src/lib/advice.ts": "e1c668797d34b6b4c1b79c741c9037b5e7570011667231d37545d497a9b89bae",
    "src/components/settings/SettingsPage.tsx": "abf552d6fc9435e63e9a49445e65847f25ba2770268680d3665dcd2cebbf0472",
    "src/components/reports/ReportsPage.tsx": "94edb252f8d7935d495184b2dc78dde7d8bb3f58579ba2075485f73775c5f334",
    "src/App.tsx": "0a040c8ca6a7d29fd0371f75482edfacaac3ef914d7b6d1ca1355830840856d4",
    "vite.config.ts": "a7dbb7eabc0091a46acf7be3df886ed5a919cc5b707b6dfaef385b7a14be7d28",
}
for name, expected in hashes.items():
    if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
        raise SystemExit(f"Unsupported upstream file: {name}")

subprocess.run([sys.executable, str(here / "assemble_db.py"), str(root)], check=True)
shutil.copytree(here / "overlay", root, dirs_exist_ok=True)
shutil.copy2(here / "backup.ts", root / "src/lib/full-backup.ts")
shutil.copytree(here, root / "browser-adapter", dirs_exist_ok=True,
                ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))

path = root / "src/lib/export.ts"
source = path.read_text().split("// ===== .wakerun (JSON) =====", 1)[0]
source = source.replace('import { getDb, listTransactions } from "@/lib/db";', '''import { listTransactions } from "@/lib/db";
import { lockedDatabase, withDatabase } from "@/lib/browser-sql";
import { makeBackup, restoreBackup } from "@/lib/full-backup";''')
source += '''
// Browser v2 backups contain every accounting table and sensitive settings.
export async function exportWakerunFile(): Promise<void> {
  const data = await withDatabase(async () => makeBackup(await lockedDatabase()));
  downloadBlob(new Blob([data], { type: "application/json;charset=utf-8" }),
    `wakerun-backup-${new Date().toISOString().slice(0, 10)}.wakerun`);
}

export async function importWakerunFile(jsonText: string): Promise<void> {
  await withDatabase(async () => restoreBackup(await lockedDatabase(), jsonText));
}
'''
path.write_text(source)

path = root / "src/lib/ai.ts"
source = path.read_text()
source = source.replace('import { fetch } from "@tauri-apps/plugin-http";\n', "", 1)
path.write_text(source)

path = root / "src/components/settings/BackupSettings.tsx"
source = path.read_text()
source = source.replace("  exportTransactionsCsv,\n", "  downloadBlob,\n  exportTransactionsCsv,\n", 1)
source = source.replace("  listBackupFiles,\n", "  listBackupFiles,\n  readBackup,\n", 1)
source = source.replace(
    "  async function handleReset() {",
    '''  async function downloadSavedBackup(name: string) {
    try {
      const data = await readBackup(name);
      downloadBlob(new Blob([data], { type: "application/json;charset=utf-8" }), name);
    } catch (e) {
      toast("バックアップの取得に失敗しました", {
        tone: "error", description: e instanceof Error ? e.message : String(e),
      });
    }
  }

  async function handleReset() {''', 1)
source = source.replace(
    "Wakerunのデータはこの PC の中だけに保存されています。PC が壊れたり買い替えたりする時に備えて、定期的にバックアップを取りましょう。",
    "データはこのブラウザに保存され、ブラウザのデータ消去や端末の故障で失われます。バックアップには個人番号や AI の API キーが含まれる場合があり、暗号化されません。安全な場所に保管してください。", 1)
source = source.replace(
    "アプリ起動時に 1 日 1 回、Wakerun のデータをアプリ内のフォルダに自動保存します。最新 3 世代まで保持。",
    "アプリ起動時に 1 日 1 回、このブラウザに自動保存します。最新 3 世代まで保持。ブラウザデータを消去すると一緒に失われます。", 1)
source = source.replace(
    "すべてのデータを丸ごと保存します。別の PC への引っ越しにも使えます。",
    "このブラウザ版のデータを保存します。バックアップには秘密情報も含まれます。", 1)
source = source.replace(
    "以前書き出した .wakerun ファイルからデータを復元できます。今のデータは上書きされます。",
    "このブラウザ版から書き出したバックアップのみ復元できます。元の Wakerun のバックアップには対応していません。今のデータは上書きされます。", 1)
needle = '''                  {f.isAuto && (
                    <span className="text-[10px] text-primary bg-primary-soft px-1.5 py-0.5 rounded">
                      自動
                    </span>
                  )}'''
replacement = needle + '''
                  <button type="button" onClick={() => downloadSavedBackup(f.name)}
                    className="underline" aria-label={`${f.name} をダウンロード`}>
                    ダウンロード
                  </button>'''
if source.count(needle) != 1:
    raise SystemExit("Upstream backup list changed")
path.write_text(source.replace(needle, replacement, 1))

# Upstream incorrectly calls the deduction amount a reduction in tax itself.
# These wording fixes do not constitute a full tax-year or accounting review.
for name in ["src/components/wizard/Step2FilingType.tsx", "src/components/settings/ProfileSettings.tsx"]:
    path = root / name
    source = path.read_text().replace("65万円の税金の割引つき", "所得から最大65万円を控除（要件あり）")
    source = source.replace("10万円の税金の割引つき", "所得から最大10万円を控除（要件あり）")
    source = source.replace("税金の割引はありません", "青色申告特別控除はありません")
    source = source.replace("帳簿はWakerunが自動でつけるので、あなたがやることは変わりません。一番おトクです。", "帳簿や電子申告など、65万円の控除には所定の要件があります。")
    source = source.replace("シンプルな帳簿でOK。それでも 10 万円分の税金の割引が受けられます。", "要件を満たすと、所得から最大10万円を控除できます。")
    source = source.replace("一番かんたんな申告方法ですが、青色申告のような割引は受けられません。", "青色申告特別控除は適用されません。")
    path.write_text(source)

path = root / "src/lib/advice.ts"
source = path.read_text()
source = source.replace("帳簿をしっかりつけていますね！青色申告に切り替えると、最大65万円の税金の割引が受けられます。", "青色申告特別控除は、要件を満たす場合に所得から最大65万円を控除できます。")
source = source.replace("国民健康保険や年金の支払い、記録していますか？確定申告で税金の割引になります。", "国民健康保険や年金の支払いは、要件を満たす場合に所得控除の対象になります。")
source = source.replace("iDeCo（個人型確定拠出年金）を使うと、掛け金が全額「税金の割引」になります。フリーランスは月額 6.8 万円まで加入できます。", "iDeCo の掛金は、要件を満たす場合に所得控除の対象になります。加入要件と掛金の上限をご確認ください。")
source = source.replace("小規模企業共済に加入すると、掛け金が全額「税金の割引」になり、将来は退職金として受け取れます。", "小規模企業共済の掛金は、要件を満たす場合に所得控除の対象になります。加入条件と受取条件をご確認ください。")
path.write_text(source)

path = root / "src/App.tsx"
source = path.read_text().replace('''      <ToastContainer />''', '''      <div role="status" className="bg-amber-100 px-4 py-3 text-sm text-amber-950">
        試用版です。税計算は2024年基準で最新の税制に対応していません。実データを入力せず、申告には使用しないでください。
        {" "}<a className="underline" href="/source.html">ソースコード</a>
      </div>
      <ToastContainer />''', 1)
source = source.replace('''        await runAutoBackup();''', '''        const backup = await runAutoBackup();
        if (backup.error && !cancelled) {
          toast("自動バックアップに失敗しました", { tone: "error", description: backup.error });
        }''', 1)
path.write_text(source)

path = root / "vite.config.ts"
source = path.read_text().replace('import react from "@vitejs/plugin-react";',
    'import react from "@vitejs/plugin-react";\nimport { resolve } from "node:path";', 1)
source = source.replace('  plugins: [react()],', '''  plugins: [react()],
  build: { rollupOptions: { input: {
    index: resolve(process.cwd(), "index.html"),
    source: resolve(process.cwd(), "source.html"),
  } } },''', 1)
path.write_text(source)

# React Router 7 resolves relative links differently under nested splats.
path = root / "src/components/settings/SettingsPage.tsx"
source = path.read_text().replace('to={item.to}', 'to={`/settings/${item.to}`}')
source = source.replace('<Navigate to="profile" replace />', '<Navigate to="/settings/profile" replace />')
path.write_text(source)
path = root / "src/components/reports/ReportsPage.tsx"
source = path.read_text().replace('to={t.to}', 'to={`/reports/${t.to}`}')
source = source.replace('<Navigate to="monthly" replace />', '<Navigate to="/reports/monthly" replace />')
path.write_text(source)
print("Applied browser SQLite and full backup adapter; end-to-end verification remains required")
