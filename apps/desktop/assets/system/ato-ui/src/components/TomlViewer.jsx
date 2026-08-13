import React, { useState } from "react";

/**
 * TOML syntax highlighter / viewer.
 * Shows a "Structured" tab (key-value pairs) and a "Raw" tab (plain text).
 */

function highlightToml(text) {
  if (!text) return "";
  return text
    .split("\n")
    .map((line) => {
      // Section headers [section]
      if (/^\s*\[/.test(line)) {
        return `<span class="toml-section">${esc(line)}</span>`;
      }
      // Comments
      if (/^\s*#/.test(line)) {
        return `<span class="toml-comment">${esc(line)}</span>`;
      }
      // Key = "value" | key = 123 | key = true
      const kv = line.match(/^(\s*)([\w.-]+)(\s*=\s*)(.*)$/);
      if (kv) {
        const val = kv[4];
        let valSpan;
        if (/^"/.test(val.trim())) {
          valSpan = `<span class="toml-string">${esc(val)}</span>`;
        } else if (/^(true|false)$/.test(val.trim())) {
          valSpan = `<span class="toml-bool">${esc(val)}</span>`;
        } else if (/^-?\d/.test(val.trim())) {
          valSpan = `<span class="toml-number">${esc(val)}</span>`;
        } else {
          valSpan = esc(val);
        }
        return `${esc(kv[1])}<span class="toml-key">${esc(kv[2])}</span>${esc(kv[3])}${valSpan}`;
      }
      return esc(line);
    })
    .join("\n");
}

function esc(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

const TOML_CSS = `
.toml-section { color: var(--accent); font-weight: 600; }
.toml-comment { color: var(--soft); font-style: italic; }
.toml-key     { color: #0ea5e9; }
.toml-string  { color: #16a34a; }
.toml-number  { color: #d97706; }
.toml-bool    { color: #9333ea; }
`;

export function TomlViewer({ content = "", className = "" }) {
  const [tab, setTab] = useState("highlighted");

  const tabCls = (t) =>
    `px-3 py-1 text-[12px] font-medium rounded-t cursor-pointer border-b-2 transition-colors ${
      tab === t
        ? "border-[var(--accent)] text-[var(--accent)]"
        : "border-transparent text-[var(--muted)] hover:text-[var(--text)]"
    }`;

  return (
    <div
      className={`flex flex-col rounded-[var(--radius-md)] border border-[var(--border)] overflow-hidden ${className}`}
    >
      <style>{TOML_CSS}</style>

      {/* Tab bar */}
      <div className="flex gap-1 px-2 pt-1 bg-[var(--surface)] border-b border-[var(--border)]">
        <button className={tabCls("highlighted")} onClick={() => setTab("highlighted")}>
          表示
        </button>
        <button className={tabCls("raw")} onClick={() => setTab("raw")}>
          Raw
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto bg-[var(--bg)]">
        {tab === "highlighted" ? (
          <pre
            className="m-0 p-3 text-[12px] leading-relaxed whitespace-pre-wrap break-words"
            style={{ fontFamily: "var(--font-mono)" }}
            dangerouslySetInnerHTML={{ __html: highlightToml(content) }}
          />
        ) : (
          <pre
            className="m-0 p-3 text-[12px] leading-relaxed text-[var(--text)] whitespace-pre-wrap break-words"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {content}
          </pre>
        )}
      </div>
    </div>
  );
}
