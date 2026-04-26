import { Activity, Menu } from "lucide-react";
import type { Process } from "../types";

interface AppHeaderProps {
  processes: Process[];
  isMobile: boolean;
  onOpenMobileSidebar: () => void;
  onOpenProcesses: () => void;
}

export function AppHeader({
  processes,
  isMobile,
  onOpenMobileSidebar,
  onOpenProcesses,
}: AppHeaderProps): JSX.Element {
  const activeProcesses = processes.filter((p) => p.active);

  return (
    <header className="app-header">
      <div className="app-header-inner">
        <div className="app-header-left">
          {isMobile ? (
            <button
              className="icon-btn app-header-menu-btn"
              type="button"
              aria-label="Open navigation menu"
              onClick={onOpenMobileSidebar}
            >
              <Menu size={15} strokeWidth={1.5} />
            </button>
          ) : null}
          <div className="app-header-brand">
            <span className="app-header-logo">ato</span>
            <span className="app-header-sep">/</span>
            <span className="app-header-subtitle">local dock</span>
          </div>
        </div>

        <div className="app-header-right">
          <button
            className="app-header-processes-btn"
            type="button"
            onClick={onOpenProcesses}
          >
            <Activity size={13} strokeWidth={1.5} />
            <span>Processes</span>
            {activeProcesses.length > 0 ? (
              <span className="app-header-processes-count">
                {activeProcesses.length}
              </span>
            ) : null}
          </button>
        </div>
      </div>
    </header>
  );
}
