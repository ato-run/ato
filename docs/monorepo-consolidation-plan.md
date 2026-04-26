# Monorepo 統合計画 — `ato-cli` × `ato-desktop`

> **Status:** Approved (2026-04-26)
> **Successor location:** `ato-run/ato/docs/monorepo-consolidation-plan.md` (新リポジトリ作成後に移管)
> **Pairs with:** [`v0.5-distribution-plan.md`](../../../docs/v0.5-distribution-plan.md), [`specs/CCP_SPEC.md`](specs/CCP_SPEC.md)

「コード管理は統合」「実行モデルは Desktop > CLI を死守」の二軸を物理的に強制する設計。前者だけ動かして後者は触らない、を構造的に縛る。

---

## 1. 結論

| 軸 | 方針 | 強制手段 |
|---|---|---|
| Cargo workspace | ⭕️ 統合 | 単一リポジトリ + `crates/` 配下 |
| Git repo | ⭕️ 統合（履歴保存） | `git subtree` |
| プロセス主従 | ❌ 統合しない | `capsule-core` から `gpui`/`wry`/`std::process::Command::new("ato-desktop")` 系を **lint で禁止** |
| ato-cli が ato-desktop を spawn | ❌ 永久禁止 | CI lint + ARCHITECTURE.md |
| ato-desktop が ato-cli を spawn | ⭕️ 唯一の許可ルート | 既存 `orchestrator.rs` を維持 |

---

## 2. 現状の歪み（統合の動機）

| 場所 | 内容 | 問題 |
|---|---|---|
| `apps/ato-cli/src/app_control/` | CCP envelope の **producer**（`schema_version: "ccp/v1"` を吐く） | wire shape 定義が片側にしかない |
| `apps/ato-desktop/src/ccp_envelope.rs` | CCP envelope の **consumer**（version classifier + tolerance） | 同じ schema を別言語的に再宣言 |
| `apps/ato-cli/core/src/foundation/types/manifest_v03.rs` | `capsule.toml` 型定義 | Desktop は subprocess 経由で manifest を *見られない*。本来は型で共有したい |
| `apps/ato-cli/src/app_control/snapshots/*.json` | bootstrap/status/repair の wire shape fixture | Desktop の単体テストには来ない |
| `dist-workspace.toml` (CLI) と `xtask` (Desktop) | リリースが二系統 | 同じバージョン番号でも別 PR・別 CI |

PR-1 で *仕様としては* ロックしたが、**コードベースは依然 wire shape を二重管理している**。これが monorepo 化の本質的動機。

---

## 3. ターゲット構造

