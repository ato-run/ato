import React, { useState, useRef } from "react";

function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

function parseRepo(input) {
  const trimmed = input.trim();
  // Full GitHub URL
  const urlMatch = trimmed.match(/^https?:\/\/(?:www\.)?github\.com\/([^/]+\/[^/]+?)(?:\.git)?(?:\/.*)?$/i);
  if (urlMatch) return urlMatch[1];
  // owner/repo shorthand
  const shortMatch = trimmed.match(/^([a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+)$/);
  if (shortMatch) return shortMatch[1];
  return null;
}

const GithubIcon = () => (
  <svg viewBox="0 0 24 24" fill="currentColor" style={{ width: 20, height: 20 }}>
    <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844a9.59 9.59 0 0 1 2.504.337c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2z"/>
  </svg>
);

export function GithubRunScreen({ onCandidatesFound }) {
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [validRepo, setValidRepo] = useState(null);
  const abortRef = useRef(null);

  const handleInput = (v) => {
    setInput(v);
    setError(null);
    const repo = parseRepo(v);
    setValidRepo(repo);
  };

  const findCandidates = async () => {
    if (!validRepo) {
      setError("有効な GitHub URL または owner/repo 形式で入力してください。");
      return;
    }
    setLoading(true);
    setError(null);

    // Install callback BEFORE sending IPC so we never miss the response.
    const timeout = setTimeout(() => {
      setLoading(false);
      setError("タイムアウトしました。もう一度お試しください。");
      delete window.__ato_github_candidates_result;
    }, 30000);

    window.__ato_github_candidates_result = (result) => {
      clearTimeout(timeout);
      setLoading(false);
      delete window.__ato_github_candidates_result;
      if (result && result.ok) {
        onCandidatesFound(result.candidates || [], validRepo);
      } else {
        const msg = (result && result.error) ? result.error : "候補の取得に失敗しました。";
        setError(msg);
      }
    };

    abortRef.current = () => {
      clearTimeout(timeout);
      delete window.__ato_github_candidates_result;
      setLoading(false);
    };

    // Send IPC request to Rust to find candidates (after callback is ready).
    bridge({ kind: "github_find_candidates", repo: validRepo });
  };

  const handleKeyDown = (e) => {
    if (e.key === "Enter" && !loading) findCandidates();
  };

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)",
    }}>
      {/* Header */}
      <div style={{ padding: "28px 24px 0", textAlign: "center" }}>
        <div style={{
          width: 44, height: 44, borderRadius: 12, background: "var(--surface-2)",
          color: "var(--text)", display: "flex", alignItems: "center", justifyContent: "center", margin: "0 auto 12px",
        }}>
          <GithubIcon />
        </div>
        <h1 style={{ margin: 0, fontSize: 17, fontWeight: 700 }}>Run from GitHub</h1>
        <p style={{ margin: "6px 0 0", fontSize: 12.5, color: "var(--muted)" }}>
          GitHubリポジトリからcapsuleを探して実行します
        </p>
      </div>

      {/* Form */}
      <div style={{ flex: 1, overflowY: "auto", padding: "24px 24px 0" }}>
        <div style={{ marginBottom: 16 }}>
          <label style={{ display: "block", fontSize: 12, fontWeight: 600, marginBottom: 6, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
            GitHub リポジトリ
          </label>
          <input
            type="text"
            value={input}
            onChange={e => handleInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="https://github.com/owner/repo  または  owner/repo"
            disabled={loading}
            autoFocus
            style={{
              width: "100%", padding: "10px 12px", borderRadius: 9, fontSize: 13,
              border: `1px solid ${error ? "var(--danger)" : validRepo ? "var(--ok)" : "var(--border-soft)"}`,
              background: "var(--surface)", color: "var(--text)", outline: "none",
              transition: "border-color 0.15s",
            }}
          />
          {error && <div style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 5 }}>{error}</div>}
          {validRepo && !error && (
            <div style={{ fontSize: 11.5, color: "var(--ok)", marginTop: 5 }}>
              ✓ {validRepo}
            </div>
          )}
        </div>

        {/* Helper text */}
        <div style={{
          background: "var(--surface-2)", borderRadius: 9, padding: "12px 14px",
          fontSize: 11.5, color: "var(--muted)", lineHeight: 1.7,
        }}>
          <div style={{ fontWeight: 600, marginBottom: 4, color: "var(--text)" }}>事前確認について</div>
          <ul style={{ margin: 0, paddingLeft: 18, display: "flex", flexDirection: "column", gap: 2 }}>
            <li>候補の確認はメタデータのみを使用します</li>
            <li>クローン・インストール・ビルドは行いません</li>
            <li>起動はレビューを承認した後にのみ開始されます</li>
          </ul>
        </div>
      </div>

      {/* Footer */}
      <div style={{ padding: "16px 24px 20px", display: "flex", gap: 8, alignItems: "center" }}>
        <button
          onClick={() => bridge({ kind: "cancel" })}
          style={{ padding: "9px 18px", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", fontWeight: 500, color: "var(--text)" }}
        >キャンセル</button>
        <button
          onClick={loading ? abortRef.current : findCandidates}
          disabled={!loading && !validRepo}
          style={{
            flex: 1, padding: "9px 0", borderRadius: 8, border: "none",
            background: (!loading && !validRepo) ? "var(--surface-2)" : "var(--accent)",
            color: (!loading && !validRepo) ? "var(--soft)" : "#fff",
            fontSize: 13, fontWeight: 600, cursor: "pointer",
            display: "flex", alignItems: "center", justifyContent: "center", gap: 8,
            transition: "all 0.15s",
          }}
        >
          {loading && (
            <div style={{ width: 14, height: 14, border: "2px solid rgba(255,255,255,0.4)", borderTopColor: "#fff", borderRadius: "50%", animation: "spin 0.8s linear infinite" }} />
          )}
          {loading ? "検索中... （クリックでキャンセル）" : "候補を探す"}
        </button>
      </div>

      <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
    </div>
  );
}
