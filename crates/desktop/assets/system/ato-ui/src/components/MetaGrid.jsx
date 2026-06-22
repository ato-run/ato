import React from "react";

/**
 * Two-column metadata grid used in ConsentScreen and CandidateDetailScreen.
 * rows: Array<{ label: string, value: string }>
 */
export function MetaGrid({ rows = [], className = "" }) {
  return (
    <div className={`rounded-[var(--radius-md)] border border-[var(--border)] overflow-hidden ${className}`}>
      {rows.map((row, i) => (
        <div
          key={i}
          className="flex items-baseline gap-3 px-3 py-2 border-b border-[var(--border-soft)] last:border-b-0 odd:bg-[var(--surface)] even:bg-[var(--bg)]"
        >
          <span className="text-[11.5px] text-[var(--muted)] flex-shrink-0 w-24 font-medium">
            {row.label}
          </span>
          <span
            className="text-[12px] text-[var(--text)] font-mono truncate"
            title={row.value}
          >
            {row.value || "—"}
          </span>
        </div>
      ))}
    </div>
  );
}
