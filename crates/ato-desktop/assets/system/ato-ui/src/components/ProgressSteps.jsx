import React from "react";

const STEP_STATES = {
  pending: { ring: "border-[var(--border)]", dot: "bg-[var(--border)]", label: "text-[var(--soft)]" },
  active: { ring: "border-[var(--accent)]", dot: "bg-[var(--accent)]", label: "text-[var(--accent)] font-semibold" },
  done: { ring: "border-[var(--ok)]", dot: "bg-[var(--ok)]", label: "text-[var(--muted)]" },
  error: { ring: "border-[var(--danger)]", dot: "bg-[var(--danger)]", label: "text-[var(--danger)] font-semibold" },
};

/**
 * Boot progress step list.
 * steps: Array<{ id, label, state: "pending"|"active"|"done"|"error" }>
 */
export function ProgressSteps({ steps = [] }) {
  return (
    <ol className="flex flex-col gap-2.5">
      {steps.map((step, i) => {
        const s = STEP_STATES[step.state] ?? STEP_STATES.pending;
        return (
          <li key={step.id ?? i} className="flex items-center gap-3">
            {/* Ring indicator */}
            <span
              className={`w-4 h-4 rounded-full border-2 flex items-center justify-center flex-shrink-0 ${s.ring}`}
            >
              {step.state === "done" && (
                <svg viewBox="0 0 12 12" width="8" height="8" fill="none">
                  <polyline
                    points="2 6 5 9 10 3"
                    stroke="var(--ok)"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  />
                </svg>
              )}
              {step.state === "active" && (
                <span className="w-2 h-2 rounded-full bg-[var(--accent)] animate-pulse" />
              )}
              {step.state === "error" && (
                <span className="w-2 h-2 rounded-full bg-[var(--danger)]" />
              )}
            </span>
            <span className={`text-[13px] ${s.label}`}>{step.label}</span>
          </li>
        );
      })}
    </ol>
  );
}
