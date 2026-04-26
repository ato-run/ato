# ato V1 Demo Recording Runbook

## Pre-flight (1 回だけ)

```bash
# 1. CLI / Desktop をビルド
cd apps/ato-cli  && cargo build -p ato-cli
cd apps/ato-desktop && cargo build

# 2. レジストリのデータディレクトリを初期化
#    (既存の ~/.ato/registry があれば流用可)
mkdir -p ~/.ato/registry

# 3. レジストリを一度起動してスキーマを作成 → 停止
ato registry serve --data-dir ~/.ato/registry &
sleep 2 && kill %1

# 4. フィクスチャを流し込む
./scripts/seed_local_registry.sh ~/.ato/registry
#    → "[OK] Inserted 2 capsules" と出れば成功

# 5. 検証
ato registry serve --data-dir ~/.ato/registry &
curl -s 'http://127.0.0.1:8787/v1/capsules?q=' | python3 -m json.tool
#    → byok-ai-chat と openclaw-local-llm の2件が返る
```

## 録画開始

OBS / QuickTime で画面録画をスタート。

---

### Scene 1: フック (ストア画面)

```bash
# レジストリがポート 8787 で起動中であることを確認
lsof -nP -iTCP:8787 -sTCP:LISTEN

# デスクトップを起動
cd apps/ato-desktop && cargo run
```

**画面:** ストアが開き、2つのカプセルカードが表示される。
- "OpenClaw + Ollama (Local LLM)"
- "BYOK AI Chat"

**ナレーション:** 「READMEを読む時代は終わった。ワンクリックで動くWebアプリOSだ」

---

### Scene 2: ゼロコンフィグ (OpenClaw)

**操作:** "OpenClaw + Ollama (Local LLM)" カードの Run をクリック

**期待動作:**
- ConfigModal は出ない (required_env / config_schema が空)
- そのまま capsule が起動
- ターミナルペインに OpenClaw の起動ログが流れる

**ナレーション:** ポップアップなしで複雑なローカルLLM環境が一瞬で起動。

---

### Scene 3: 動的設定UI (BYOK AI Chat)

**操作:** "BYOK AI Chat" カードの Run をクリック

**期待動作:**
1. CLI が E103 `MissingRequiredEnv` を返す (stderr JSON)
2. Desktop が `details.missing_schema` をパース
3. **ConfigModal がオーバーレイで表示される:**
   - タイトル: "This capsule needs configuration"
   - Field 1: "OpenAI API Key" (Secret, masked input, placeholder "sk-...")
   - Field 2: "Model" (Enum, choices: gpt-4o-mini / gpt-4o / gpt-4-turbo, default gpt-4o-mini)
4. API Key を入力 → Save & Launch
5. SecretStore に保存 → capsule が再起動

**ナレーション:** 「CLIのエラーを完全に隠蔽した対話的フロー」

---

### Scene 4: ハッカーの権利 (decap)

**操作:** 別のターミナルウィンドウを開く

```bash
mkdir /tmp/byok-hack && cd /tmp/byok-hack
ato decap ato/byok-ai-chat --into .
```

**期待動作:**
- capsule.toml + ソースコード一式がカレントディレクトリに展開される
- `ls` でファイル一覧を見せる
- `cat capsule.toml` で config_schema (APIキー + Model) が見える

**ナレーション:** 「気に入らないなら中身をハックしろ。我々は海賊を求めている」

---

## Post-flight

```bash
# レジストリ停止
pkill -f "ato registry serve"

# テンポラリ削除
rm -rf /tmp/byok-hack
```

## Troubleshooting

| 症状 | 原因 | 対処 |
|------|------|------|
| ストアに何も表示されない | レジストリ未起動 or URL不一致 | `lsof -iTCP:8787` で確認。`/v1/capsules` にヒットするか `curl` で確認 |
| ConfigModal が出ない | E103 パースに失敗 | `RUST_LOG=debug cargo run` でオーケストレーターの stderr を確認 |
| "database is locked" | レジストリ起動中にフィクスチャ実行 | レジストリを停止してから `seed_local_registry.sh` を再実行 |
| E999 (ato build) | 既知のパイプラインバグ | フィクスチャ戦略で回避済み。build は不要 |
