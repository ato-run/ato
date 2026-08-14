import React from "react";

function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

export function NoCandidatesScreen({ repo, onCliInference, onCreateManually, onReviewUrl }) {
  const actions = [
    {
      key: "cli",
      icon: (
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 20, height: 20 }}>
          <rect x="3" y="3" width="18" height="18" rx="2"/>
          <polyline points="7 12 10 15 17 8"/>
        </svg>
      ),
      title: "CLI 推論を使う",
      desc: "Ato CLI がリポジトリを解析して capsule.toml の下書きを生成します。",
      badge: "推奨",
      badgeColor: "var(--accent)",
      onClick: onCliInference,
    },
    {
      key: "manual",
      icon: (
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 20, height: 20 }}>
          <path d="M12 20h9"/>
          <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/>
        </svg>
      ),
      title: "手動で作成する",
      desc: "テンプレートから始めるか、ゼロから capsule.toml を記述します。",
      badge: null,
      onClick: onCreateManually,
    },
    {
      key: "url",
      icon: (
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 20, height: 20 }}>
          <circle cx="11" cy="11" r="8"/>
          <line x1="21" y1="21" x2="16.65" y2="16.65"/>
        </svg>
      ),
      title: "URL を見直す",
      desc: "別のリポジトリやブランチを指定して、もう一度候補を検索します。",
      badge: null,
      onClick: onReviewUrl,
    },
  ];

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)",
    }}>
      {/* Header */}
      <div style={{ padding: "28px 24px 0", textAlign: "center" }}>
        <div style={{
          width: 44, height: 44, borderRadius: 12, background: "#fefce8",
          color: "#a16207", display: "flex", alignItems: "center", justifyContent: "center", margin: "0 auto 12px",
          fontSize: 22,
        }}>
          ⚠
        </div>
        <h1 style={{ margin: 0, fontSize: 17, fontWeight: 700 }}>候補が見つかりませんでした</h1>
        <p style={{ margin: "6px 0 0", fontSize: 12.5, color: "var(--muted)" }}>
          <code style={{ fontFamily: "ui-monospace, monospace", fontSize: 12, background: "var(--surface-2)", padding: "1px 6px", borderRadius: 4 }}>{repo}</code> に capsule.toml の候補がありません
        </p>
      </div>

      {/* Actions */}
      <div style={{ flex: 1, overflowY: "auto", padding: "24px 24px 0" }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          {actions.map(a => (
            <button
              key={a.key}
              onClick={a.onClick}
              style={{
                display: "flex", alignItems: "flex-start", gap: 14,
                padding: "14px 14px", borderRadius: 10, width: "100%",
                background: "var(--surface)", border: "1px solid var(--border-soft)",
                cursor: "pointer", textAlign: "left", transition: "all 0.15s",
              }}
              onMouseEnter={e => { e.currentTarget.style.borderColor = "var(--accent)"; e.currentTarget.style.background = "var(--accent-light)"; }}
              onMouseLeave={e => { e.currentTarget.style.borderColor = "var(--border-soft)"; e.currentTarget.style.background = "var(--surface)"; }}
            >
              <div style={{
                width: 36, height: 36, borderRadius: 9, background: "var(--surface-2)",
                display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
                color: "var(--muted)",
              }}>{a.icon}</div>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 3 }}>
                  <span style={{ fontWeight: 700, fontSize: 13.5 }}>{a.title}</span>
                  {a.badge && (
                    <span style={{ fontSize: 10, fontWeight: 700, padding: "1px 7px", borderRadius: 999, background: "var(--accent-light)", color: a.badgeColor, border: `1px solid var(--accent-border)` }}>{a.badge}</span>
                  )}
                </div>
                <div style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.55 }}>{a.desc}</div>
              </div>
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 14, height: 14, color: "var(--muted)", flexShrink: 0, marginTop: 3 }}>
                <polyline points="9 18 15 12 9 6"/>
              </svg>
            </button>
          ))}
        </div>
      </div>

      {/* Footer cancel */}
      <div style={{ padding: "16px 24px 20px" }}>
        <button
          onClick={() => bridge({ kind: "cancel" })}
          style={{ width: "100%", padding: "9px 0", borderRadius: 8, border: "1px solid var(--border-soft)", background: "var(--surface)", fontSize: 13, cursor: "pointer", fontWeight: 500, color: "var(--muted)" }}
        >キャンセル</button>
      </div>
    </div>
  );
}
