import { useState } from "react";
import { Activity, ExternalLink, Menu, X } from "lucide-react";
import type { Process } from "../types";

interface LocalNavProps {
  processes: Process[];
  isMobile: boolean;
  onOpenMobileSidebar: () => void;
  onOpenProcesses: () => void;
  publisherHandle?: string;
}

export function LocalNav({
  processes,
  onOpenProcesses,
  publisherHandle,
}: LocalNavProps): JSX.Element {
  const [isMenuOpen, setIsMenuOpen] = useState(false);
  const activeProcesses = processes.filter((p) => p.active);

  return (
    <header
      className="fixed top-0 left-0 right-0 z-50 border-b border-[var(--border)]"
      style={{
        backdropFilter: "blur(12px)",
        WebkitBackdropFilter: "blur(12px)",
        background: "rgba(250, 250, 250, 0.86)",
      }}
    >
      <div className="max-w-6xl mx-auto px-6 h-16 flex items-center justify-between">
        {/* Left: brand + nav */}
        <div className="flex items-center gap-8 min-w-0">
          <div className="flex items-center gap-2 font-semibold text-lg text-[var(--fg)]">
            <img
              src="/favicon.svg"
              alt=""
              aria-hidden="true"
              className="w-7 h-7 object-contain"
              onError={(e) => { (e.currentTarget as HTMLImageElement).style.display = "none"; }}
            />
            <span
              style={{
                fontFamily: "'JetBrains Mono', monospace",
                fontSize: "15px",
                fontWeight: 700,
                letterSpacing: "-0.03em",
              }}
            >
              ato
            </span>
            <span className="text-[var(--fg-tertiary,#a3a3a3)] text-sm font-normal">/</span>
            <span className="text-[var(--fg-secondary,#525252)] text-sm font-normal">
              local dock
            </span>
          </div>

          <nav className="hidden md:flex items-center gap-6" aria-label="main">
            <span className="text-sm font-semibold text-[var(--fg,#0a0a0a)]">
              Library
            </span>
          </nav>
        </div>

        {/* Right: Processes button */}
        <div className="flex items-center gap-3">
          <a
            href="https://ato.run/dock"
            className="hidden md:inline-flex items-center gap-1.5 px-3 py-1.5 rounded-xl border border-[var(--border,#e5e5e5)] bg-[var(--bg-card,#fff)] hover:bg-[var(--bg-elevated,#f5f5f5)] transition-colors text-sm text-[var(--fg,#0a0a0a)]"
            target="_blank"
            rel="noreferrer"
          >
            <ExternalLink size={13} strokeWidth={1.5} />
            View Remote
          </a>
          <button
            type="button"
            className="inline-flex items-center gap-2 px-3 py-1.5 rounded-xl border border-[var(--border,#e5e5e5)] bg-[var(--bg-card,#fff)] hover:bg-[var(--bg-elevated,#f5f5f5)] transition-colors text-sm text-[var(--fg,#0a0a0a)]"
            onClick={onOpenProcesses}
          >
            <Activity size={14} strokeWidth={1.5} />
            <span>Processes</span>
            {activeProcesses.length > 0 && (
              <span className="bg-[var(--fg,#0a0a0a)] text-white text-[10px] font-semibold rounded-full w-[18px] h-[18px] inline-flex items-center justify-center">
                {activeProcesses.length}
              </span>
            )}
          </button>

          {/* Mobile hamburger */}
          <button
            type="button"
            className="md:hidden p-2 text-[var(--fg-secondary,#525252)] hover:text-[var(--fg,#0a0a0a)]"
            onClick={() => setIsMenuOpen((o) => !o)}
            aria-label="Toggle menu"
          >
            {isMenuOpen ? <X size={22} /> : <Menu size={22} />}
          </button>
        </div>
      </div>

      {/* Mobile dropdown */}
      {isMenuOpen && (
        <div className="md:hidden absolute top-full left-0 w-full bg-[var(--bg-card,#fff)] border-b border-[var(--border,#e5e5e5)] shadow-lg">
          <div className="p-4 flex flex-col gap-2">
            <span className="px-4 py-3 rounded-lg text-sm font-semibold text-[var(--fg,#0a0a0a)]">
              Library
            </span>
            <button
              type="button"
              className="px-4 py-3 rounded-lg text-sm font-medium text-left hover:bg-[var(--bg-elevated,#f5f5f5)] text-[var(--fg,#0a0a0a)] inline-flex items-center gap-2"
              onClick={() => { setIsMenuOpen(false); onOpenProcesses(); }}
            >
              <Activity size={14} strokeWidth={1.5} />
              Processes
              {activeProcesses.length > 0 && (
                <span className="bg-[var(--fg,#0a0a0a)] text-white text-[10px] font-semibold rounded-full w-[18px] h-[18px] inline-flex items-center justify-center">
                  {activeProcesses.length}
                </span>
              )}
            </button>
            <a
              href="https://ato.run/dock"
              className="px-4 py-3 rounded-lg text-sm font-medium hover:bg-[var(--bg-elevated,#f5f5f5)] text-[var(--fg,#0a0a0a)] inline-flex items-center gap-2"
              target="_blank"
              rel="noreferrer"
              onClick={() => setIsMenuOpen(false)}
            >
              <ExternalLink size={14} strokeWidth={1.5} />
              View Remote
            </a>
          </div>
        </div>
      )}
    </header>
  );
}
