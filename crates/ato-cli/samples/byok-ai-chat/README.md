# BYOK AI Chat Capsule

**Bring Your Own Key** 型のAIチャットアプリケーション。ユーザーが自身のOpenAI APIキーを使用してAIとチャットできます。

## 特徴

- **Hybrid Auth**: Vault連携（環境変数注入）とUI手入力（localStorage）の両方をサポート
- **プライバシー重視**: APIキーはサーバーに保存されず、クライアント側のlocalStorageまたは環境変数経由で管理
- **複数プロバイダー対応**: Base URL変更でGroq、OpenRouter等のOpenAI互換APIに対応

## 認証モード

### 1. Vault Mode (推奨: プロダクション)
環境変数 `OPENAI_API_KEY` が設定されている場合、自動的にVaultモードで動作。UIでのキー入力は不要。

```bash
# 環境変数を設定して起動
OPENAI_API_KEY=sk-xxx npm run start
```

### 2. BYOK Mode (開発/個人利用)
環境変数が未設定の場合、ブラウザ上でAPIキーを入力するセットアップ画面が表示されます。
入力されたキーはブラウザの `localStorage` に保存されます。

## ローカル開発

```bash
# 依存関係のインストール
npm install

# 開発サーバー起動
npm run dev

# ブラウザで http://127.0.0.1:3000 を開く
```

## Capsule として実行

```bash
# カプセルとして実行（nacelleが依存関係を解決）
ato run ./

# 環境変数を注入して実行
ato run ./ --env OPENAI_API_KEY=sk-xxx
```

## ファイル構造

```
byok-ai-chat/
├── capsule.toml          # カプセルマニフェスト
├── .capsuleignore        # バンドル除外設定
├── package.json          # Node.js依存関係
├── next.config.mjs       # Next.js設定
├── tsconfig.json         # TypeScript設定
├── tailwind.config.js    # Tailwind CSS設定
├── postcss.config.js     # PostCSS設定
├── lib/
│   └── hooks/
│       └── useAtoBridge.ts  # 汎用Sync I/O フック (ドメイン非依存)
└── app/
    ├── layout.tsx        # ルートレイアウト
    ├── page.tsx          # メインUI (Hybrid Auth + 自動保存対応)
    ├── globals.css       # グローバルスタイル
    └── api/
        ├── chat/
        │   └── route.ts  # AI Chat API (Hybrid Auth)
        └── config/
            └── route.ts  # 設定状態API (boolean only)
```

## データ永続化 (Generic Host Bridge)

Ato Dashboard内で実行すると、チャット履歴が `~/Ato/Data/Chats/*.chat.sync` に保存されます。

### アーキテクチャ: 汎用ランタイム設計

**重要**: Atoランタイムは「チャット」などのドメイン概念を**知りません**。
アプリ（Capsule）側が以下を完全に制御します:

- **ファイルパス**: `Chats/2026-02-05-MyChat.chat.sync`
- **メタデータ**: `content_type`, `title`, `model` など
- **ペイロード構造**: `{ messages: [...], model: "gpt-4o" }`

```typescript
// アプリ側でファイル名・メタデータ・ペイロードを構築
await saveSync({
  path: `Chats/${date}-${title}.chat.sync`,  // アプリが決める
  manifest: {
    contentType: 'application/vnd.ato.chat+json',  // アプリが定義
    title: 'My Chat',
    extra: { model: 'gpt-4o', sessionId: '...' }
  },
  payload: { messages: [...] }  // アプリが定義
});
```

### 保存形式 (.sync v1.3)

```text
[2026-02-05-My-Chat.chat.sync] (ZIP Archive)
├── manifest.toml     # content_type = "application/vnd.ato.chat+json" (アプリ指定)
├── payload           # TAR (chat_history.json を含む)
└── sync.wasm         # Minimal no-op WASM
```

### 保存タイミング

- **自動保存**: AIの応答完了後、2秒のデバウンス後に自動保存
- **手動保存**: ヘッダーの「💾 保存」ボタンをクリック

### スタンドアロンモード

Ato Dashboard外（`npm run dev` など）で実行した場合、`localStorage` にフォールバックします。

## セキュリティ

- `/api/config` はAPIキーの**存在有無のみ**を返し、値は絶対に返しません
- `network.egress_allow` で許可されたドメインのみと通信可能
- `isolation.allow_env` で許可された環境変数のみがカプセル内で利用可能
- チャット履歴は平文で保存（将来的に暗号化対応予定）

## カスタマイズ

### 別のプロバイダーを使用

BYOK ModeでBase URLを設定することで、OpenAI互換APIを使用できます:

- **Groq**: `https://api.groq.com/openai/v1`
- **OpenRouter**: `https://openrouter.ai/api/v1`
- **Azure OpenAI**: `https://{resource}.openai.azure.com/openai/deployments/{deployment}/`
