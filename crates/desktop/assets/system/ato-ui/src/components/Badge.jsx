import React from "react";

const BADGE_STYLES = {
  official: {
    bg: "var(--accent-light)",
    color: "var(--accent)",
    border: "var(--accent-border)",
    label: "公式",
  },
  community: {
    bg: "var(--ok-light)",
    color: "var(--ok)",
    border: "#bbf7d0",
    label: "コミュニティ",
  },
  inferred: {
    bg: "var(--warn-bg)",
    color: "var(--warn)",
    border: "#fde68a",
    label: "推定",
  },
  unverified: {
    bg: "var(--surface-2)",
    color: "var(--muted)",
    border: "var(--border)",
    label: "未確認",
  },
};

/**
 * Status badge for capsule candidate trust levels.
 * variant: "official" | "community" | "inferred" | "unverified"
 */
export function Badge({ variant = "unverified", children, className = "" }) {
  const s = BADGE_STYLES[variant] ?? BADGE_STYLES.unverified;
  const label = children ?? s.label;
  return (
    <span
      className={`inline-flex items-center px-1.5 py-0.5 rounded text-[11px] font-semibold border ${className}`}
      style={{ background: s.bg, color: s.color, borderColor: s.border }}
    >
      {label}
    </span>
  );
}
