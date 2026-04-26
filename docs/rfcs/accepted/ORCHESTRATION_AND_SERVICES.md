---
title: "オーケストレーション & サービス（最新）"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/nacelle/src/launcher/"
related:
  - "CAPSULE_CORE.md"
---

# オーケストレーション & サービス（最新）

このドキュメントは、multi-service（Compose相当）を `nacelle` が責務として担う前提で、実行時のサービス管理モデルをまとめます。

## 1. 方針

- multi-service の責務は **nacelle（Supervisor Mode）**に置く。
- Desktop/CLI は nacelle を「起動するだけ」にし、オーケストレーションロジックを重複させない。

参照: `NACELLE_SPEC.md`, `CAPSULE_CORE.md` Section 5

## 2. Dockerless Compose

- daemon不要: `ato run` がPID1（nacelle）を起動し、終了でクリーンアップ
- localhost mesh: サービス間はlocalhostで通信し、DNSではなく **環境変数注入**で接続情報を渡す

## 3. 依存関係と起動順

- `depends_on` によりDAGを構築し、循環はエラー
- readiness（health_check/readiness_probe）を満たすまで後続を待機

（manifestの詳細はSupervisor ADRに準拠）

## 4. Port Registry / 動的ポート

- `expose` により空きポートを確保し、テンプレート（例: `{{services.api.ports.API_PORT}}`）に埋め込む
- UI/ユーザーに提示する「公開ポート」を最小フィールドで選択できる（将来整理）

## 5. Socket Activation（FD継承）

- 親（nacelle）が先にソケットをbindし、子にFDを継承して起動する。
- これによりポート競合を回避し、起動前からlisten準備ができる。

参照: `apps/nacelle/src/launcher/` モジュール

## 6. ログ

- 既定は人間向け行ログ（`[service] ...`）
- 構造化（NDJSON等）はオプション（UIフィルタ用途）

参照: `NACELLE_SPEC.md` Section 3 (I/O)
