import { Activity, X } from "lucide-react";

import type { Process } from "../types";

interface SidebarProps {
  processes: Process[];
  isMobile: boolean;
  mobileOpen: boolean;
  onCloseMobile: () => void;
  onOpenProcesses: () => void;
}

export function Sidebar({
  processes,
  isMobile,
  mobileOpen,
  onCloseMobile,
  onOpenProcesses,
}: SidebarProps): JSX.Element {
  const activeProcesses = processes.filter((process) => process.active);

  return (
    <>
      {isMobile ? (
        <div
          className={`sidebar-overlay ${mobileOpen ? "open" : ""}`}
          aria-hidden={!mobileOpen}
          onClick={onCloseMobile}
        />
      ) : null}
      <aside className={`sidebar${isMobile ? " mobile" : ""}${isMobile && mobileOpen ? " open" : ""}`}>
        <div className="sidebar-header">
          <div className="sidebar-header-row">
            <div className="sidebar-logo">ato</div>
            {isMobile ? (
              <button className="icon-btn sidebar-close-btn" type="button" aria-label="Close navigation menu" onClick={onCloseMobile}>
                <X size={14} strokeWidth={1.5} />
              </button>
            ) : null}
          </div>
          <div className="sidebar-subtitle">local dock</div>
        </div>

        <div className="sidebar-group">
          <div className="sidebar-label">Navigate</div>
          <button className="sidebar-item active" type="button">
            Library
          </button>
          <button className="sidebar-item disabled" type="button" disabled>
            Store (soon)
          </button>
        </div>

        <div className="sidebar-group">
          <div className="sidebar-label">Running</div>
          {activeProcesses.length === 0 ? (
            <div className="sidebar-subtitle">No active sessions</div>
          ) : (
            activeProcesses.map((process) => (
              <div key={`${process.capsuleId}-${process.pid}`} className="running-item">
                <span className="running-dot" />
                <span>{process.scopedId}</span>
              </div>
            ))
          )}
          <button className="pill btn-primary sidebar-process-button" type="button" onClick={onOpenProcesses}>
            <Activity size={14} strokeWidth={1.5} />
            Processes: {activeProcesses.length} active
          </button>
        </div>
      </aside>
    </>
  );
}