```text
ato/                              # github.com/ato-run/ato (新リポジトリ)
├── Cargo.toml                    # [workspace] members = ["crates/*", "xtask"]
├── rust-toolchain.toml           # 統一: stable
├── deny.toml                     # 統一: cargo-deny
├── crates/
│   ├── capsule-core/             # 🆕 純粋ロジック (Desktop/CLI 双方が depends_on)
│   │   ├── src/
│   │   │   ├── ccp/              #  ← apps/ato-desktop/src/ccp_envelope.rs から移設
│   │   │   │   ├── schema.rs    #     + producer 側の構造体定義
│   │   │   │   └── tolerance.rs #     classifier + enforce_ccp_compat
│   │   │   ├── manifest/         #  ← apps/ato-cli/core/src/foundation/types/manifest_v03.rs
│   │   │   ├── error/            #  ← E-code envelopes (E103 etc.)
│   │   │   └── config/           #  ← ConfigField / ConfigKind (最近 PR で入った schema)
│   │   └── Cargo.toml            #  禁則: gpui / wry / objc2 / windows-rs
│   │
│   ├── ato-cli/                  # ← apps/ato-cli/ から移植
│   │   ├── src/
│   │   ├── core/                 #  もとの core/ は capsule-core に吸収後、空 or runtime-only
│   │   └── Cargo.toml            #  depends_on: capsule-core
│   │
│   ├── ato-desktop/              # ← apps/ato-desktop/ から移植
│   │   ├── src/
│   │   │   ├── ccp_envelope.rs   #  → 削除 (capsule-core に統合)
│   │   │   ├── orchestrator.rs   #  CLI を spawn する唯一の場所 (維持)
│   │   │   └── ...
│   │   └── Cargo.toml            #  depends_on: capsule-core (ato-cli には依存しない)
│   │
│   └── ato-tsnetd/               # 既存サイドカーがあれば一緒に
│
├── xtask/                        # ← apps/ato-desktop/xtask/ から移植 (workspace member に復帰)
│   └── Cargo.toml                #  bundle 済みアプリは Desktop のみ扱う
│
├── installer/                    # ← apps/ato-desktop/installer/ をそのまま
│   ├── entitlements.plist
│   ├── wix.wxs
│   ├── ato-desktop.desktop
│   ├── ato-desktop.appdata.xml
│   └── homebrew/Casks/ato.rb
│
├── docs/
│   ├── specs/
│   │   ├── CCP_SPEC.md           # ← apps/ato-cli/docs/specs/CCP_SPEC.md
│   │   └── ARCHITECTURE.md       # 🆕 「Desktop > CLI 主従不変条件」を明文化
│   ├── known-limitations.md
│   ├── v0.5-distribution-plan.md
│   └── monorepo-consolidation-plan.md  # 🆕 (この文書の移管先)
│
├── .github/
│   └── workflows/
│       ├── ci.yml                # 統合: build + clippy + test for all crates
│       ├── purity-lint.yml       # 統合: state-layer-lint + core-purity-lint
│       ├── cli-release.yml       # cargo-dist (CLI 単体リリース)
│       └── desktop-release.yml   # xtask (Desktop bundle リリース)
│
└── dist-workspace.toml           # cargo-dist: package = "ato-cli" のみ対象
```

**設計上の主張:** xtask は workspace member だが、**Desktop bundle のみを生成する責務に閉じる**。CLI のリリースは引き続き cargo-dist 標準フロー。これでツールチェーンが分離される。

---

## 4. `capsule-core` に入れるもの / 入れないもの

### 入れる（共有）

| 候補 | 出処 | 共有理由 |
|---|---|---|
| CCP envelope 型 (`schema_version`, `package_id`, `action`, `session: T`) | ato-desktop/src/ccp_envelope.rs + ato-cli の app_control/snapshots | producer/consumer の wire shape 単一定義 |
| `classify_schema_version` / `enforce_ccp_compat` | ato-desktop | CLI 自身も古い CLI bin と新 schema の相互運用ケースで使う可能性 |
| Manifest types (`CapsuleManifestV03` など) | ato-cli/core/src/foundation/types | Desktop も subprocess 出力をパースする時に型を借りたい |
| Error envelope (E103 含む E-code 体系) | ato-cli | Desktop の error toast UI が型で受けたい |
| `ConfigField` / `ConfigKind` schema | ato-cli (最近の PR) | Desktop の動的 config UI がまさに consumer |

### 入れない（GUI は親、CLI 実装は内部詳細）

| 不採用 | 理由 |
|---|---|
| `clap` based CLI コマンド定義 | ato-cli の私的事情 |
| Runtime drivers (Python/Node/Rust/...) | CLI の実装詳細、Desktop は知るべきでない |
| `app_control/session.rs` の状態機械 | CLI の private state |
| GPUI / Wry / state layer | Desktop の GUI 詳細、core に漏らしたら negate される |
| `orchestrator.rs` (Desktop が CLI を spawn) | Desktop 専属 — ここは「主従の場所」そのもの |

**ガードレール:** `capsule-core/Cargo.toml` には以下を **絶対に** 入れない:

```toml
# 禁則 (CI で grep 落とす)
gpui        # GUI
wry         # WebView
objc2       # macOS native
windows     # Windows native
clap        # CLI argv
nix         # POSIX runtime
sandbox-exec # OS sandbox driver
```

