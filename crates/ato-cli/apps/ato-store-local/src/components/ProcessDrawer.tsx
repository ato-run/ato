import { useEffect, useRef } from "react";
import { Square, X, FileText } from "lucide-react";
import { getProcessStatusMeta } from "../types";
import type { Process } from "../types";

interface ProcessDrawerProps {
  open: boolean;
  processes: Process[];
  onClose: () => void;
  onStop: (process: Process) => void;
  onOpenLogs: (processId: string) => void;
}

export function ProcessDrawer({
  open,
  processes,
  onClose,
  onStop,
  onOpenLogs,
}: ProcessDrawerProps): JSX.Element {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      if (!dialog.open) dialog.showModal();
    } else {
      if (dialog.open) dialog.close();
    }
  }, [open]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dialog.addEventListener("cancel", handleCancel);
    return () => dialog.removeEventListener("cancel", handleCancel);
  }, [onClose]);

  return (
    <dialog
      ref={dialogRef}
      className="process-modal"
      onClick={(e) => {
        if (e.target === dialogRef.current) onClose();
      }}
    >
      <div className="process-modal-panel">
        {/* Header */}
        <div className="process-modal-header">
          <strong className="process-modal-title">Processes</strong>
          <button
            className="icon-btn"
            type="button"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={16} strokeWidth={1.5} />
          </button>
        </div>

        {/* Body */}
        <div className="process-modal-body">
          {processes.length === 0 ? (
            <div className="process-modal-empty">No process records</div>
          ) : (
            processes.map((process) => {
              const status = getProcessStatusMeta(process.status);
              return (
                <div key={process.id} className="process-modal-card">
                  <div className="process-modal-scoped-id mono">{process.scopedId}</div>
                  <div className="process-modal-meta-row">
                    <span className={`badge status-badge status-${status.tone}`}>
                      {status.label}
                    </span>
                    <span className="row-meta">PID {process.pid}</span>
                  </div>
                  <div className="process-modal-actions">
                    <button
                      className="btn btn-ghost"
                      type="button"
                      onClick={() => { onOpenLogs(process.id); onClose(); }}
                    >
                      <FileText size={13} strokeWidth={1.5} />
                      Logs
                    </button>
                    <button
                      className="btn btn-ghost"
                      type="button"
                      aria-label="Stop process"
                      onClick={() => onStop(process)}
                      disabled={!process.active}
                    >
                      <Square size={13} strokeWidth={1.5} />
                      Stop
                    </button>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>
    </dialog>
  );
}
