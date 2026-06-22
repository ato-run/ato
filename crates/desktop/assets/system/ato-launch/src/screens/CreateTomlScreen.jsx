import React, { useState, useEffect } from "react";

function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

const TEMPLATES = {
  web: `[capsule]
name = "my-web-app"
version = "0.1.0"
description = "A web application"

[execution]
runtime = "node"
entry = "src/index.js"
port = 3000

[permissions]
network = ["localhost:3000"]
`,
  cli: `[capsule]
name = "my-cli-tool"
version = "0.1.0"
description = "A command-line tool"

[execution]
runtime = "node"
entry = "bin/cli.js"
`,
  python: `[capsule]
name = "my-python-app"
version = "0.1.0"
description = "A Python application"

[execution]
runtime = "python"
entry = "main.py"

[permissions]
network = []
`,
  blank: `[capsule]
name = "my-capsule"
version = "0.1.0"
description = ""

[execution]
runtime = ""
entry = ""
`,
};

export function CreateTomlScreen({ initialContent, repo, onSave, onCancel }) {
  const isCliInference = initialContent === "__cli_inference__";

  const [tab, setTab] = useState(isCliInference ? "inference" : initialContent !== null && initialContent !== "" ? "editor" : "template");
  const [content, setContent] = useState(
    isCliInference ? "" : initialContent || TEMPLATES.web
  );
  const [selectedTemplate, setSelectedTemplate] = useState("web");
  const [inferring, setInferring] = useState(false);
  const [inferred, setInferred] = useState(false);
  const [manifestSource, setManifestSource] = useState(isCliInference ? "inferred_fallback" : "user_edited");
  const [inferenceError, setInferenceError] = useState(null);

  // When tab switches to template, sync content
  useEffect(() => {
    if (tab === "template") {
      setContent(TEMPLATES[selectedTemplate]);
      setManifestSource("user_edited");
    }
  }, [tab, selectedTemplate]);

  const handleTemplateSelect = (key) => {
    setSelectedTemplate(key);
    setContent(TEMPLATES[key]);
    setManifestSource("user_edited");
  };

  const runCliInference = () => {
    setInferring(true);
    setInferenceError(null);
    bridge({ kind: "github_cli_inference", repo: repo || "" });
    window.__ato_cli_inference_result = (result) => {
      if (result && result.ok && result.toml) {
        setContent(result.toml);
        setInferring(false);
        setInferred(true);
        setManifestSource("inferred_fallback");
        setInferenceError(null);
        setTab("editor");
      } else {
        const msg = (result && result.message) || "推論に失敗しました。もう一度お試しいただくか、手動で作成してください。";
        setInferenceError(msg);
        setInferring(false);
        setInferred(false);
      }
      delete window.__ato_cli_inference_result;
    };
    setTimeout(() => {
      if (window.__ato_cli_inference_result) {
        delete window.__ato_cli_inference_result;
        setInferenceError("推論がタイムアウトしました。ネットワーク接続を確認してください。");
        setInferring(false);
      }
    }, 30000);
  };

  const tabs = [
    { key: "template", label: "テンプレート" },
    { key: "inference", label: "CLI 推論" },
    { key: "editor",   label: "エディタ" },
  ];

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)",
    }}>
      {/* Header */}
      <div style={{ padding: "18px 20px 0" }}>
        <h1 style={{ margin: "0 0 4px", fontSize: 16, fontWeight: 700 }}>capsule.toml を作成・編集</h1>
        <p style={{ margin: 0, fontSize: 12, color: "var(--muted)" }}>作成後、通常の検査・レビュー・起動フローに進みます</p>
      </div>

      {/* Tab bar */}
      <div style={{ display: "flex", gap: 4, padding: "12px 20px 0" }}>
        {tabs.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            style={{
              padding: "6px 14px", borderRadius: "8px 8px 0 0", border: "none", cursor: "pointer",
              fontSize: 12, fontWeight: tab === t.key ? 700 : 500,
              background: tab === t.key ? "var(--accent-light)" : "var(--surface-2)",
              color: tab === t.key ? "var(--accent)" : "var(--muted)",
              borderBottom: tab === t.key ? "2px solid var(--accent)" : "2px solid transparent",
            }}
          >{t.label}</button>
        ))}
      </div>
      <div style={{ borderTop: "1px solid var(--border-soft)", marginBottom: 0 }} />

      {/* Content */}
      <div style={{ flex: 1, overflowY: "auto", padding: "16px 20px 0", display: "flex", flexDirection: "column" }}>
        {tab === "template" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <div style={{ fontSize: 12, color: "var(--muted)" }}>テンプレートを選択してください：</div>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
              {Object.keys(TEMPLATES).map(key => (
                <button
                  key={key}
                  onClick={() => handleTemplateSelect(key)}
                  style={{
                    padding: "10px 14px", borderRadius: 9, border: `1.5px solid ${selectedTemplate === key ? "var(--accent)" : "var(--border-soft)"}`,
                    background: selectedTemplate === key ? "var(--accent-light)" : "var(--surface)",
                    cursor: "pointer", textAlign: "left", fontSize: 12, fontWeight: 600,
                    color: selectedTemplate === key ? "var(--accent)" : "var(--text)",
                    transition: "all 0.15s",
                  }}
                >
                  {{ web: "🌐 Web App", cli: "⚡ CLI Tool", python: "🐍 Python App", blank: "📄 Blank" }[key]}
                </button>
              ))}
            </div>
            <div style={{ background: "var(--surface)", borderRadius: 9, border: "1px solid var(--border-soft)", overflow: "hidden", flex: 1 }}>
              <div style={{ padding: "8px 12px", borderBottom: "1px solid var(--border-soft)", fontSize: 11, color: "var(--muted)", fontWeight: 600 }}>プレビュー</div>
              <pre style={{ margin: 0, padding: "10px 12px", fontSize: 11.5, fontFamily: "ui-monospace, monospace", lineHeight: 1.7, overflowX: "auto", whiteSpace: "pre", color: "var(--text)" }}>{content}</pre>
            </div>
          </div>
        )}

        {tab === "inference" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 16, alignItems: "center", textAlign: "center", padding: "20px 0" }}>
            <div style={{ width: 52, height: 52, borderRadius: 12, background: "var(--surface-2)", display: "flex", alignItems: "center", justifyContent: "center", fontSize: 24 }}>🤖</div>
            <div>
              <div style={{ fontWeight: 700, fontSize: 14, marginBottom: 6 }}>CLI 推論</div>
              <div style={{ fontSize: 12.5, color: "var(--muted)", lineHeight: 1.7, maxWidth: 300 }}>
                Ato CLI がリポジトリのソースを解析して<br />capsule.toml の下書きを自動生成します。<br />生成後にエディタで編集できます。
              </div>
              {inferred && <div style={{ fontSize: 12, color: "var(--ok)", marginTop: 8, fontWeight: 600 }}>✓ 推論完了 — エディタで確認してください</div>}
              {inferenceError && <div style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 8, lineHeight: 1.6, maxWidth: 280 }}>{inferenceError}</div>}
            </div>
            <button
              onClick={inferring ? undefined : runCliInference}
              disabled={inferring}
              style={{
                padding: "10px 28px", borderRadius: 9, border: "none", fontSize: 13, fontWeight: 600, cursor: inferring ? "not-allowed" : "pointer",
                background: inferring ? "var(--surface-2)" : "var(--accent)", color: inferring ? "var(--soft)" : "#fff",
                display: "flex", alignItems: "center", gap: 8,
              }}
            >
              {inferring && <div style={{ width: 14, height: 14, border: "2px solid rgba(255,255,255,0.4)", borderTopColor: "#fff", borderRadius: "50%", animation: "spin 0.8s linear infinite" }} />}
              {inferring ? "推論中…" : "CLI 推論を実行"}
            </button>
          </div>
        )}

        {tab === "editor" && (
          <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 8 }}>
            <div style={{ fontSize: 12, color: "var(--muted)" }}>
              capsule.toml の内容を編集してください。
            </div>
            <textarea
              value={content}
              onChange={e => {
                setContent(e.target.value);
                setManifestSource("user_edited");
              }}
              spellCheck={false}
              style={{
                flex: 1, minHeight: 260, padding: "12px 14px", borderRadius: 9, resize: "none",
                border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 12.5,
                fontFamily: "ui-monospace, 'SF Mono', monospace", lineHeight: 1.7,
                color: "var(--text)", outline: "none", userSelect: "text",
              }}
            />
          </div>
        )}
      </div>

      {/* Footer */}
      <div style={{ padding: "12px 20px 20px", display: "flex", gap: 8, borderTop: "1px solid var(--border-soft)" }}>
        <button onClick={onCancel} style={{ padding: "9px 18px", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", fontWeight: 500 }}>
          キャンセル
        </button>
        <button
          onClick={() => onSave(content, { manifest_source: manifestSource })}
          disabled={!content.trim()}
          style={{
            flex: 1, padding: "9px 0", borderRadius: 8, border: "none",
            background: content.trim() ? "var(--accent)" : "var(--surface-2)",
            color: content.trim() ? "#fff" : "var(--soft)",
            fontSize: 13, fontWeight: 600, cursor: content.trim() ? "pointer" : "not-allowed",
          }}
        >
          保存してレビューへ
        </button>
      </div>

      <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
    </div>
  );
}
