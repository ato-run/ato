import { Trash, X } from "lucide-react";
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
  return (
    <>
      <div className={`process-drawer-overlay ${open ? "open" : ""}`} onClick={onClose} />
      <aside className={`process-drawer ${open ? "open" : ""}`}>
        <div className="process-drawer-header">
          <strong className="process-drawer-title">Processes</strong>
          <div className="process-drawer-spacer" />
          <button className="icon-btn" type="button" onClick={onClose} aria-label="Close process drawer">
            <X size={14} strokeWidth={1.5} />
          </button>
        </div>

        <div className="process-drawer-body">
          {processes.length === 0 ? (
            <div className="row-meta">No process records</div>
          ) : (
            processes.map((process) => {
              const status = getProcessStatusMeta(process.status);
              return (
                <div key={process.id} className="card process-drawer-card">
                  <div className="mono process-drawer-scoped-id">{process.scopedId}</div>
                  <div className="process-drawer-meta-row">
                    <span className={`badge status-badge status-${status.tone}`}>{status.label}</span>
                    <span className="row-meta process-drawer-meta">PID {process.pid}</span>
                  </div>
                  <div className="process-drawer-actions">
                    <button className="btn btn-ghost" type="button" onClick={() => onOpenLogs(process.id)}>
                      Logs
                    </button>
                    <button
                      className="icon-btn"
                      type="button"
                      aria-label="Stop process"
                      onClick={() => onStop(process)}
                      disabled={!process.active}
                    >
                      <Trash size={14} strokeWidth={1.5} />
                    </button>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </aside>
    </>
  );
}
