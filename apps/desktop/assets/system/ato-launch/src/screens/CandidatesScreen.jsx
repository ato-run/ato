import React from "react";

const STATUS_CONFIG = {
  official:    { label: "Official",    bg: "#f0fdf4", color: "#15803d", border: "#bbf7d0" },
  community:   { label: "Community",   bg: "#eff6ff", color: "#1d4ed8", border: "#bfdbfe" },
  inferred:    { label: "Inferred",    bg: "#fefce8", color: "#a16207", border: "#fde68a" },
  unverified:  { label: "Unverified",  bg: "var(--surface-2)", color: "var(--muted)", border: "var(--border-soft)" },
};

function Badge({ status }) {
  const cfg = STATUS_CONFIG[status] || STATUS_CONFIG.unverified;
  return (
    <span style={{
      display: "inline-block", padding: "2px 8px", borderRadius: 999,
      fontSize: 10.5, fontWeight: 700, letterSpacing: "0.03em",
      background: cfg.bg, color: cfg.color, border: `1px solid ${cfg.border}`,
    }}>{cfg.label}</span>
  );
}

export function CandidatesScreen({ candidates, repo, onSelect, onCreateOwn }) {
  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13, color: "var(--text)",
    }}>
      {/* Header */}
      <div style={{ padding: "22px 20px 16px", borderBottom: "1px solid var(--border-soft)" }}>
        <h1 style={{ margin: "0 0 4px", fontSize: 16, fontWeight: 700 }}>capsule.toml の候補</h1>
        <div style={{ fontSize: 12, color: "var(--muted)" }}>
          {repo} · {candidates.length} 件見つかりました
        </div>
      </div>

      {/* Candidates list */}
      <div style={{ flex: 1, overflowY: "auto", padding: "12px 20px" }}>
        {candidates.map((c, i) => (
          <button
            key={i}
            onClick={() => onSelect(i)}
            style={{
              display: "flex", flexDirection: "column", gap: 6, width: "100%",
              padding: "14px 14px", borderRadius: 10, marginBottom: 8,
              background: "var(--surface)", border: "1px solid var(--border-soft)",
              textAlign: "left", cursor: "pointer", transition: "all 0.15s",
            }}
            onMouseEnter={e => e.currentTarget.style.borderColor = "var(--accent)"}
            onMouseLeave={e => e.currentTarget.style.borderColor = "var(--border-soft)"}
          >
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
              <span style={{ fontWeight: 700, fontSize: 13.5 }}>{c.title || "capsule.toml"}</span>
              <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                <Badge status={c.status} />
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ width: 13, height: 13, color: "var(--muted)", flexShrink: 0 }}>
                  <polyline points="9 18 15 12 9 6"/>
                </svg>
              </div>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 12, fontSize: 11.5, color: "var(--muted)" }}>
              <span>{c.author || "—"}</span>
              <span style={{ color: "var(--border-soft)" }}>·</span>
              <span>{c.source === "registry" ? "Ato Registry" : "GitHub"}</span>
              {c.popularity && <><span style={{ color: "var(--border-soft)" }}>·</span><span>★ {c.popularity}</span></>}
            </div>
            {c.description && (
              <div style={{ fontSize: 12, color: "var(--muted)", lineHeight: 1.55, marginTop: 2 }}>{c.description}</div>
            )}
          </button>
        ))}

        {/* Create own capsule.toml */}
        <div style={{ borderTop: "1px solid var(--border-soft)", marginTop: 8, paddingTop: 14 }}>
          <button
            onClick={onCreateOwn}
            style={{
              display: "flex", alignItems: "center", gap: 12, width: "100%",
              padding: "12px 14px", borderRadius: 10,
              background: "transparent", border: "1.5px dashed var(--border-soft)",
              cursor: "pointer", textAlign: "left", transition: "all 0.15s",
            }}
            onMouseEnter={e => { e.currentTarget.style.borderColor = "var(--accent)"; e.currentTarget.style.background = "var(--accent-light)"; }}
            onMouseLeave={e => { e.currentTarget.style.borderColor = "var(--border-soft)"; e.currentTarget.style.background = "transparent"; }}
          >
            <div style={{ width: 32, height: 32, borderRadius: 8, background: "var(--surface-2)", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, color: "var(--muted)", fontSize: 18, fontWeight: 300 }}>+</div>
            <div>
              <div style={{ fontWeight: 600, fontSize: 13 }}>自分の capsule.toml を作成する</div>
              <div style={{ fontSize: 11.5, color: "var(--muted)", marginTop: 2 }}>テンプレートから作成・CLIで推論・手動で記述</div>
            </div>
          </button>
        </div>
      </div>
    </div>
  );
}
