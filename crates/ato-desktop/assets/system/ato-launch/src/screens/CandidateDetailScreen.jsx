import React, { useState } from "react";

function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

const STATUS_CONFIG = {
  official:   { label: "Official",   bg: "#f0fdf4", color: "#15803d", border: "#bbf7d0" },
  community:  { label: "Community",  bg: "#eff6ff", color: "#1d4ed8", border: "#bfdbfe" },
  inferred:   { label: "Inferred",   bg: "#fefce8", color: "#a16207", border: "#fde68a" },
  unverified: { label: "Unverified", bg: "var(--surface-2)", color: "var(--muted)", border: "var(--border-soft)" },
};

function Badge({ status }) {
  const cfg = STATUS_CONFIG[status] || STATUS_CONFIG.unverified;
  return (
    <span style={{ display: "inline-block", padding: "2px 8px", borderRadius: 999, fontSize: 10.5, fontWeight: 700, background: cfg.bg, color: cfg.color, border: `1px solid ${cfg.border}` }}>
      {cfg.label}
    </span>
  );
}

// Minimal TOML syntax highlighter
function TomlViewer({ content }) {
  if (!content) return <span style={{ color: "var(--muted)" }}>(empty)</span>;
  const highlighted = content.split("\n").map((line, i) => {
    let color = "var(--text)";
    let style = {};
    if (/^\s*#/.test(line)) { color = "var(--muted)"; style = { fontStyle: "italic" }; }
    else if (/^\s*\[/.test(line)) color = "var(--accent)";
    else if (/=/.test(line)) {
      const [k, ...v] = line.split("=");
      return (
        <div key={i}>
          <span style={{ color: "var(--text)", fontWeight: 600 }}>{k}</span>
          <span style={{ color: "var(--soft)" }}>=</span>
          <span style={{ color: "#0a8" }}>{v.join("=")}</span>
        </div>
      );
    }
    return <div key={i} style={{ color, ...style }}>{line || " "}</div>;
  });
  return <>{highlighted}</>;
}

export function CandidateDetailScreen({ candidate, onBack, onProceed }) {
  const [tab, setTab] = useState("structured");
  const [proceedError, setProceedError] = useState(null);
  const [proceeding, setProceeding] = useState(false);
  if (!candidate) return null;

  function handleProceed() {
    if (!candidate.repo) {
      setProceedError("リポジトリ情報が不足しています。候補選択からやり直してください。");
      return;
    }
    setProceedError(null);
    setProceeding(true);
    window.__ato_github_proceed_result = (result) => {
      if (result && result.ok === false) {
        setProceedError(result.error || "capsule.toml の検証に失敗しました。");
        setProceeding(false);
      }
      delete window.__ato_github_proceed_result;
    };
    bridge({
      kind: "github_proceed_to_consent",
      repo: candidate.repo,
      title: candidate.title || candidate.repo,
      manifest_toml: candidate.toml || "",
      manifest_source: candidate.manifest_source || (candidate.source === "github" ? "repo" : "user_edited"),
      requested_ref: candidate.requested_ref || "HEAD",
    });
    if (typeof onProceed === "function") onProceed();
  }

  const metaFields = [
    { label: "Name",       value: candidate.title || "—" },
    { label: "Author",     value: candidate.author || "—" },
    { label: "Source",     value: candidate.source === "registry" ? "Ato Registry" : "GitHub" },
    { label: "Status",     value: <Badge status={candidate.status} /> },
    { label: "Version",    value: candidate.version || "—" },
    { label: "Description", value: candidate.description || "—" },
  ];

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)",
    }}>
      {/* Header */}
      <div style={{ padding: "16px 20px 12px", borderBottom: "1px solid var(--border-soft)" }}>
        <button
          onClick={onBack}
          style={{ background: "none", border: "none", cursor: "pointer", fontSize: 12, color: "var(--accent)", padding: "0 0 8px", display: "flex", alignItems: "center", gap: 4 }}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 13, height: 13 }}>
            <polyline points="15 18 9 12 15 6"/>
          </svg>
          候補一覧に戻る
        </button>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <h1 style={{ margin: 0, fontSize: 16, fontWeight: 700, flex: 1 }}>{candidate.title || "capsule.toml"}</h1>
          <Badge status={candidate.status} />
        </div>
        {candidate.description && (
          <p style={{ margin: "6px 0 0", fontSize: 12, color: "var(--muted)" }}>{candidate.description}</p>
        )}
      </div>

      {/* Tab bar */}
      <div style={{ display: "flex", gap: 4, padding: "8px 20px 0", borderBottom: "1px solid var(--border-soft)" }}>
        {["structured", "raw"].map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              padding: "6px 14px", borderRadius: "8px 8px 0 0", border: "none", cursor: "pointer",
              fontSize: 12, fontWeight: tab === t ? 700 : 500,
              background: tab === t ? "var(--accent-light)" : "transparent",
              color: tab === t ? "var(--accent)" : "var(--muted)",
              borderBottom: tab === t ? "2px solid var(--accent)" : "2px solid transparent",
            }}
          >
            {t === "structured" ? "構造表示" : "Raw TOML"}
          </button>
        ))}
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflowY: "auto", padding: 20 }}>
        {tab === "structured" ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 0, borderRadius: 10, overflow: "hidden", border: "1px solid var(--border-soft)" }}>
            {metaFields.map(({ label, value }) => (
              <div key={label} style={{ display: "flex", gap: 12, padding: "10px 14px", borderBottom: "1px solid var(--border-soft)", alignItems: "flex-start" }}>
                <span style={{ fontSize: 12, color: "var(--muted)", width: 90, flexShrink: 0 }}>{label}</span>
                <span style={{ fontSize: 12, fontWeight: 500, flex: 1 }}>{value}</span>
              </div>
            ))}
            {candidate.permissions && (
              <div style={{ padding: "10px 14px" }}>
                <div style={{ fontSize: 12, color: "var(--muted)", marginBottom: 6 }}>Permissions summary</div>
                <div style={{ fontSize: 12 }}>{candidate.permissions}</div>
              </div>
            )}
          </div>
        ) : (
          <div style={{ background: "var(--surface)", borderRadius: 10, border: "1px solid var(--border-soft)", padding: 14 }}>
            <pre style={{ margin: 0, fontSize: 12, fontFamily: "ui-monospace, monospace", lineHeight: 1.7, overflowX: "auto", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
              {candidate.toml ? <TomlViewer content={candidate.toml} /> : <span style={{ color: "var(--muted)" }}>（TOML内容が取得できませんでした）</span>}
            </pre>
          </div>
        )}
      </div>

      {/* Footer */}
      <div style={{ padding: "12px 20px", display: "flex", gap: 8, borderTop: "1px solid var(--border-soft)" }}>
        <button
          onClick={onBack}
          style={{ padding: "9px 18px", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", fontWeight: 500 }}
        >戻る</button>
        <button
          onClick={handleProceed}
          disabled={proceeding}
          style={{
            flex: 1, padding: "9px 0", borderRadius: 8, border: "none",
            background: proceeding ? "var(--surface-2)" : "var(--accent)",
            color: proceeding ? "var(--soft)" : "#fff",
            fontSize: 13, fontWeight: 600, cursor: proceeding ? "not-allowed" : "pointer",
          }}
        >{proceeding ? "検証中..." : "起動レビューへ進む"}</button>
      </div>
      {proceedError && (
        <div style={{ padding: "0 20px 12px", fontSize: 11.5, color: "var(--danger)" }}>{proceedError}</div>
      )}
    </div>
  );
}
