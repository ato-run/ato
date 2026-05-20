import React from "react";

/**
 * Wizard window header with icon, title, and optional subtitle.
 * Used at the top of all ato-launch screens.
 */
export function WizardHeader({ icon, title, subtitle, className = "" }) {
  return (
    <div className={`flex flex-col items-center gap-1.5 ${className}`}>
      {icon && (
        <div
          className="w-10 h-10 rounded-[var(--radius-md)] flex items-center justify-center"
          style={{ background: "var(--accent-light)", color: "var(--accent)" }}
        >
          {icon}
        </div>
      )}
      <h1 className="text-[15px] font-bold text-[var(--text)] m-0">{title}</h1>
      {subtitle && (
        <p className="text-[12px] text-[var(--muted)] text-center m-0 leading-relaxed max-w-[340px]">
          {subtitle}
        </p>
      )}
    </div>
  );
}
