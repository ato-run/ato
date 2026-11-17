# 引き継ぎ: Capsule Hub M2 — SolidCraft 2D の実機検証（2026-11-17 14:20 JST）

Status: Draft（引き継ぎ用。作業途中の状態をそのまま記録）
親 Issue: ato-run/ato #1819（重複 Issue を作らない）
実装契約: `docs/rfcs/draft/capsule-hub-m2-native.md`
受け入れサマリ: `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md`

## 1. 目的（この作業が完了したと言える条件）

1. M1（macOS arm64）実機で `capsules/ato/solidcraft` の **2 つの D** が動くこと
   - D1 = Pre-built Native（上流 v0.43.0 の zip を digest 固定で参照、実行時に取得）
   - D2 = Source-built Native（commit `8ba46e9e2181396a9b5903b98031399aef61cf26` からローカルビルド）
2. 両 D の **Receipt が同一 `contract_ref`**、D ごとに異なる `derivation_ref`、`fully_satisfied: true`
3. ビルド成功と K 充足が分離されている（起動 → terminal exchange で K を観測）
4. 2 回目の D2 実行が **内容ハッシュ cache hit** になること
5. 受け入れ記録を更新し、本番公開の可否を別承認で判断する（**本番は未実施**）

## 2. 到達済み（dev / stg マージ済み）

### コード
| 領域 | PR | 内容 |
| --- | --- | --- |
| ato-api | #1287/#1288 | 公式 publisher 限定 1GB publish（migration 0419、presigned R2、complete 検証、sweep） |
| ato-api | #1289/#1290, #1291/#1292 | 大容量 publish の受け入れ記録と再現スクリプト |
| ato | #1824/#1825 | `ato.native@1` の remote digest 参照・Source-backed Native D・managed Rust toolchain・Adapter 監査 |
| ato | #1830/#1831 | remote native artifact の zip 展開 |
| ato | #1838/#1839 | v2 native terminal 観測（probe port 不要、`ato.contract.terminal@1`、`lib/pty-host`） |
| ato | #1840/#1841 | 宣言 argv（`ato.native.argv`、16/256B/printable、直接 exec） |
| ato | #1842/#1843 | 複数 terminal route が同一 Contract を実現 |
| ato | #1844/#1845 | SolidCraft 2D サンプル（`samples/solidcraft`） |
| ato | #1846/#1847 | staging 公開記録 |
| ato | #1848/#1849 | argv の descriptor 往復テスト |
| ato | #1850/#1851 | 実機検証スクリプト `scripts/acceptance/verify-solidcraft-native.sh` |
| ato | #1852/#1853 | M2 受け入れサマリ |
| ato | #1826/#1827 | multi-D 明示選択 UI・`--list-derivations` |
| ato | #1828/#1829 | 大容量 publish/download のストリーミング |
| ato | #1854/#1855 | ローカル実行者が native terminal surface を所有（admission 修正） |
| ato | #1856/#1857 | terminal route は URL を要求しない |
| ato | #1858/#1859 | ユーザーが受理した declared requirements を認可として運ぶ |
| ato | #1860/#1861 | run Adapter の egress 拒否は runtime phase 限定 |
| ato | #1862/#1863 | tar の pax メタデータヘッダを読み飛ばす |
| ato | #1865/#1866 | tar の symlink を root 内コピーとして配置 |
| ato | #1867/#1868 | source tree の展開上限を runtime artifact と分離（4GiB / 400k） |
| ato | #1869/#1870 | build sandbox に remote 443 + resolver port の grant を追加 |
| ato | #1836/#1837 | publish metadata の null 送信バグ修正 |
| ato-pwa | #1006/#1007 | Hub 詳細ペインの「実行方法」一覧 |
| ato-pwa | #1012/#1013 | staging デプロイ証跡 |

### staging の実測（完了済み）
- 大容量 publish: `scripts/staging/capsule_hub_m2_large_publish_e2e.sh` が **11/11 pass**
- `capsules/ato/solidcraft` 公開済み: digest `sha256:1f1895ec…`、2 derivations、`native_terminal`、`macos/aarch64`、
  `verification=experimental / 0 runs`
- `https://stg-app.ato.run/capsules/ato/solidcraft` は 200 で「実行方法」2 行を表示
  （Worker `ato-pwa-staging` / version `7f4a720c-cad9-4098-a43d-1985348c2816`、source `origin/stg`）
- D1 は M1 実機で **成功**（`ok: pre-built Derivation ran`）

## 3. いま詰まっている問題（ここが引き継ぎ対象）

