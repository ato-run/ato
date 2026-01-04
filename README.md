# capsuled

Capsule Application Runtime Engine (Rust OSS 版)

## Overview

capsuled は UARC の実装である Capsule アプリケーションを実行するためのランタイムエンジンです。
OCI コンテナランタイム (youki) を使用して、署名済み Capsule を安全に実行します。

## Features

- Capsule 実行・管理
- OCI ランタイム統合 (youki)
- ストレージ/キャッシュ管理
- gRPC API

## Building

```bash
cargo build --release
```

## License

FSL-1.1-ALv2
