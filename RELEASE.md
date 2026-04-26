# RELEASE

このドキュメントは `capsuled-dev` ワークスペース配下の各アプリのリリース手順をまとめたものです。

---

## ato-cli リリース手順

`apps/ato-cli` は独立した git リポジトリで、GitHub Actions (`release-plz` + `cargo-dist`) を使ったリリースフローを採用しています。

### 前提

- `gh` CLI がインストール済み・認証済みであること
- `apps/ato-cli/` で作業すること（独立 git リポジトリ）
- バージョン衝突時はパッチバンプして新バージョンで再リリース（既存タグを再利用しない）

### Pre-flight

```bash
cd apps/ato-cli
git branch --show-current    # main であることを確認
git status --short           # ワークツリーがクリーンであることを確認
```

### Step 1: main に変更を反映

```bash
# dev ブランチ経由または直接 main へコミット・マージ
git push origin main
```

### Step 2: release-plz で Release PR を作成

`release-plz.yml` は **週次（月曜 00:00 UTC）** 自動実行されるが、手動ディスパッチも可能:

```bash
env -u GH_TOKEN -u GITHUB_TOKEN gh workflow run release-plz.yml \
  --ref main -f command=release-pr
```

`chore(ato-cli): release vX.Y.Z` というタイトルの PR が自動作成される。

### Step 3: PR チェックの監視とマージ

PR 上のチェック対象: **Clippy**・**Security Audit** (cargo-audit, cargo-deny)・**Release/plan**

```bash
# チェック状況確認（~5–7 分）
env -u GH_TOKEN -u GITHUB_TOKEN gh pr checks <pr>

# 全チェック green 後にマージ（ブランチポリシーには --admin が必要な場合がある）
env -u GH_TOKEN -u GITHUB_TOKEN gh pr merge <pr> --merge --delete-branch=false --admin

# マージコミット SHA を取得
env -u GH_TOKEN -u GITHUB_TOKEN gh pr view <pr> --json mergeCommit
```

### Step 4: main ポストマージの確認

```bash
# Security Audit が green になるまで待つ（~2–3 分）
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --commit <merge-sha> \
  --json databaseId,status,conclusion,workflowName
```

### Step 5: バージョン確認とタグ付け

```bash
# origin/main の version を確認
git fetch origin main
git show origin/main:Cargo.toml | grep '^version'

# タグが未存在であることを確認
git tag -l 'vX.Y.Z'
env -u GH_TOKEN -u GITHUB_TOKEN gh release view vX.Y.Z 2>&1

# アノテーテッドタグを作成・push（これが release.yml を起動する）
git tag -a vX.Y.Z <merge-sha> -m "ato-cli vX.Y.Z"
git push origin vX.Y.Z
```

> タグ push が `release.yml` を起動する。PR モードでは publish しない。

### Step 6: リリース発行の監視

```bash
# ワークフロー全体の状態
env -u GH_TOKEN -u GITHUB_TOKEN gh run list --limit 5 \
  --json databaseId,status,conclusion,workflowName,displayTitle

# ジョブ単位の詳細（build-local-artifacts が最も時間がかかる）
env -u GH_TOKEN -u GITHUB_TOKEN gh api \
  repos/ato-run/ato-cli/actions/runs/<run-id>/jobs --paginate \
  --jq '.jobs[] | [.name, .status, (.conclusion // "")] | @tsv'

# 完了確認（host ジョブ完了 → GitHub Release 公開）
env -u GH_TOKEN -u GITHUB_TOKEN gh release view vX.Y.Z \
  --json name,tagName,isDraft,isPrerelease,publishedAt,url,assets
```

### 所要時間の目安

| フェーズ | 時間 |
|---------|------|
| Release PR チェック | ~5–7 分 |
| マージ後 Security Audit | ~2–3 分 |
| リリースビルド（4 platform） | ~15–20 分 |
| **合計** | **~25–30 分** |

### バージョン衝突時の対処

タグまたは GitHub Release がすでに存在する場合は **再利用しない**。

```bash
# 次パッチバンプブランチを作成
git checkout -b release-bump-vX.Y.(Z+1)
# Cargo.toml / Cargo.lock のバージョンをバンプ
# CHANGELOG.md に新バージョンセクションを追加
git commit -m "chore: bump release to vX.Y.(Z+1)"
git push origin release-bump-vX.Y.(Z+1)
env -u GH_TOKEN -u GITHUB_TOKEN gh pr create --base main \
  --head release-bump-vX.Y.(Z+1) \
  --title "Bump release to vX.Y.(Z+1)" \
  --body "Previous vX.Y.Z already tagged. Bump patch to unblock release."
```

マージ後、Step 2（release-plz dispatch）から再開する。

---

## nacelle リリース手順

`apps/nacelle` も独立 git リポジトリ。フローは ato-cli と同様だが release-plz を使わない:

1. 変更を `main` にマージ
2. `Cargo.toml` / `Cargo.lock` のバージョンを手動バンプしてコミット
3. アノテーテッドタグを作成・push
4. `Build and Publish nacelle` ワークフローが起動してリリース発行

---

## その他のアプリ（Web / Workers）

`apps/ato-api`, `apps/ato-web`, `apps/ato-play-*`, `apps/ato-proxy-edge` のデプロイは
`AGENTS.md` の「Web / Workers Build & Deploy」セクションを参照。
