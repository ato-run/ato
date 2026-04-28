---
title: "ADR-007: macOS Sandbox API 戦略 — 動的 SBPL via sandbox_init(flags=0)"
status: accepted
date: 2026-04-29
author: "@egamikohsuke"
related: ["NACELLE_TERMINAL_SPEC", "DRAFT_CAPSULE_IPC"]
---

# ADR-007: macOS Sandbox API 戦略

## 1. コンテキスト

ato-cli の IPC Broker は、capsule に注入する Unix domain socket のパスを
`ipc_socket_paths: Vec<String>` として nacelle へ渡す。nacelle は受け取った
パスを kernel-level sandbox に動的注入し、それ以外のパスへのアクセスは
deny-by-default で遮断する責務を持つ (Phase 13a §13a.1)。

Linux 側は Landlock LSM を用いた `path_beneath_rules()` でこの要件を
満たしており、`crates/nacelle/src/system/sandbox/linux.rs` で実装済み。

問題は **macOS 側**。Apple は 2016 年頃から `sandbox_init(3)` を
`__OSX_AVAILABLE_BUT_DEPRECATED` でマークしており、公式に推奨される
custom sandbox 適用の代替 API は存在しない。App Sandbox は entitlement
ベースで子プロセスの fork/exec パターンに不適合 (NACELLE_TERMINAL_SPEC.md §7.1)。

これまでの nacelle 実装は、deprecation を理由に `sandbox_init` を
`SANDBOX_NAMED = 0x1` フラグで呼び出して predefined profile (`no-network`,
`no-write-except-temporary` 等) のみを使う方針を採っていた。結果として
`ipc_socket_paths` の動的注入はできず、`apply_seatbelt_sandbox()` は
custom path policy が指定されると `not_enforced` を返していた
(`crates/nacelle/src/system/sandbox/macos.rs:96-99`、改訂前)。

## 2. 決定

`sandbox_init(profile, flags=0, &errorbuf)` を採用する。`flags = 0` のとき
`sandbox_init` は profile 引数を **raw SBPL source** として解釈する
(predefined name 扱いではない)。これは `sandbox-exec(1)` 自身が内部で使う
パターンであり、`always-further/nono` v0.36+ が production で採用している。

具体的には:

1. `nacelle::system::sandbox::macos::generate_sbpl_profile(policy)` で
   `SandboxPolicy` から完全な SBPL 文字列を生成する。
2. `sandbox_init(profile_cstr.as_ptr(), 0, &mut error_buf)` で適用。
3. 0 が返れば `SandboxResult::fully_enforced`、非 0 はエラー文字列付きで
   `not_enforced`。

生成される SBPL は `(deny default)` ベースの restrictive profile で、以下を
明示的に許可する:

- `process-exec` / `process-fork` / `signal (target self)` / `sysctl-read`
- `mach-lookup` / `ipc-posix-shm` (Unix 動作に必須)
- `read_write_paths` / `read_only_paths` (capsule.toml の isolation)
- IPC socket パス (`ipc_socket_paths`) — file ops と Unix-domain socket
  network ops の両方を allow

加えて以下を明示 deny:

- `mach-lookup` to `com.apple.secd` / `com.apple.SecurityServer` /
  `com.apple.security.agent` / `com.apple.authorizationd`
  (Keychain / authorisation の secret 流出防御。capsule は env 経由で
  注入された secret のみ使う設計)

### 2.1 採用しない案

- **案B: 規約による回避** — IPC ソケットを `/private/tmp/capsule-ipc/` に
  固定して predefined profile の `(allow file* (subpath "/private/tmp"))`
  に乗る案。Apple の predefined profile 内容に依存し、macOS バージョンで
  挙動が変わるリスク。Linux Landlock 側との対称性も崩れる。
- **案C: development_mode 維持** — release 前に再検討が必要であり、
  IPC を実機検証できないまま Phase 13b/13c が進む。
- **App Sandbox** — entitlement ベースで sandbox-exec の代替にならない
  (cf. AkihiroSuda/alcless の Medium 記事)。
- **sandbox-exec(1) ラッパー** — fork/exec の前段に sub-process を挟むと
  `pre_exec` フックで Landlock を適用するパターンと非対称になる。

### 2.2 リスクと緩和

| リスク | 緩和 |
|---|---|
| 将来の macOS で `sandbox_init` が削除される | nono が大規模 fleet で採用しており、同じパターンが先に検知される。release notes と nono の deprecation 関連 issue を 6 ヶ月毎に確認 |
| `flags=0` の挙動が undocumented | `sandbox-exec(1)` 自身が同 API を使うため、Apple が壊すと `sandbox-exec` も壊れる。実用上の保証あり |
| dynamic SBPL に予期しない deny が混入し子プロセスが起動失敗 | `(allow process-exec)` 等の essential rules を必ず含める。SBPL 生成テスト + IPC E2E で回帰検出 |
| ABI probe (Landlock V6→V1) との非対称 | Landlock の ABI probe は別 ADR (中期 RFC) で扱う。本 ADR の対象外 |

## 3. 影響範囲

### 3.1 コード変更

- `crates/nacelle/src/system/sandbox/macos.rs`: `apply_seatbelt_sandbox()` を
  動的 SBPL 経路に書き換え、`generate_sbpl_profile()` / `escape_path_for_sbpl()`
  の `#[allow(dead_code)]` を解除。Mach IPC keychain deny を追加。
- `crates/nacelle/docs/ENGINE_INTERFACE_CONTRACT.md`: §5 に Sandbox semantics
  節を追加。

### 3.2 テスト

- `crates/nacelle/src/system/sandbox/macos.rs::tests`: 9 件 (動的 SBPL の
  ipc paths inclusion、keychain deny、symlink resolution 等)。
- `crates/ato-cli/tests/ipc_socket_e2e.rs::ipc_socket_path_survives_native_sandbox_and_reaches_host_listener`:
  実機 macOS Seatbelt を通る IPC が host listener に到達することを確認 (15 秒)。

### 3.3 ドキュメント

- `claudedocs/research_phase13a_sandbox_best_practices_20260429.md` に
  詳細な背景と nono / alcless 比較。
- `docs/rfcs/accepted/NACELLE_TERMINAL_SPEC.md` §9 が既に nono パターンの
  取り込みを記述しており、本 ADR と整合。

## 4. 見直しサイクル

本 ADR は 6 ヶ月毎に再評価する (次回: 2026-10-29)。レビュー観点:

1. macOS major version (16+) で `sandbox_init(flags=0)` の挙動に変化なきか
2. nono が同パターンを継続採用しているか (release notes / source 確認)
3. Apple が sandbox 系 API の後継を発表していないか (WWDC notes)
4. `mach-lookup` deny リストに追加すべき service が現れていないか
   (Keychain Services 周辺の API 変更)

3 か月以内に macOS で問題が報告された場合は緊急レビュー。

## 5. 参考

- [always-further/nono crates/nono/src/sandbox/macos.rs](https://github.com/always-further/nono)
  — `flags = 0` を使う production 実装の参照
- [Apple Sandbox Guide v1.0 (fG!)](https://reverse.put.as/wp-content/uploads/2011/09/Apple-Sandbox-Guide-v1.0.pdf)
  — SBPL 文法のリバースエンジニアリング決定版
- [AkihiroSuda/alcless Medium](https://medium.com/nttlabs/alcoholless-lightweight-security-sandbox-for-macos-ccf0d1927301)
  — `sandbox_init` の deprecation 状況と App Sandbox の限界に関する一次資料
- `claudedocs/research_phase13a_sandbox_best_practices_20260429.md` —
  本 ADR の元になった研究成果
