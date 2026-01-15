# Legacy scripts

このディレクトリは、過去の構成（daemon 前提など）に依存するスクリプトを退避します。

- `e2e-test-daemon.sh`: daemon/HTTPサーバー前提のE2E（現行のv2.0 bundle-first とは前提が異なる）

現行のテストは基本的に `cargo test -p nacelle` を使用してください。