### 症状（D2）
```
[formation] attempt failed: native_build_failed: cargo exited with exit status: 101:
    Updating crates.io index
warning: spurious network error (3 tries remaining): [6] Couldn't resolve host name (Could not resolve host: index.crates.io)
error: failed to get web-time as a dependency of package solidcraft-doc ...
Caused by: [6] Couldn't resolve host name (Could not resolve host: index.crates.io)
```

### 原因の理解
- source build は Seatbelt 内で `cargo build --locked --release` を実行する（`apps/portable-application/src/native_desktop/source_build.rs` の `SandboxedCargo`）。
- `SandboxPolicy` は `allow_tcp_connect([443])` を使っており、SBPL レンダラは接続先を **loopback 限定**で出力していた
  → 外向き HTTPS が通らない。
- #1869 で `allowed_connect_remote_tcp_ports`（port スコープの remote grant）を追加し、
  `(allow network-outbound (remote tcp "*:443"))` と resolver（udp/tcp 53）を出すようにしたが、**DNS はまだ通らない**。
- macOS の名前解決は mDNSResponder との **Unix socket**（`/var/run/mDNSResponder`）経由で、
  port スコープの grant ではこの socket 接続が許可されないため。

### 未コミットの試行（worktree `apps/ato/.worktrees/m2-terminal`、`git status` に残っている差分）
`git status` の差分:
- `lib/sandbox/src/macos.rs`: remote grant があるとき resolver socket の literal 許可と `/private/var/run` の metadata 許可を追加
- `apps/portable-application/src/native_desktop/source_build.rs`: build policy を `.with_network(true)` に変更
  （Seatbelt では host 名フィルタ不可のため、**ビルドだけ** network を開ける。ファイル/状態/プロセスの封じ込めは維持）
- `lib/formation/src/capsule_toml_v2.rs`: **無関係な rustfmt 差分**（コミットしないこと。`git checkout --` で戻す）

ビルドとテストは通っている:
- `cargo test -p ato-sandbox` → 26 passed（新規 `a_build_egress_grant_renders_the_port_and_the_resolver` 含む）
- `cargo test -p ato-portable-application --lib native_desktop` → 76 passed

**まだ PR 化・マージ・CLI 再ビルドをしていない。**

## 4. 次にやること（順番）

1. 未コミット差分を整理する
   ```sh
   cd ~/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-terminal
   git checkout -- lib/formation/src/capsule_toml_v2.rs   # 無関係な fmt 差分
   git add lib/sandbox/src/macos.rs apps/portable-application/src/native_desktop/source_build.rs
   git commit -m "fix(sandbox): open the host resolver to a source build"
   ```
2. **方式の判断**（重要。どちらかを選ぶ）
   - (A) 今回の差分どおり **build sandbox に network を許可**（シンプル・確実。ただし host 名フィルタ無しを Audit に明記）
   - (B) **依存取得とビルドを分離**: ホスト側で `cargo fetch --locked`（network あり・書き込みは Ato 管理の CARGO_HOME のみ）→
     Seatbelt 内で `--offline` ビルド。封じ込めは強いが実装量が多い
3. マージ手順（dev → stg、本番はしない）
   ```sh
   git push -u origin fix/build-egress-and-resolver
   gh pr create --base dev --head fix/build-egress-and-resolver --body-file <body>
   gh pr merge <n> --merge --admin --delete-branch=false
   gh pr create --base stg --head dev --title "release: promote dev to staging (...)"
   gh pr merge <n> --merge --admin --delete-branch=false
   ```
4. CLI 再ビルド（ユーザーの検証スクリプトが使うバイナリ）
   ```sh
   cd ~/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-streaming
   CARGO_TARGET_DIR=$PWD/target git fetch origin --quiet && git switch --detach origin/dev
   CARGO_TARGET_DIR=$PWD/target cargo build -p ato-cli --bin ato
   ```
5. ユーザーに実機で再実行してもらう（下記 §5）
6. 成功したら受け入れ記録を更新（§6）し、本番公開の可否を相談

## 5. 実機検証（ユーザーの Mac、Seatbelt が使える端末）

```sh
cd ~/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-terminal
ATO=/Users/egamikohsuke/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-streaming/target/debug/ato \
  scripts/acceptance/verify-solidcraft-native.sh
```

- スクリプトは pack → `--list-derivations` から prebuilt/source を自動判別 → D1 → D2 → D2 再実行（cache）→ receipt 検査。
- 出力は `.tmp/solidcraft-verify/`（`d1.log`, `d2.log`, `d2-second.log`, `*-receipt.json`）。
- 失敗時はユーザーに `.tmp/solidcraft-verify/d2.log` を貼ってもらう。
- 注: D1 の成功時点で `d1-receipt.json` ができている。