これは現行の `state-layer-lint.yml` パターンを `capsule-core` にも適用する形で実現する（既に `apps/ato-desktop/.github/workflows/state-layer-lint.yml` と `apps/ato-cli/.github/workflows/core-purity-lint.yml` で雛形が landed しているので、monorepo 後はこの 2 つを 1 本に統合するだけ）。

---

## 5. プロセス実行モデルの不変条件

新リポジトリの `docs/specs/ARCHITECTURE.md` に **invariant** として明文化し、lint と PR テンプレで強制する。

```md
## §1. Process Hierarchy Invariant (NON-NEGOTIABLE)

1. ato-desktop は OS-level の親プロセスとしてのみ起動する。
   - macOS: `Ato Desktop.app/Contents/MacOS/ato-desktop` (Launch Services)
   - Windows: `Program Files\Ato\ato-desktop.exe` (explorer / Start Menu)
   - Linux: `/usr/bin/ato-desktop` (Desktop entry)

2. ato-cli は以下のいずれかとしてのみ起動する:
   a. ユーザーが直接ターミナルで叩く (CLI-only path)
   b. ato-desktop が `std::process::Command` で spawn する子プロセス

3. **禁則:** ato-cli から ato-desktop を spawn してはならない。
   - `ato gui` / `ato desktop` / `ato open --gui` 系のサブコマンドは作らない
   - 例外なし。VS Code 方式の "messenger" すら v0.x では実装しない
     (将来許可するとしても、それは `ato-cli` でなく別の小さな
      `ato-open` バイナリとして切り出す)

4. capsule-core は std::process を含む I/O ライブラリに依存しない。
   - producer/consumer の wire shape のみを所有する
```

CI 強制:

```yaml
# .github/workflows/purity-lint.yml に追加
- name: ato-cli must not spawn ato-desktop
  run: |
    if rg -n 'Command::new\(["\x27](.*?ato-desktop|ato_desktop)' crates/ato-cli/; then
      echo "::error::ato-cli must not spawn ato-desktop (ARCHITECTURE.md §1.3)"
      exit 1
    fi
```

---

## 6. v0.5 リリースとの関係（タイミング）

| Phase | Repo 構成 | 期間 |
|---|---|---|
| **Now → v0.5.0 tag** | 既存の 2 repo のまま | 11 PR の distribution work が landed 済み、これを壊さない |
| v0.5.0 ship 直後 | monorepo migration (M1〜M3) | コード移動のみ。capsule-core 抽出はまだ |
| v0.5.x | capsule-core extraction (M4〜M5) | wire shape 重複の解消 |
| v0.6 | monorepo 前提でリリース | xtask + cargo-dist 統合 CI |

**理由:** v0.5 は「同じバージョン pair = 同じ CCP version」の保証 (PR-1) を売り文句にしている。リリース直前に repo 構造を弄ると、ad-hoc 署名済みバンドルの再生成・Cask の SHA 再計算など、検証のやり直しが発生する。

---

## 7. 段階移行プラン（PR シーケンス）

### M1 — 新リポジトリ作成（履歴保存）

```bash
# 新 repo 初期化
mkdir ato && cd ato
git init -b main

# ato-cli を crates/ato-cli/ に履歴つきで吸収
git subtree add --prefix=crates/ato-cli \
    https://github.com/ato-run/ato-cli.git main

# ato-desktop を crates/ato-desktop/ に履歴つきで吸収
git subtree add --prefix=crates/ato-desktop \
    https://github.com/ato-run/ato-desktop.git main
```

`git subtree` は SHA は変わるが、`git log --follow crates/ato-cli/src/app_control/session.rs` で旧 commit にトレース可能。`git filter-repo` を使うルートもあるが履歴の透明性で subtree を推奨。

**この PR のスコープ:** ファイル移動のみ、ビルドはまだ通らなくてよい。レビューは「ファイルが正しい場所に来ているか」だけ。

### M2 — Cargo workspace 化

