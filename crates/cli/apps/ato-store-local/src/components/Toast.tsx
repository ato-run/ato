import { useEffect, useState } from "react";
import { Copy, X } from "lucide-react";

export interface ToastState {
  kind: "success" | "error";
  message: string;
  copyText?: string;
  sticky?: boolean;
}

interface ToastProps {
  toast: ToastState;
  onClose: () => void;
}

export function Toast({ toast, onClose }: ToastProps): JSX.Element {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setCopied(false);
  }, [toast.message, toast.copyText, toast.kind]);

  const handleCopy = async (): Promise<void> => {
    if (!toast.copyText) {
      return;
    }
    try {
      await navigator.clipboard.writeText(toast.copyText);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div
      className={`toast-root ${toast.kind === "error" ? "error" : "success"}`}
      role={toast.kind === "error" ? "alert" : "status"}
      aria-live={toast.kind === "error" ? "assertive" : "polite"}
    >
      <div className="toast-message">{toast.message}</div>
      <div className="toast-actions">
        {toast.copyText ? (
          <button className="btn btn-ghost toast-btn" type="button" onClick={() => void handleCopy()}>
            <Copy size={13} strokeWidth={1.5} />
            {copied ? "Copied" : "Copy"}
          </button>
        ) : null}
        <button className="icon-btn toast-close-btn" type="button" aria-label="Close toast" onClick={onClose}>
          <X size={14} strokeWidth={1.5} />
        </button>
      </div>
    </div>
  );
}
