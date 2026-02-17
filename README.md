# nacelle

`nacelle` は **Capsule を実行するためのエンジン（internal runtime）**です。エンドユーザーが直接触る入口は `ato-cli` を想定し、`ato-cli` がプロセス境界（JSON over stdio）で `nacelle internal ...` を呼び出して実行します。

## 役割

- **Mechanism（実行メカニズム）**
  - バンドル/アーティファクトの展開
  - OSネイティブ隔離（filesystem / network）
  - プロセス起動・監視（Supervisor Mode）
  - Socket Activation（FD継承）

- **非ゴール（原則ホスト側へ）**
  - 署名検証・ポリシー決定・対話的UX（Smart Build, Dumb Runtime）
  - OS API提供（Host Bridge Pattern はホスト側の責務）

## ドキュメント

- Engine契約（CLI↔Engine）: [nacelle/docs/ENGINE_INTERFACE_CONTRACT.md](docs/ENGINE_INTERFACE_CONTRACT.md)
- セキュリティポリシー: [nacelle/SECURITY.md](SECURITY.md)
- 最新アーキテクチャ概要（repo全体）: [docs/architecture/ARCHITECTURE_OVERVIEW.md](../docs/architecture/ARCHITECTURE_OVERVIEW.md)

## 関連ADR

- `docs/adr/2026-01-06_000000_smart-build-dumb-runtime.md`
- `docs/adr/2026-01-03_000000_supervisor-mode.md`
- `docs/adr/2026-01-15_000001_socket-activation.md`
- `docs/adr/2026-01-07_000000_system-abstraction.md`