```toml
# /Cargo.toml
[workspace]
resolver = "2"
members = [
    "crates/ato-cli",
    "crates/ato-desktop",
    "xtask",
]

[workspace.package]
edition = "2021"            # gpui-component が要求する継承元 ★重要
version = "0.5.0"
license = "Apache-2.0"
repository = "https://github.com/ato-run/ato"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
# ... 共通バージョン pinning
```

**この PR で解決すべき既知の罠:**
- 旧 ato-desktop 単体での workspace 化試行は `gpui-component` (`.tmp/vendor/gpui-component-local/crates/ui/Cargo.toml`) の `edition.workspace = true` で失敗した。`workspace.package.edition = "2021"` を root に置けば解消。
- `gpui-component` のような vendored path dep が `[workspace] members` 外にある場合、`exclude` への追加で十分（workspace 内継承を切れる）。

### M3 — Vendored deps の整理

| 対象 | 当初の計画 | 実際の対応 (2026-04-26) |
|---|---|---|
| `apps/ato-desktop/.tmp/vendor/gpui-component-local/` | `crates/ato-desktop/vendor/gpui-component/` に正式昇格 | ✅ **不要だった** — 元 repo で gitignore されており subtree merge で持ち込まれなかった。Cargo.toml は既に `git = "https://github.com/ato-run/gpui-component"` で fork を参照済み（rev pinning あり）。 |
| `gpui` Zed fork | git submodule として `vendor/gpui` に固定 | ⏸ **保留** — git dep + rev pinning で実用上 deterministic。submodule 化は (a) `gpui-component` を patch する用途が出たとき、(b) Zed 上流の breaking 変更を local で当てたいときに再検討。それまでは Cargo.toml の `rev` 文字列 1 行で pin する現状を維持。 |
| `apps/ato-cli/.tmp/scratch` | `.ato/` に統合済み、引き続き `.gitignore` | ✅ 完了 (commit `e68234b`)。monorepo 側は root `.gitignore` に `.ato/`/`**/.ato/`、`crates/ato-cli/.gitignore` と `crates/ato-desktop/.gitignore` にも `.ato/` を追加して二重防御。 |
| **追加発見**: `crates/ato-desktop/.ato/` に runtime cache が 25MB tracked | — | ✅ **修正済み** — subtree merge 経由で `crates/ato-desktop/.ato/` に CI artifact (25MB) や lockfile snapshot が混入していた。`git rm -r --cached` で untrack、ローカルからも削除。`.gitignore` に `.ato/` を追加して再発防止。元 repo の `.gitignore` には `.ato/` がなく、`.tmp/` だけだったのが原因。 |
| **追加**: `tests/.git` (embedded repository) | — | ✅ 削除 — capsuled-dev 側で `tests/` 配下に独立 git repo があり、subtree merge ではなく単純コピーで持ち込んだ際に `.git` ディレクトリも同梱されていた。M2 commit 直前に削除。 |

**Vendor 戦略決定 (確定):** 当面は Cargo の `git = "..." rev = "..."` による pinning を採用。submodule 移行は「fork で patch を当てたい」要件が具体化した時点で別 PR として実施する（GPUI 周りで上流 breaking が来た場合など）。

### M4 — `capsule-core` 抽出 (Phase 1: CCP)

最初に切るのは **CCP envelope の wire shape** だけ。一番依存関係が浅く、両側から型として参照される。

```
crates/ato-cli/core/src/ccp/        # capsule-core crate (relocation deferred to M5+)
├── mod.rs                          # re-exports, module-level docs
├── schema.rs                       # CcpHeader (payload-agnostic deserializer)
├── tolerance.rs                    # classify_schema_version, enforce_ccp_compat,
│                                   # CcpCompat, HasSchemaVersion, MalformedSchemaVersion
└── version.rs                      # SCHEMA_VERSION = "ccp/v1" 定数
```

