"use client";

import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { useChat } from "ai/react";
import { useAtoBridge, type WriteSyncRequest } from "../lib/hooks/useAtoBridge";

type AuthMode = "loading" | "vault" | "byok" | "setup";

interface ConfigResponse {
  hasEnvKey: boolean;
  hasEnvBaseUrl: boolean;
  authMode: "vault" | "byok";
}

// Generate or retrieve session ID
function getOrCreateSessionId(): string {
  if (typeof window === "undefined") return "";
  let sessionId = sessionStorage.getItem("chat_session_id");
  if (!sessionId) {
    sessionId = `chat_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
    sessionStorage.setItem("chat_session_id", sessionId);
  }
  return sessionId;
}

// Generate date string for filename
function getDateString(): string {
  const now = new Date();
  return now.toISOString().split("T")[0]; // YYYY-MM-DD
}

// Sanitize title for filename
function sanitizeTitle(title: string): string {
  return (
    title
      .replace(/[^a-zA-Z0-9\s-]/g, "")
      .replace(/\s+/g, "-")
      .slice(0, 50)
      .replace(/-+$/, "") || "untitled"
  );
}

export default function Page() {
  const [authMode, setAuthMode] = useState<AuthMode>("loading");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [configError, setConfigError] = useState<string | null>(null);
  const [chatTitle, setChatTitle] = useState("New Chat");
  const sessionId = useMemo(() => getOrCreateSessionId(), []);

  // Ato Bridge for generic sync I/O
  const {
    saveSync,
    saveState,
    isAtoEnvironment,
    createAutoSave,
    error: saveError,
    listSync,
    readSync,
  } = useAtoBridge();
  const autoSaveRef = useRef<((request: WriteSyncRequest) => void) | null>(
    null,
  );
  const [loadState, setLoadState] = useState<
    "idle" | "loading" | "loaded" | "error"
  >("idle");
  const [lastLoadedPath, setLastLoadedPath] = useState<string | null>(null);
  const [lastLoadedPayload, setLastLoadedPayload] = useState<string | null>(
    null,
  );

  // Initialize auto-save function
  useEffect(() => {
    autoSaveRef.current = createAutoSave(2000); // 2 second debounce
  }, [createAutoSave]);

  // Check server configuration on mount
  useEffect(() => {
    const checkConfig = async () => {
      try {
        const res = await fetch("/api/config");
        const config: ConfigResponse = await res.json();

        if (config.hasEnvKey) {
          // Vault mode: env var is set, no UI input needed
          setAuthMode("vault");
        } else {
          // BYOK mode: check localStorage for saved key
          const storedKey = localStorage.getItem("byok_api_key");
          const storedUrl = localStorage.getItem("byok_base_url");

          if (storedKey) {
            setApiKey(storedKey);
            setBaseUrl(storedUrl || "");
            setAuthMode("byok");
          } else {
            setAuthMode("setup");
          }
        }
      } catch (error) {
        console.error("Config check failed:", error);
        setConfigError("設定の読み込みに失敗しました");
        setAuthMode("setup");
      }
    };

    checkConfig();
  }, []);

  const { messages, input, handleInputChange, handleSubmit, error, isLoading } =
    useChat({
      api: "/api/chat",
      // Only send credentials in BYOK mode (Vault mode uses env vars server-side)
      body:
        authMode === "byok" ? { apiKey, baseUrl: baseUrl || undefined } : {},
      onError: (err) => {
        console.error("Chat Error:", err);
        // If auth error in BYOK mode, prompt for new key
        if (authMode === "byok" && err.message.includes("401")) {
          setConfigError("APIキーが無効です。再設定してください。");
        }
      },
      onFinish: () => {
        // Auto-save after assistant response completes
        // App controls: file path, manifest metadata, payload structure
        if (autoSaveRef.current && messages.length > 0) {
          const title = chatTitle || generateTitleFromMessages(messages);
          const filename = `${getDateString()}-${sanitizeTitle(title)}.chat.sync`;

          autoSaveRef.current({
            // App controls the directory structure
            path: `Chats/${filename}`,
            // App defines the content type and metadata
            manifest: {
              contentType: "application/vnd.ato.chat+json",
              displayExt: "chat",
              title: title,
              extra: {
                model: "gpt-4o-mini",
                sessionId: sessionId,
                messageCount: messages.length,
              },
            },
            // App defines the payload structure
            payload: {
              sessionId,
              title,
              model: "gpt-4o-mini",
              createdAt: new Date().toISOString(),
              messages: messages.map((m) => ({
                id: m.id,
                role: m.role,
                content: m.content,
              })),
            },
          });
        }
      },
    });

  // Generate title from first user message
  function generateTitleFromMessages(msgs: typeof messages): string {
    const firstUserMsg = msgs.find((m) => m.role === "user");
    if (firstUserMsg) {
      const content = firstUserMsg.content.slice(0, 50);
      return content.length < firstUserMsg.content.length
        ? `${content}...`
        : content;
    }
    return "New Chat";
  }

  // Manual save handler - App controls all metadata
  const handleManualSave = useCallback(async () => {
    if (messages.length === 0) return;

    const title = chatTitle || generateTitleFromMessages(messages);
    const filename = `${getDateString()}-${sanitizeTitle(title)}.chat.sync`;

    await saveSync({
      path: `Chats/${filename}`,
      manifest: {
        contentType: "application/vnd.ato.chat+json",
        displayExt: "chat",
        title: title,
        extra: {
          model: "gpt-4o-mini",
          sessionId: sessionId,
          messageCount: messages.length,
        },
      },
      payload: {
        sessionId,
        title,
        model: "gpt-4o-mini",
        createdAt: new Date().toISOString(),
        messages: messages.map((m) => ({
          id: m.id,
          role: m.role,
          content: m.content,
        })),
      },
    });
  }, [messages, chatTitle, sessionId, saveSync]);

  const handleLoadLatest = useCallback(async () => {
    if (!isAtoEnvironment) return;
    setLoadState("loading");

    const listResult = await listSync("Chats");
    if (
      !listResult.success ||
      !listResult.files ||
      listResult.files.length === 0
    ) {
      setLoadState("error");
      return;
    }

    const latest = [...listResult.files].sort().slice(-1)[0];
    const readResult = await readSync(latest);

    if (!readResult.success) {
      setLoadState("error");
      return;
    }

    setLastLoadedPath(latest);
    setLastLoadedPayload(JSON.stringify(readResult.payload ?? {}, null, 2));
    setLoadState("loaded");
  }, [isAtoEnvironment, listSync, readSync]);

  const saveConfig = useCallback(() => {
    if (!apiKey.trim()) {
      setConfigError("APIキーを入力してください");
      return;
    }

    localStorage.setItem("byok_api_key", apiKey.trim());
    if (baseUrl.trim()) {
      localStorage.setItem("byok_base_url", baseUrl.trim());
    } else {
      localStorage.removeItem("byok_base_url");
    }

    setConfigError(null);
    setAuthMode("byok");
  }, [apiKey, baseUrl]);

  const clearConfig = useCallback(() => {
    localStorage.removeItem("byok_api_key");
    localStorage.removeItem("byok_base_url");
    setApiKey("");
    setBaseUrl("");
    setAuthMode("setup");
  }, []);

  // Loading state
  if (authMode === "loading") {
    return (
      <div className="flex items-center justify-center min-h-screen">
        <div className="text-gray-500">読み込み中...</div>
      </div>
    );
  }

  // Setup screen (BYOK mode without saved key)
  if (authMode === "setup") {
    return (
      <div className="flex flex-col items-center justify-center min-h-screen p-4 space-y-6">
        <div className="text-center space-y-2">
          <h1 className="text-2xl font-bold">BYOK AI Chat</h1>
          <p className="text-gray-600 dark:text-gray-400 text-sm">
            自分のAPIキーを使用してAIとチャット
          </p>
        </div>

        <div className="w-full max-w-md space-y-4">
          <div>
            <label className="block text-sm font-medium mb-1">
              OpenAI API Key <span className="text-red-500">*</span>
            </label>
            <input
              type="password"
              placeholder="sk-..."
              className="w-full border border-gray-300 dark:border-gray-700 p-3 rounded-lg bg-white dark:bg-gray-900 focus:outline-none focus:ring-2 focus:ring-blue-500"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && saveConfig()}
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">
              Base URL <span className="text-gray-400">(オプション)</span>
            </label>
            <input
              type="text"
              placeholder="https://api.openai.com/v1 (デフォルト)"
              className="w-full border border-gray-300 dark:border-gray-700 p-3 rounded-lg bg-white dark:bg-gray-900 focus:outline-none focus:ring-2 focus:ring-blue-500"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
            />
            <p className="text-xs text-gray-500 mt-1">
              Groq, OpenRouter等の互換APIを使用する場合に設定
            </p>
          </div>

          {configError && (
            <div className="text-red-500 text-sm bg-red-50 dark:bg-red-900/20 p-3 rounded-lg">
              {configError}
            </div>
          )}

          <button
            onClick={saveConfig}
            className="w-full bg-blue-600 hover:bg-blue-700 text-white font-medium px-4 py-3 rounded-lg transition-colors"
          >
            チャットを開始
          </button>
        </div>

        <p className="text-xs text-gray-400 max-w-md text-center">
          APIキーはブラウザのlocalStorageに保存され、サーバーには保存されません。
        </p>
      </div>
    );
  }

  // Chat interface (Vault or BYOK mode)
  return (
    <div className="flex flex-col h-screen max-w-3xl mx-auto">
      {/* Header */}
      <header className="flex justify-between items-center p-4 border-b border-gray-200 dark:border-gray-800">
        <div className="flex items-center gap-2">
          <h1 className="font-bold">BYOK AI Chat</h1>
          <span
            className={`text-xs px-2 py-0.5 rounded-full ${
              authMode === "vault"
                ? "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400"
                : "bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400"
            }`}
          >
            {authMode === "vault" ? "Vault" : "BYOK"}
          </span>
          {/* Save status indicator (only in Ato environment) */}
          {isAtoEnvironment && (
            <span
              className={`text-xs px-2 py-0.5 rounded-full ${
                saveState === "saving"
                  ? "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400"
                  : saveState === "saved"
                    ? "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400"
                    : saveState === "error"
                      ? "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400"
                      : "bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400"
              }`}
            >
              {saveState === "saving"
                ? "保存中..."
                : saveState === "saved"
                  ? "保存済み"
                  : saveState === "error"
                    ? "エラー"
                    : ""}
            </span>
          )}
        </div>

        <div className="flex items-center gap-2">
          {/* Manual save button (only in Ato environment) */}
          {isAtoEnvironment && messages.length > 0 && (
            <button
              onClick={handleManualSave}
              disabled={saveState === "saving"}
              className="text-xs px-3 py-1.5 rounded-lg bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300 transition-colors disabled:opacity-50"
            >
              💾 保存
            </button>
          )}
          {isAtoEnvironment && (
            <button
              onClick={handleLoadLatest}
              disabled={loadState === "loading"}
              className="text-xs px-3 py-1.5 rounded-lg bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300 transition-colors disabled:opacity-50"
            >
              {loadState === "loading" ? "読み込み中..." : "📥 読み込み"}
            </button>
          )}
          {authMode === "byok" && (
            <button
              onClick={clearConfig}
              className="text-xs text-red-500 hover:text-red-600 transition-colors"
            >
              キーを削除
            </button>
          )}
        </div>
      </header>

      {isAtoEnvironment && lastLoadedPayload && (
        <div className="border-b border-gray-200 dark:border-gray-800 px-4 py-2">
          <div className="text-xs text-gray-500 mb-1">
            最新の読み込み: {lastLoadedPath}
          </div>
          <pre className="max-h-40 overflow-auto text-[11px] bg-gray-50 dark:bg-gray-900/40 rounded-lg p-2 text-gray-600 dark:text-gray-300">
            {lastLoadedPayload}
          </pre>
        </div>
      )}

      {/* Messages */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="text-center text-gray-400 mt-20">
            <p>メッセージを入力してチャットを開始</p>
          </div>
        )}

        {messages.map((m) => (
          <div
            key={m.id}
            className={`flex ${m.role === "user" ? "justify-end" : "justify-start"}`}
          >
            <div
              className={`max-w-[80%] p-4 rounded-2xl ${
                m.role === "user"
                  ? "bg-blue-600 text-white"
                  : "bg-gray-100 dark:bg-gray-800 text-gray-900 dark:text-gray-100"
              }`}
            >
              <p className="whitespace-pre-wrap">{m.content}</p>
            </div>
          </div>
        ))}

        {isLoading && (
          <div className="flex justify-start">
            <div className="bg-gray-100 dark:bg-gray-800 p-4 rounded-2xl">
              <div className="flex gap-1">
                <span
                  className="w-2 h-2 bg-gray-400 rounded-full animate-bounce"
                  style={{ animationDelay: "0ms" }}
                />
                <span
                  className="w-2 h-2 bg-gray-400 rounded-full animate-bounce"
                  style={{ animationDelay: "150ms" }}
                />
                <span
                  className="w-2 h-2 bg-gray-400 rounded-full animate-bounce"
                  style={{ animationDelay: "300ms" }}
                />
              </div>
            </div>
          </div>
        )}

        {(error || configError) && (
          <div className="text-red-500 text-sm bg-red-50 dark:bg-red-900/20 p-3 rounded-lg">
            {configError || "エラーが発生しました。APIキーを確認してください。"}
          </div>
        )}
      </div>

      {/* Input */}
      <form
        onSubmit={handleSubmit}
        className="p-4 border-t border-gray-200 dark:border-gray-800"
      >
        <div className="flex gap-2">
          <input
            className="flex-1 border border-gray-300 dark:border-gray-700 p-3 rounded-xl bg-white dark:bg-gray-900 focus:outline-none focus:ring-2 focus:ring-blue-500"
            value={input}
            onChange={handleInputChange}
            placeholder="メッセージを入力..."
            disabled={isLoading}
          />
          <button
            type="submit"
            disabled={isLoading || !input.trim()}
            className="bg-blue-600 hover:bg-blue-700 disabled:bg-gray-400 text-white px-6 py-3 rounded-xl font-medium transition-colors"
          >
            送信
          </button>
        </div>
      </form>
    </div>
  );
}
