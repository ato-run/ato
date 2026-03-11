import type { RunPermissionMode } from "../types";

interface PermissionModeSelectorProps {
  name: string;
  value: RunPermissionMode;
  disabled?: boolean;
  onChange: (value: RunPermissionMode) => void;
}

interface PermissionModeOption {
  value: RunPermissionMode;
  label: string;
  tag: string;
  description: string;
}

interface PermissionModeMessage {
  tone: "" | "warn" | "error";
  text: string;
}

const PERMISSION_MODE_OPTIONS: PermissionModeOption[] = [
  {
    value: "standard",
    label: "Standard",
    tag: "Blocked",
    description: "Default runtime policy. Tier2 launches stay blocked until you choose an elevated permission mode.",
  },
  {
    value: "sandbox",
    label: "Sandbox",
    tag: "Recommended",
    description: "Runs Tier2 targets inside the native OS sandbox and keeps Ato safety barriers in place.",
  },
  {
    value: "dangerous",
    label: "Dangerous",
    tag: "High risk",
    description: "Bypasses the native sandbox and Ato permission barriers for this launch.",
  },
];

export function getPermissionModeMessage(mode: RunPermissionMode): PermissionModeMessage {
  if (mode === "dangerous") {
    return {
      tone: "warn",
      text: "Dangerous mode removes runtime permission barriers. Use it only when the capsule needs full host access.",
    };
  }
  if (mode === "sandbox") {
    return {
      tone: "",
      text: "Sandbox is the safest Tier2 option and is recommended for most native and Python targets.",
    };
  }
  return {
    tone: "error",
    text: "Tier2 launches stay blocked in Standard mode. Pick Sandbox or Dangerous before running.",
  };
}

export function PermissionModeSelector({
  name,
  value,
  disabled,
  onChange,
}: PermissionModeSelectorProps): JSX.Element {
  return (
    <div className="permission-mode-list" role="radiogroup" aria-label="Execution permissions">
      {PERMISSION_MODE_OPTIONS.map((option) => {
        const selected = option.value === value;
        return (
          <label
            key={option.value}
            className={`permission-mode-card permission-mode-${option.value} ${
              selected ? "selected" : ""
            } ${disabled ? "disabled" : ""}`}
          >
            <input
              className="permission-mode-input"
              type="radio"
              name={name}
              value={option.value}
              checked={selected}
              disabled={disabled}
              onChange={() => onChange(option.value)}
            />
            <span className="permission-mode-copy">
              <span className="permission-mode-head">
                <span className="permission-mode-title">{option.label}</span>
                <span className="permission-mode-tag">{option.tag}</span>
              </span>
              <span className="permission-mode-description">{option.description}</span>
            </span>
          </label>
        );
      })}
    </div>
  );
}