- ato-desktop/src/ccp_envelope.rs → ✅ 削除済み、`use capsule_core::ccp::*;` に置換 (orchestrator.rs)
- ato-cli/src/app_control.rs → ✅ ローカル `const SCHEMA_VERSION` を削除し
  `use capsule_core::ccp::SCHEMA_VERSION;` 経由で参照。子モジュール (resolve / session) は
  `super::SCHEMA_VERSION` 経由で同じ name resolution を継承
- 既存スナップショット fixture (`bootstrap.json`/`status.json`/`repair.json`) を
  `crates/ato-cli/core/tests/fixtures/ccp/` に移動し、producer (ato-cli の `assert_snapshot`) と
  consumer (`capsule-core/tests/ccp_fixtures.rs` の golden test 3 本) の **両方** が同一バイト列を
  読む構造にした。これで wire shape の二重管理が物理的に不可能になる。
- 完了範囲外: producer 側の envelope 構造体 (`StatusEnvelope` / `BootstrapEnvelope` /
  `RepairEnvelope` など) と consumer 側の `ResolveEnvelope` / `SessionStartEnvelope` /
  `SessionStopEnvelope` は **まだ各 crate に閉じている**。これらの統合は payload type
  (manifest / config / E-code) が capsule-core に来る M5 以降に実施。
- crate ディレクトリそのものの `crates/ato-cli/core/` → `crates/capsule-core/` への移設は
  パッケージ名 (`capsule-core`) が既に一致しているため import パスに影響せず、後段で
  まとめて実施可能。M4 ではディレクトリ移動は見送り。

### M5 — `capsule-core` 抽出 (Phase 2: Manifest + Error + Config)

- `crates/ato-cli/core/src/foundation/types/manifest_v03.rs` → `crates/capsule-core/src/manifest/`
- E-code envelopes (E103 含む) → `crates/capsule-core/src/error/`
- `ConfigField` / `ConfigKind` → `crates/capsule-core/src/config/`

各々独立 PR に分けてレビュー。manifest は WASM 連携 (`lock-draft-wasm`) があるので、必要に応じて `capsule-core` を `no_std` 互換にする道も検討（ただし v0.6 までは `std` 前提で OK）。

### M6 — リリース統合

- 旧 `apps/ato-cli/dist-workspace.toml` → 新 root の `dist-workspace.toml` (cargo-dist は **`packages = ["ato-cli"]` で CLI のみ対象**、Desktop はノータッチ)
- 旧 `apps/ato-desktop/.github/workflows/desktop-release.yml` → 新 `.github/workflows/desktop-release.yml`
- タグ運用は維持: `ato-cli-v*` で CLI リリース、`ato-desktop-v*` で Desktop リリース。タグ prefix で workflow を出し分け。

### M7 — 旧 repo の archive

- `ato-run/ato-cli` と `ato-run/ato-desktop` を **archive (read-only)** に。
- README に "Moved to ato-run/ato" の pointer のみ残す。
- 既存 issue / PR は新 repo に転送せず、リンクのみで参照。

---

## 8. リスク と 対策

| リスク | 影響 | 対策 |
|---|---|---|
| `gpui-component` workspace 継承で再爆発 | M2 で workspace 化が止まる | `[workspace.package]` に `edition` / `version` を必ず置く。前回の試行では `edition.workspace = true` の継承元が無かったのが原因 |
| cargo-dist が multi-package workspace を嫌う | CLI リリースが壊れる | `dist-workspace.toml` の `[dist] packages = ["ato-cli"]` で対象を絞る。Desktop は cargo-dist 対象外 |
| 履歴のリンクが切れる (issue が SHA 参照している等) | レビュー時の追跡性低下 | `git subtree` で SHA は新規だが、旧 SHA → 新 SHA のマッピング表を `docs/migration/sha-map.md` に出力 |
| ブランチモデル不整合 (ato-cli=`dev`, ato-desktop=`main`) | デフォルトブランチ統一が必要 | 新 repo は `main` 単一。CI フックの再設定は M1 と同時に |
| 二系統の `xtask` / `dist` ワークフローが互いに踏む | リリース時 race | タグ prefix で trigger 切り分け。`cli-release.yml` は `ato-cli-v*` のみ、`desktop-release.yml` は `ato-desktop-v*` のみ |
| open PR が両 repo に大量にある状態で凍結 | コントリビューターの怒り | M1 直前に "freeze week" を設けて全 PR を merge / close |
| Desktop bundle 内蔵の `Helpers/ato` の path が変わる | cli_install.rs の探索が壊れる | bundle 内の path は維持 (xtask が `crates/ato-cli/target/.../ato` を `Contents/Helpers/ato` にコピーする規約は不変) |

