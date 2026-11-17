# macOS arm64 CLI インストール検証（Terminal で実行する手順）

Status: Current（2026-11-17 時点。DSH のサンドボックスでは Homebrew の書き込みが拒否されるため、この手順だけは通常の Terminal.app で実行する）

## 前提

- `brew install` は `~/.homebrew/trust.json.lock` 等へ書き込むため、workspace 外の書き込みが許可された通常の Terminal で実行する。
- 隔離属性の強制削除（`xattr -d com.apple.quarantine`）や Gatekeeper の無効化は**行わない**。

## 手順

```sh
# 1) Formula の内容と SHA256 を確認（v0.9.3 arm64 の実測値: 6f808611591bb4da63f230a0c0ea562aedbecc1da2e252cfa65bdfb3004b9079）
brew info ato-run/ato/ato-cli

# 2) インストール（個別の信頼が必要な場合のみ --formula で信頼する。tap 全体は信頼しない）
brew install ato-run/ato/ato-cli
# 必要時: brew trust --formula ato-run/ato/ato-cli

# 3) バージョンとビルド同一性
ato --version
ato version --json

# 4) 診断（process-identity が pass すること。fail する場合はホスト側の制約）
ato doctor --json

# 5) 既製 Capsule の実行（静的 Web、receipt が fully_satisfied: true になること）
mkdir -p ~/ato-check && cd ~/ato-check
ato run <path-to>/samples/interop-static.capsule --no-open --verification-receipt receipt.json
python3 -c "import json;print(json.load(open('receipt.json'))['fully_satisfied"])"
```

## 記録すること

- OS/arch、`ato version --json` の commit、`ato doctor --json` の全チェック、receipt の `bundle_sha256`/`contract_ref`/`derivation_ref`/`fully_satisfied`
- インストール前後で Gatekeeper の警告が出たか（出た場合は無効化せず、そのまま記録する）
- アンインストール手順: `brew uninstall ato-run/ato/ato-cli`。`~/.ato` のデータは削除されない
