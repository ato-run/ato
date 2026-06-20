import { X } from "lucide-react";

interface ConfirmActionModalProps {
  open: boolean;
  title: string;
  lines: string[];
  extraContent?: JSX.Element | null;
  authRequired: boolean;
  authToken: string;
  isSubmitting: boolean;
  confirmLabel: string;
  onAuthTokenChange: (value: string) => void;
  onClose: () => void;
  onConfirm: () => void;
  confirmDisabled: boolean;
}

export function ConfirmActionModal({
  open,
  title,
  lines,
  extraContent,
  authRequired,
  authToken,
  isSubmitting,
  confirmLabel,
  onAuthTokenChange,
  onClose,
  onConfirm,
  confirmDisabled,
}: ConfirmActionModalProps): JSX.Element | null {
  if (!open) {
    return null;
  }

  return (
    <div
      className="confirm-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Action confirmation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !isSubmitting) {
          onClose();
        }
      }}
    >
      <div className="confirm-card">
        <div className="confirm-head">
          <h3 className="confirm-title">{title}</h3>
          <button
            className="icon-btn"
            type="button"
            aria-label="Close confirmation"
            onClick={onClose}
            disabled={isSubmitting}
          >
            <X size={14} strokeWidth={1.5} />
          </button>
        </div>
        <div className="confirm-body mono">
          {lines.map((line, index) => (
            <div key={`${index}-${line}`}>{line}</div>
          ))}
        </div>
        {extraContent}
        {authRequired ? (
          <div className="confirm-auth">
            <label className="confirm-auth-label" htmlFor="confirm-auth-token">
              Auth Token (required)
            </label>
            <input
              id="confirm-auth-token"
              className="input confirm-auth-input mono"
              type="password"
              value={authToken}
              placeholder="Bearer token"
              onChange={(event) => onAuthTokenChange(event.target.value)}
            />
          </div>
        ) : null}
        <div className="confirm-actions">
          <button className="btn btn-ghost" type="button" onClick={onClose} disabled={isSubmitting}>
            Cancel
          </button>
          <button className="btn btn-primary" type="button" onClick={onConfirm} disabled={confirmDisabled}>
            {isSubmitting ? "Submitting..." : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
