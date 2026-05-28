import React, { useState, useEffect, useRef, useCallback } from "react";

function bridge(cmd) {
  const msg = JSON.stringify({ capsule: "launch", command: cmd });
  if (window.ipc && window.ipc.postMessage) window.ipc.postMessage(msg);
  else console.log("[no bridge]", cmd);
}

function nowLabel() {
  const d = new Date();
  const p2 = n => String(n).padStart(2, "0");
  return `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`;
}

const STEP_LABELS = ["検証中", "解決中", "起動中", "接続中"];
const MAX_LOG_ENTRIES = 400;

export function BootScreen() {
  const boot = window.__ATO_BOOT && typeof window.__ATO_BOOT === "object" ? window.__ATO_BOOT : null;
  const displayName = boot?.handle || boot?.name || "";

  // steps: 0=pending, 1=active, 2=done
  const [steps, setSteps] = useState([0, 0, 0, 0]); // 0=pending, 1=active, 2=done
  const [logs, setLogs] = useState(["[" + nowLabel() + "] [detail] Launching capsule securely"]);
  const [showLogs, setShowLogs] = useState(false);
  const [failMsg, setFailMsg] = useState(null);
  const logsRef = useRef(null);

  const appendLog = useCallback((line, kind = "detail") => {
    if (!line) return;
    const entry = `[${nowLabel()}] [${kind}] ${line}`;
    setLogs(prev => {
      const next = [...prev, entry];
      return next.length > MAX_LOG_ENTRIES ? next.slice(next.length - MAX_LOG_ENTRIES) : next;
    });
  }, []);

  // Scroll logs to bottom on update
  useEffect(() => {
    if (showLogs && logsRef.current) {
      logsRef.current.scrollTop = logsRef.current.scrollHeight;
    }
  }, [logs, showLogs]);

  // Register global step/detail/fail handlers
  useEffect(() => {
    window.__atoStep = (n) => {
      const idx = Math.max(0, Math.min(3, n));
      setSteps(prev => prev.map((s, i) => {
        if (i < idx) return 2; // done
        if (i === idx) return 1; // active
        return 0; // pending
      }));
      appendLog(`${STEP_LABELS[idx]} フェーズに入りました`, "step");
    };

    window.__atoDetail = (line) => {
      if (typeof line === "string" && line.length > 0) appendLog(line, "detail");
    };

    window.__atoFail = (message) => {
      const msg = (typeof message === "string" && message.length > 0) ? message : "Launch failed";
      setFailMsg(msg);
      appendLog(`Failure: ${msg}`, "error");
      setShowLogs(true);
    };

    // Replay buffered events
    const pending = window.__atoPending ? window.__atoPending() : null;
    if (pending && typeof pending === "object") {
      if (Array.isArray(pending.details)) pending.details.forEach(l => window.__atoDetail(l));
      if (pending.step !== null) window.__atoStep(pending.step);
      if (pending.failure) window.__atoFail(pending.failure);
    } else if (pending !== null && pending !== undefined) {
      window.__atoStep(pending);
    }

    return () => {
      delete window.__atoStep;
      delete window.__atoDetail;
      delete window.__atoFail;
    };
  }, [appendLog]);

  useEffect(() => {
    const onKey = (e) => { if (e.key === "Escape") bridge({ kind: "abort_boot" }); };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const copyLogs = () => {
    const text = logs.join("\n");
    if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).catch(() => {});
    else {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.cssText = "position:fixed;left:-9999px";
      document.body.appendChild(ta);
      ta.select();
      try { document.execCommand("copy"); } catch (_) {}
      document.body.removeChild(ta);
    }
  };

  const stepState = (i) => {
    if (steps[i] === 2) return "done";
    if (steps[i] === 1) return "active";
    return "pending";
  };

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100vh",
      background: "var(--bg)", fontFamily: "var(--font-system)", fontSize: 13,
      color: "var(--text)", padding: "36px 30px 22px", gap: 18,
    }}>
      {/* Spinning capsule badge */}
      <div style={{ display: "flex", justifyContent: "center" }}>
        <div style={{
          width: 88, height: 88, borderRadius: "50%",
          background: "conic-gradient(from 0deg, #c4b5fd 0deg, #6366f1 300deg, transparent 360deg)",
          display: "flex", alignItems: "center", justifyContent: "center",
          animation: "spin 1.4s linear infinite",
        }}>
          <div style={{
            width: 78, height: 78, borderRadius: "50%",
            background: "var(--bg)", display: "flex", alignItems: "center", justifyContent: "center",
          }}>
            <div style={{
              width: 44, height: 24, borderRadius: 999,
              background: "linear-gradient(135deg, #818cf8 0%, #6366f1 100%)",
              position: "relative", boxShadow: "0 4px 10px rgba(99,102,241,0.40)",
            }}>
              <div style={{ position: "absolute", left: "50%", top: 0, bottom: 0, width: 2, background: "rgba(255,255,255,0.6)" }} />
            </div>
          </div>
        </div>
      </div>

      {/* Header */}
      <div style={{ textAlign: "center" }}>
        <h1 style={{ margin: 0, fontSize: 18, fontWeight: 700 }}>
          {displayName ? `${displayName} を起動中…` : "Launching capsule"}
        </h1>
        <p style={{ margin: "4px 0 0", fontSize: 12.5, color: "var(--muted)" }}>
          しばらくお待ちください
        </p>
      </div>

      {/* Error banner */}
      {failMsg && (
        <div style={{
          padding: "10px 14px", borderRadius: 10,
          background: "#fff5f5", border: "1px solid #fecaca",
          display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12,
        }}>
          <span style={{ fontSize: 12.5, color: "var(--danger)", fontWeight: 600, flex: 1 }}>{failMsg}</span>
          <button
            onClick={copyLogs}
            style={{ padding: "4px 10px", borderRadius: 6, border: "1px solid #fecaca", background: "transparent", fontSize: 11, color: "var(--danger)", cursor: "pointer" }}
          >Copy error</button>
        </div>
      )}

      {/* Steps */}
      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {STEP_LABELS.map((label, i) => {
          const state = stepState(i);
          return (
            <div key={i} style={{
              display: "flex", alignItems: "center", gap: 10,
              padding: "8px 10px", borderRadius: 8, fontSize: 12.5,
              background: state === "active" ? "#eef2ff" : "transparent",
              color: state === "pending" ? "var(--muted)" : "var(--text)",
              transition: "background 0.2s",
            }}>
              <div style={{ width: 18, height: 18, flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center" }}>
                {state === "done" && (
                  <div style={{ width: 18, height: 18, borderRadius: "50%", background: "#10b981", display: "flex", alignItems: "center", justifyContent: "center" }}>
                    <svg viewBox="0 0 24 24" fill="none" stroke="#fff" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" style={{ width: 11, height: 11 }}>
                      <polyline points="20 6 9 17 4 12"/>
                    </svg>
                  </div>
                )}
                {state === "active" && (
                  <div style={{ width: 18, height: 18, border: "2px solid var(--accent)", borderTopColor: "transparent", borderRadius: "50%", animation: "spin 0.9s linear infinite" }} />
                )}
                {state === "pending" && (
                  <div style={{ width: 18, height: 18, border: "1.5px solid var(--soft)", borderRadius: "50%" }} />
                )}
              </div>
              <span style={{ flex: 1 }}>{label}</span>
              <span style={{ fontSize: 11, color: "var(--soft)", fontFamily: "ui-monospace, monospace" }}>
                {state === "done" ? "完了" : state === "active" ? "進行中" : ""}
              </span>
            </div>
          );
        })}
      </div>

      {/* Action buttons */}
      <div style={{ display: "flex", gap: 8, justifyContent: "center" }}>
        <button
          onClick={() => setShowLogs(v => !v)}
          style={{
            height: 30, padding: "0 12px", border: "1px solid var(--border-soft)",
            borderRadius: 8, fontSize: 12, fontWeight: 600, cursor: "pointer",
            background: showLogs ? "var(--accent-light)" : "#fafafd",
            color: showLogs ? "var(--accent)" : "var(--muted)",
          }}
        >{showLogs ? "ログを隠す" : "ログを表示"}</button>
        <button
          onClick={() => bridge({ kind: "abort_boot" })}
          style={{ height: 30, padding: "0 12px", border: "1px solid var(--border-soft)", borderRadius: 8, fontSize: 12, fontWeight: 600, cursor: "pointer", background: "#fafafd", color: "var(--muted)" }}
        >中断する</button>
      </div>

      {/* Logs panel */}
      {showLogs && (
        <div style={{
          flex: 1, display: "flex", flexDirection: "column", minHeight: 0,
          background: "var(--surface)", border: "1px solid var(--border-soft)", borderRadius: 10, overflow: "hidden",
        }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "8px 12px", borderBottom: "1px solid var(--border-soft)" }}>
            <span style={{ fontSize: 12, fontWeight: 700 }}>Launch logs</span>
            <button onClick={copyLogs} style={{ padding: "3px 8px", borderRadius: 6, border: "1px solid var(--border-soft)", background: "var(--surface-2)", fontSize: 11, cursor: "pointer", color: "var(--muted)" }}>Copy</button>
          </div>
          <pre
            ref={logsRef}
            style={{ flex: 1, margin: 0, padding: "10px 12px", fontSize: 11, fontFamily: "ui-monospace, monospace", lineHeight: 1.7, color: "var(--text)", overflowY: "auto", whiteSpace: "pre-wrap", wordBreak: "break-all" }}
          >{logs.join("\n")}</pre>
          <div style={{ padding: "4px 12px 8px", fontSize: 10.5, color: "var(--muted)" }}>ログ領域は選択してコピーできます。</div>
        </div>
      )}

      {/* Footer */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 6, fontSize: 11, color: "var(--soft)" }}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ width: 12, height: 12 }}>
          <path d="M12 2l8 4v6c0 5-3.5 9-8 10-4.5-1-8-5-8-10V6l8-4z"/>
        </svg>
        <span>ato-desktop · 安全な起動環境</span>
      </div>

      <style>{`
        @keyframes spin { to { transform: rotate(360deg); } }
      `}</style>
    </div>
  );
}
