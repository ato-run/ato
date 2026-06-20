import React from "react";

/**
 * Permission card used in ConsentScreen and CandidateDetailScreen.
 * Shows an icon with accent background, title, subtitle description, and
 * a chevron button to open a detail panel.
 */
export function PermCard({ icon, title, description, onExpand, className = "" }) {
  return (
    <div
      className={`flex items-start gap-2.5 p-3 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--bg)] ${className}`}
    >
      {/* Icon box */}
      <div
        className="w-9 h-9 rounded-[9px] flex items-center justify-center flex-shrink-0"
        style={{ background: "var(--accent-light)", color: "var(--accent)" }}
      >
        {icon}
      </div>

      {/* Body */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[13px] font-bold text-[var(--text)]">{title}</span>
          {onExpand && (
            <button
              onClick={onExpand}
              className="w-5 h-5 flex items-center justify-center rounded border-0 bg-transparent text-[var(--soft)] hover:bg-[var(--surface)] hover:text-[var(--text)] transition-colors cursor-pointer flex-shrink-0"
              aria-label="詳細を見る"
            >
              <svg
                viewBox="0 0 24 24"
                width="13"
                height="13"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
            </button>
          )}
        </div>
        {description && (
          <p className="mt-0.5 text-[11.5px] text-[var(--muted)] leading-relaxed truncate">
            {description}
          </p>
        )}
      </div>
    </div>
  );
}
