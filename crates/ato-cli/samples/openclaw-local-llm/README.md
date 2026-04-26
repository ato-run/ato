# OpenClaw + Ollama (Local LLM)

OpenClaw AI エージェントをローカル LLM (Ollama) で動かすサンプルカプセル。
クラウド API Key 不要 - 全てローカルで完結。

## 実行

```bash
ato run ./samples/openclaw-local-llm/ --sandbox \
  --compatibility-fallback host --agent off --yes
```

初回実行時に自動で:
1. Ollama をインストール (brew)
2. Ollama サーバーを起動
3. モデルを pull (デフォルト: qwen3:32b, ~20GB)
4. OpenClaw gateway を起動

## 推奨モデル

| モデル | サイズ | RAM 目安 | ツール呼び出し |
|--------|--------|----------|----------------|
| `qwen3:32b` | 32B | 20GB+ | 安定 |
| `llama4:scout` | 17Bx16E | 64GB+ | 安定 |
| `mistral-small3.1` | 24B | 16GB+ | 安定 |

モデル変更:

```bash
OPENCLAW_MODEL=mistral-small3.1 ato run ./samples/openclaw-local-llm/ \
  --sandbox --compatibility-fallback host --agent off --yes
```

## カスタマイズ

- `SOUL.md` — エージェントの人格・振る舞いを編集
- `OLLAMA_HOST` — Ollama のアドレス変更 (デフォルト: `http://127.0.0.1:11434`)
- `OPENCLAW_MODEL` — 使用モデル変更 (デフォルト: `qwen3:32b`)

## トラブルシューティング

**ツール呼び出しが失敗する**
→ 8B 以下のモデルはツール呼び出しが不安定。32B 以上を推奨。

**コンテキストウィンドウ不足**
→ OpenClaw は 64k+ トークンを推奨。Ollama の `num_ctx` で調整可能。
