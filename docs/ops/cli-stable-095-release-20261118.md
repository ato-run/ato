# CLI 0.9.5 — stable 公開記録（2026-11-18）

Status: Current — stable release の公開、全配布 asset のハッシュ、公開 installer からのクリーン導入、Homebrew formula の更新を確認。

## 公開範囲

[v0.9.5](https://github.com/ato-run/ato/releases/tag/v0.9.5) は draft/prerelease ではなく、GitHub の latest release。
macOS arm64/x64、Linux arm64/x64、Windows x64、Windows MSI、shell installer を公開した。
[Release workflow](https://github.com/ato-run/ato/actions/runs/44149093428) と Homebrew 更新の結果は機械可読証跡に固定する。
Web/API の production deploy、migration、flag 変更は行っていない。
本番 `ato.run/install.sh` の配信 bytes は前後で一致する。staging の installer は既存の 0.9.5-stg のまま。

## ソースと機能

- source / annotated tag の参照先: `8ee783ba317f2233f1de8694998cc3e051ff0c0b`。
- version identity: `0.9.5 / release / git_dirty=false`。
- [#1902](https://github.com/ato-run/ato/pull/1902) → [#1903](https://github.com/ato-run/ato/pull/1903) → [#1904](https://github.com/ato-run/ato/pull/1904) の通常経路で dev → stg → main に統合。
- `ato app install` による永続 GUI Instance、`ato app start` による再起動、検証済み DMG の取得、ソース GUI の署名付きローカル app wrapper、整合性確認付き build cache を含む。
- D2 の `--allow-native-source-build --allow-unsigned-native` は各起動で明示する。

受け入れ済み staging CLI source `8e35343f34eabdc9defbaf94f44e5901b643cf3d` と runtime source が同一であることを Git diff で確認した。
stable 化の変更は CLI の版、lockfile、changelog。既存の GUI 受け入れ記録も main に含まれる。
[D1/D2 の CAD 受け入れ記録](solidcraft-gui-staging-095-acceptance-20261118.md) は、公開 0.9.5-stg の空環境から作成・編集・保存・再起動・再読込・D2 cache hit までの実測。
今回の stable 公開では、その runtime を再ビルドした公開 CLI の配布経路と identity を検証した。stable binary で CAD 全操作や D2 cold build を再実行したとは扱わない。
GUI の実機受け入れ範囲は Apple M1 / macOS arm64。5 target の CLI 配布は他 OS の native GUI 対応を意味しない。

## 公開 installer の検証

新規 HOME/ATO_HOME/XDG/Cargo/Rustup/TMPDIR を作り、PATH は新規 bin と OS 標準だけに限定した。
GitHub release の `ato-cli-installer.sh` をそのまま実行し、12.49 秒・exit 0。
インストール済み CLI の version、source SHA、clean、release を照合し、公開 arm64 macOS archive 内 binary と SHA-256 が一致した。
`ato app install --help` に両方の明示許可 flag が含まれることを確認した。
同じ空環境の CLI から staging API の `ato hub describe` を匿名で読み、GUI bundle と D2 の参照、公開者の所要時間・依存関係との一致を確認した。Hub への書き込みは行っていない。
これは既存の macOS 上での隔離インストールで、OS/Xcode Command Line Tools の再インストールではない。

公開 18 assets すべての size と GitHub asset SHA-256 を照合し、配布 archive の `.sha256` sidecar も確認した。
Homebrew の公開 formula は 0.9.5 を参照し、macOS/Linux の全 4 archive の URL と hash が実際の公開 asset と一致した。Homebrew での実インストールはこの確認に含めない。

| 配布ファイル | SHA-256 |
| --- | --- |
| `ato-cli-aarch64-apple-darwin.tar.xz` | `sha256:b28c7adb635aa7104a792fc7ffae95132be25e865a5faf1ad971085db8a1b82b` |
| `ato-cli-aarch64-unknown-linux-gnu.tar.xz` | `sha256:8c8048fdc762f740f535ea277e519a5156374fa01e42e9d6ce1a576d861b99a8` |
| `ato-cli-x86_64-apple-darwin.tar.xz` | `sha256:b833f03f86c1e44594a0aaa672f6d75bdb60879a678e42ea6997cea45305f211` |
| `ato-cli-x86_64-pc-windows-msvc.msi` | `sha256:59191142041946b30289ee461f512f74b2fc7fc8281e21c137fb2bec311ef97b` |
| `ato-cli-x86_64-pc-windows-msvc.zip` | `sha256:8d7e61284726e6e905cb788c9b8030c3cc0ec98acddd12f3bae551eb79b0da91` |
| `ato-cli-x86_64-unknown-linux-gnu.tar.xz` | `sha256:6bcfc07d7a07c7f5fdb79beda49359168066d232e2f73a35c21a346c9071b936` |

利用者向けの今回検証した installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/ato-run/ato/releases/download/v0.9.5/ato-cli-installer.sh | sh
```

## 検証結果

ローカルの metadata、cargo-dist generate、fmt、CLI check、identity 3 テスト、CLI all-targets clippy は成功。
CLI CI は macOS/Linux/Windows すべて成功。main の architecture、snapshot materializer も成功。
全体 Rust CI の初回は macOS/relay 成功、Linux/Windows は既存ブラウザー試験の `timed out waiting for loopback CDP` で失敗した。該当コードは 0.9.4 から不変。失敗ジョブのみを再試行し、その結果を以下に記録する。初回失敗を成功として扱わない。
再試行では Windows が成功し、Linux の初回失敗テストも成功した。Linux 全体は別の既存試験 `fixed_tcp_propagates_client_eof_and_returns_the_connection_slot` が `Address already in use (os error 98)` で失敗し、全体 CI 成功とは扱わない。該当 `network_broker.rs` も 0.9.4 から不変。[失敗ジョブ](https://github.com/ato-run/ato/actions/runs/44149018729/job/134281407126) は 258 passed / 1 failed / 26 ignored。後続の Linux E2E と Clippy は成功した。

| 検証 | 最終観測 |
| --- | --- |
| Release / Homebrew | completed / success |
| CLI macOS / Linux / Windows | completed / success |
| Rust macOS / relay | completed / success |
| Rust Windows（再試行） | completed / success |
| Rust Linux（再試行） | completed / failure |
| architecture / snapshot materializer | completed / success |

[機械可読証跡](evidence/cli-stable-095-release-20261118.json) に release ID、全 asset digest、installed binary digest、公開 installer の identity、workflow/job の最終観測と初回失敗を保存した。
生ログと公開配布物は作業 worktree の `.tmp/release-095/` に保持する。
