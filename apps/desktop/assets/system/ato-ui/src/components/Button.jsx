import React from "react";

const VARIANTS = {
  primary: {
    base: "inline-flex items-center justify-center gap-1.5 px-4 py-2 rounded-[var(--radius-md)] text-[13px] font-semibold text-white cursor-pointer border-0 select-none transition-opacity",
    style: {
      background: "linear-gradient(135deg, #7c82f5 0%, #5558eb 100%)",
      boxShadow: "0 1px 4px rgba(91,95,239,0.25)",
    },
    disabled: "opacity-50 cursor-not-allowed",
    hover: "hover:opacity-90",
  },
  secondary: {
    base: "inline-flex items-center justify-center gap-1.5 px-4 py-2 rounded-[var(--radius-md)] text-[13px] font-medium cursor-pointer border select-none transition-colors",
    style: {
      background: "var(--surface)",
      color: "var(--text)",
      borderColor: "var(--border)",
    },
    disabled: "opacity-50 cursor-not-allowed",
    hover: "hover:bg-[var(--surface-2)]",
  },
  ghost: {
    base: "inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-[var(--radius-sm)] text-[13px] font-medium cursor-pointer border-0 select-none transition-colors",
    style: {
      background: "transparent",
      color: "var(--muted)",
    },
    disabled: "opacity-50 cursor-not-allowed",
    hover: "hover:bg-[var(--surface)] hover:text-[var(--text)]",
  },
  danger: {
    base: "inline-flex items-center justify-center gap-1.5 px-4 py-2 rounded-[var(--radius-md)] text-[13px] font-semibold text-white cursor-pointer border-0 select-none transition-opacity",
    style: { background: "var(--danger)" },
    disabled: "opacity-50 cursor-not-allowed",
    hover: "hover:opacity-90",
  },
};

export function Button({
  variant = "primary",
  disabled = false,
  children,
  className = "",
  style: extraStyle = {},
  ...props
}) {
  const v = VARIANTS[variant] ?? VARIANTS.primary;
  const cls = [v.base, disabled ? v.disabled : v.hover, className]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      className={cls}
      style={{ ...v.style, ...extraStyle }}
      disabled={disabled}
      {...props}
    >
      {children}
    </button>
  );
}