## 6. 成功後の作業

1. 両 receipt の確認: 同一 `contract_ref` / 別 `derivation_ref` / `fully_satisfied: true` / schema `/5` /
   evidence に `path`・`body_sha256`・`captured_bytes` / D2 は `build_network` と 2 回目 cache hit
2. `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md` に実機結果を追記
3. Hub の run_count と検証状態（experimental → verified）を確認
4. 見つかった制約を `docs/rfcs/draft/native-desktop-adapter-audit.md` に追記
   （build sandbox の network grant、host 名フィルタ不可、resolver socket）
5. 本番デプロイ・本番公開は**別承認**（現時点で未実施）

## 7. 想定される次の関門

- **toolchain 取得**: `static.rust-lang.org` から rustup-init（SHA-256 検証）→ `$ATO_HOME/toolchains/rust/1.95.0-aarch64-apple-darwin`。
  ここはホストプロセスが取得する（sandbox 外）ため通る見込みだが、初回は時間がかかる。
- **ビルド時間**: solidcraft は大規模。`build_argv` は `cargo build --locked --release -p solidcraft-cli`。
- **成果物採用**: `macos.rs` の `NativeArtifactPayload::BuiltTree` → `artifact::adopt_tree` は **artifact 上限（512MiB）**を使う。
  ビルド成果物の staging dir が大きいと `the built tree is larger than 536870912 bytes` で落ちる。その場合は source と同様に上限分離が必要。
- **receipt / launch**: 起動後に `initialize` exchange（input は 59 bytes の JSON-RPC、delimiter は `serverInfo`）が成立するか。
- **cache**: 2 回目実行で `build_cache: hit` になるか（key は recipe/source digest/commit/lock/toolchain/target/argv/entry）。

## 8. 環境の癖（必読）

- **このエージェント環境では native 実行ができない**: sandbox が macOS Seatbelt の適用を拒否する
  （`sandbox-exec: sandbox_apply: Operation not permitted`）。実機検証はユーザーの端末でしかできない。
- **worktree は独立**: 共有 `CARGO_TARGET_DIR` を使うと別 worktree の rmeta が混線する。検証は worktree ごとの `target/`。
  現在の役割:
  - `apps/ato/.worktrees/m2-terminal` … 今回の修正作業（dev 起点）
  - `apps/ato/.worktrees/m2-streaming` … ユーザーが使う CLI バイナリ（`target/debug/ato`、dev からビルド）
  - `apps/ato/.worktrees/m2-solidcraft` … サンプル（既に dev にマージ済み）
- **ato の主チェックアウトは nightly**: `apps/ato/scripts/acceptance/verify-solidcraft-native.sh` は**存在しない**。
  dev ベースの worktree から実行すること。
- **Cloudflare**: `.tmp/cf_token`（0600、IP 制限あり）を `CLOUDFLARE_API_TOKEN` に入れ、
  `NODE_OPTIONS=--dns-result-order=ipv4first` を併用。IP 不一致（`9109`）や D1 権限不足（`7403`）のときは
  `tailscale set --exit-node=100.86.119.10` で egress を合わせる。
- **staging 共有 lease**: `ato-api/scripts/staging-deploy-locked.mjs`。失敗すると lease が残る。
  自分が取得した行だけを D1 から削除して復旧する（他者の lease は触らない）。
- `wrangler` は sandbox で `~/Library/Preferences/.wrangler/logs` に書けず EPERM を出すことがある。
  HOME を workspace 内に置くと回避できる。

## 9. 参照

- 実装契約: `docs/rfcs/draft/capsule-hub-m2-native.md`（§8 に以前のブロッカー記録、§9 に状態）
- Adapter 監査: `apps/ato/docs/rfcs/draft/native-desktop-adapter-audit.md`
- サンプル: `apps/ato/samples/solidcraft/`（`capsule.toml`, README, `box-measure.json`）
- 検証スクリプト: `apps/ato/scripts/acceptance/verify-solidcraft-native.sh`
- staging 受け入れ: `apps/ato-api/docs/ops/capsule-hub-m2-large-publish-20261116.md`
- staging PWA: `apps/ato-pwa/docs/ops/capsule-hub-m2-staging-deploy-20261117.md`
- 受け入れサマリ: `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md`
- 親 Issue: https://github.com/ato-run/ato/issues/1819
