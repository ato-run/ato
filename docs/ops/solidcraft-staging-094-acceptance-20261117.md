# SolidCraft — 公開 CLI 0.9.4-stg のクリーンインストール受け入れ（2026-11-17）

Status: Current — **合格**。macOS arm64 の公開 installer → CLI → D1 → D2 → D2 cache hit を実測。本番反映なし。

## 配布経路の修正

[v0.9.4-stg](https://github.com/ato-run/ato/releases/tag/v0.9.4-stg) を GitHub prerelease / latest=false として公開した。
今回の配布対象は macOS arm64。通常の stable latest は `v0.9.3` のまま。
専用 staging release workflow は `v*-stg` のみに反応し、version と stg の ancestry を検査する。
通常の release workflow はこのタグを除外し、Homebrew・production の経路には流さない。
CLI の runtime 実装は既存の M1 対応を含み、今回の変更は version と配布経路。

PWA build は staging の installer だけ既定版を `0.9.4-stg` に固定する。
shell / PowerShell installer の legacy nacelle 取得は既定で無効とし、明示的な opt-in は維持した。
production build の既定は latest のまま。Windows / x64 の配布・実機成功を主張するものではない。

- ato #1888 → #1889: CLI version / staging release workflow。
- ato-pwa #1022 → #1023: installer / staging 固定。
- [release workflow](https://github.com/ato-run/ato/actions/runs/44041494599): build・公開とも成功。
- 配信 installer: `https://stg-app.ato.run/install.sh`。

## 固定した配備

| 対象 | 値 |
| --- | --- |
| CLI release source | `6b16e9e3998c4f4477461aa372fb2d314cadc6e0` |
| CLI identity | `0.9.4-stg` / `release` / `git_dirty=false` |
| PWA source | `69316be2e92ea3fb77f1ce56338fad6fbadfa051` |
| PWA Worker version | `53d5fb21-9efa-4cbf-b3b3-22df151697f2` |
| API Worker version | `fcb9b553-6768-4226-9580-9c5d2a0de5e9` |
| CLI archive | `sha256:cfa8afddc1114ed68104fc64747616f45a70e5354de795a40ec10582d50dace0` |
| Installed CLI | `sha256:a393bb2fecdd2569d6b33b9f5610733f0f56dee34863158ac446ff9ecdcc2a51` |
| Served installer | `sha256:0a85d1856ce51ca19fdab4cb7d5a951709bed81542c9fd4f622d405b46b47988` |
| bundle | `sha256:35f940038bc66cec78eb53485738bf029d4ce68d619993a024f8a3a10c10c7e3` |
| Contract | `sha256:34d52388678111292fa890603110c23b47ce51220f452a28c233a2259e174c98` |
| D1 | `sha256:f58cdca4697158f519a9a24d7bc36a7deb00a531b913fce3859deccbb46551e9` |
| D2 | `sha256:382f1474eb77e0c18820f65a4d96edebfeddda6cc6b2b1cb5f4834bb9462406d` |

公開 archive の SHA-256 は GitHub asset digest と一致し、展開した binary はインストール済み CLI と一致。
PWA は merge 済み clean stg から通常の `npm run deploy:staging` で配備し、配信 asset・API origin・source marker を検査した。
共有 lease 下で配備から実機検証まで直列に実行し、API/PWA の active version は実機検証前後で一致。
lease 解放と手順全体の exit 0 を確認。API 再配備、migration、feature flag 変更はない。

## クリーンインストールと結果

Apple M1 / macOS 15.8.1 上で HOME、ATO_HOME、XDG config/cache、Cargo/Rustup home、TMPDIR を新規作成した。
PATH は新規 bin と OS 標準に限定し、既存 CLI、認証情報、Rust、registry、native cache は再利用していない。
OS と Apple command-line tools を再インストールした検証ではない。

配信された script を変更せず `sh install.sh --cli-only --no-modify-path` で実行。
version や nacelle skip の環境変数は上書きせず、**32.308 秒・exit 0**。
公開 Hub の上記 digest 固定参照に対して D1、D2、D2 再実行を順番に実施した。
`--accept-permissions --no-open --verification-receipt` を指定し、D2 のみ既承認の
`--allow-native-source-build --allow-unsigned-native` を加えた。API origin は staging に限定。

| Run | 所要時間 | attempt | 署名判断 | fully_satisfied |
| --- | --- | --- | --- | --- |
| d1 | 142.355 秒 | `run-7879-1794902738644509000` | `developer_id` / override `false` | true |
| d2 | 2624.8 秒 | `run-8375-1794902882266642000` | `adhoc` / override `true` | true |
| d2-second | 260.492 秒 | `run-62684-1794905509338983000` | `adhoc` / override `true` | true |

三つの receipt は schema `/5`、同一 bundle・Contract、明示した D、異なる attempt を記録した。
各 `/initialize` 観測は `satisfied`。観測 byte 数と body digest は証跡 JSON に保存した。
D1 は `require-valid`、D2 は `require-valid-or-local-override`。codesign は valid、Gatekeeper の
standalone CLI に対する rejected は記録済みであり、notarization/stapling の成功とは扱わない。
三つの process group は終了・回収済み。

D2 の初回は managed Rust 1.95.0 と crates を取得し、新しい source tree / target / build profile を使った。
採用した artifact は **190,961,200 bytes**（512MiB 上限は変更なし）。
再実行は **cache hit**。artifact `sha256:a5fd275fee70d8c62a0823d9cb18291090bc15f6f299c38b44cc724c353ab9be` と cache key `sha256:b728ed78e125495d60c85b56e938cf9933a506bf5b2e02ae52002dcc3cce1212` を確認した。
初回・再実行・cache manifest の artifact digest は一致し、再実行では source tree と build profile が未作成。
キャッシュ証跡は二回目の receipt hash と attempt に結び付く。所要時間だけを hit の根拠にしていない。
`/5` receipt に build_cache field を追加したものでもない。
現在の実装では cache hit でも source archive の取得は発生する。

通常の CLI Run 報告によって staging の run_count は **3 → 6**。
bundle digest は変わらず、count や verification の直接更新はしていない。
本番 installer の bytes は実行前後で一致し、stable latest は `v0.9.3` のまま。
main へのマージ・production deployment・production データ更新は行っていない。

## 検証と証跡

- installer / stamp テスト 5、PWA typecheck / staging build、既存 deploy テスト 13、shell syntax / diff check が成功。
- CLI Cargo metadata / cargo-dist plan が成功。公開 release workflow の macOS arm64 build が成功。
- runtime 実装は今回変更なし。通常 Rust CI には既存の format / Linux PTY / Windows の失敗が残り、CI 全体成功とは扱わない。
- receipt / cache 検査には既存 `scripts/acceptance/verify-solidcraft-native.sh` 内の Python validator をそのまま使用した。

[機械可読証跡](evidence/solidcraft-staging-094-acceptance-20261117.json) に release manifest、各 receipt の
元 bytes の hash と path を置換した投影、empty-directory 条件、所要時間、cache / 配備検査を保存した。
生ログと元 receipt は workspace `.tmp/sc-094-d2j3x07i/`、制御ログは
`apps/ato/.worktrees/m2-terminal/.tmp/stg-cli-release/` に保持する。

[v0.9.3 での失敗記録](solidcraft-staging-clean-install-20261117.md) と
[ローカル開発 CLI での先行受け入れ](capsule-hub-m2-acceptance-20261116.md) を履歴として維持する。
これは CLI-local native terminal の受け入れであり、Hosted Native / GUI viewer / Windows の受け入れではない。
