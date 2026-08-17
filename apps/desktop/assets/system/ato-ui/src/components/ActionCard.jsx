import React from "react";

/**
 * Clickable action card used in NoCandidatesScreen and similar "choice" screens.
 * Shows icon, title, description, optional badge, and a right-arrow chevron.
 */
export function ActionCard({ icon, title, description, badge, onClick, className = "" }) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-3 p-3.5 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--bg)] text-left cursor-pointer transition-colors hover:bg-[var(--surface)] hover:border-[var(--accent-border)] group ${className}`}
    >
      {icon && (
        <div
          className="w-9 h-9 rounded-[9px] flex items-center justify-center flex-shrink-0"
          style={{ background: "var(--accent-light)", color: "var(--accent)" }}
        >
          {icon}
        </div>
      )}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 mb-0.5">
          <span className="text-[13px] font-semibold text-[var(--text)]">{title}</span>
          {badge}
        </div>
        {description && (
          <p className="text-[12px] text-[var(--muted)] m-0 leading-relaxed">
            {description}
          </p>
        )}
      </div>
      <svg
        viewBox="0 0 24 24"
        width="14"
        height="14"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="flex-shrink-0 text-[var(--soft)] group-hover:text-[var(--accent)] transition-colors"
      >
        <polyline points="9 18 15 12 9 6" />
      </svg>
    </button>
  );
}