---

## 9. 不変条件（リグレッション防止）

monorepo 化後も死守する CI ガード：

```yaml
# .github/workflows/purity-lint.yml
jobs:
  capsule-core-purity:
    name: capsule-core must not depend on GUI / OS / process libs
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - name: Forbid GUI / process deps
        run: |
          set -e
          banned='^(gpui|wry|objc2|windows|clap|nix)\b'
          if rg -n "$banned" crates/capsule-core/Cargo.toml; then
            echo "::error::capsule-core has a forbidden dep (ARCHITECTURE.md §1.4)"
            exit 1
          fi

  no-cli-spawns-desktop:
    name: ato-cli must not spawn ato-desktop
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - name: Forbid Command::new("ato-desktop") in CLI
        run: |
          if rg -n 'Command::new\(["\x27].*?ato[-_]desktop' crates/ato-cli/; then
            echo "::error::ato-cli is trying to spawn ato-desktop (ARCHITECTURE.md §1.3)"
            exit 1
          fi
```

---

## 10. 移行後に得られる具体的なメリット

| 項目 | Before (現状) | After (monorepo) |
|---|---|---|
| CCP wire shape 変更 | 2 repo に同時 PR、レビューも 2 セット | 1 PR、capsule-core の型を変えるだけで両側ビルド失敗 |
| `capsule.toml` 型変更 | CLI で変更 → Desktop は subprocess 出力を再パース | 型を共有するだけで両側 type-checked |
| バージョン同期 | PR-1 で「規約として」担保 | workspace.version で物理的に同じ番号 |
| CI 分散 | ato-cli CI + ato-desktop CI、相互無関係 | 1 push で両方ビルドされ wire shape 不整合が即発覚 |
| 新規 contributor のオンボード | 「どっちの repo を clone する？」 | 1 clone で全部 |
| クロスリポジトリ atomic refactor | 不可能（先 merge / 後 merge の orderhing 問題） | 単一 PR で完結 |

---

## 11. 公式決定（2026-04-26 確定）

| 質問 | **公式決定** | 理由 |
|---|---|---|
| 新 repo 名 | **`ato-run/ato`** | 短く、ブランド一致、ato という製品名そのもの |
| デフォルトブランチ | **`main`** | ato-desktop 側に寄せる、release tag が main で打たれる業界標準 |
| 移行タイミング | **`v0.5.0` ship 直後** | M1〜M3 を即実行、capsule-core 抽出は v0.5.x で gradual |
| `capsule-core` の crate 名 | **`capsule-core`** | ato という製品名と protocol/spec の境界を分離 |
| xtask の所属 | **workspace member** | 前回 standalone にした制約は M2 で解消されるため復帰 |
| 旧 repo の扱い | **archive (read-only)** | URL は永続、forks は read-only に保たれる |

---

## 12. 推奨アクション

1. **この計画書を `apps/ato-cli/docs/monorepo-consolidation-plan.md` として commit** ✅ (この commit)
2. **v0.5.0 を tag** — Plan B 11 PR の成果を確定
3. **freeze week 開始** — open PR を捌ききる
4. M1 (新 repo + git subtree) を実行
5. M2〜M3 を直列で merge (workspace 化 + vendor 整理)
6. v0.5.x の通常開発を新 repo で再開
7. M4 (CCP 抽出) を最初の "monorepo らしい PR" として merge し、価値を実証
8. M5〜M7 を v0.6 までの間に gradual に
