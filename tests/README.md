# tests/

This directory contains two categories of test assets:

1. **Fixtures** — capsule directories consumed by `ato-cli` and `nacelle` test harnesses
2. **Manual test suites** — pre-release verification scripts (`tests/manual/`)

## Fixtures

Minimal capsule fixtures for CI integration tests. Not meant as "how to use ato" examples.

| Fixture | Purpose |
|---|---|
| `test-concurrent-a`, `test-concurrent-b` | Concurrent capsule execution (§3 parallel download lock) |
| `test-sandbox` | Sandbox boundary enforcement (§4) |

These are consumed by `apps/ato-cli` and `apps/nacelle` test harnesses.

## Manual Test Suites (`tests/manual/`)

15 pre-release verification suites covering the human-testable axes that CI cannot cover. See [`tests/manual/README.md`](manual/README.md) for full documentation.

```bash
# Run all suites
./tests/manual/run-all.sh

# Run a single suite
bash tests/manual/04-sandbox-boundary/test.sh
```

| Suite | Section |
|---|---|
| `01-install-upgrade` | §1 インストール / アップグレード経路 |
| `02-gpu-accelerator` | §2 実機での GPU / アクセラレータ系 |
| `03-first-run-download` | §3 5GB 級モデルの初回ダウンロード UX 🔴 |
| `04-sandbox-boundary` | §4 サンドボックス境界の実測 |
| `05-cross-os` | §5 クロス OS の挙動差検証 |
| `06-share-url` | §6 Share URL の実 URL 配布フロー 🔴 |
| `07-ato-desktop-ux` | §7 ato-desktop の実 UX |
| `08-trust-ux` | §8 Trust UX の実体験 |
| `09-network-isolation` | §9 ネットワーク隔離の実観測 |
| `10-error-messages` | §10 エラーメッセージとデバッグ体験 |
| `11-ato-store` | §11 ato-store の実運用テスト |
| `12-toolchain-interference` | §12 既存ツールチェーンとの干渉 |
| `13-longtail-envs` | §13 ロングテールの環境 |
| `14-doc-alignment` | §14 ドキュメンテーションとの整合 |
| `15-dogfooding` | §15 リリース前 dogfooding |

🔴 = release blocker — must be verified with ≥10 external testers
