import { AlertTriangle, ArrowRight, Copy, Square, Play } from "lucide-react";
import type { Capsule, Process } from "../types";

interface ActionPanelProps {
  capsule: Capsule;
  process: Process | undefined;
  platform: string;
  envValues: Record<string, string>;
  onRun: (capsule: Capsule) => void;
  onStop: (capsule: Capsule) => void;
  onOpenLogs: (capsuleId: string, pid: number) => void;
  onCopyPath: (path: string) => void;
  onEnvChange: (capsuleId: string, key: string, value: string) => void;
}

function formatStarted(iso: string): string {
  return new Date(iso).toLocaleTimeString();
}

function formatUptime(iso: string): string {
  const started = new Date(iso).getTime();
  const seconds = Math.max(0, Math.floor((Date.now() - started) / 1000));
  const minutes = Math.floor(seconds / 60);
  const rem = seconds % 60;
  return `${minutes}m ${rem}s`;
}

export function ActionPanel({
  capsule,
  process,
  platform,
  envValues,
  onRun,
  onStop,
  onOpenLogs,
  onCopyPath,
  onEnvChange,
}: ActionPanelProps): JSX.Element {
  const isRunning = Boolean(process?.active);
  const mismatch = !capsule.osArch.includes(platform);

  return (
    <aside className="action-panel">
      <section className="panel-section">
        {capsule.appUrl ? (
          isRunning ? (
            <button className="btn btn-danger panel-full" type="button" onClick={() => onStop(capsule)}>
              <Square size={14} strokeWidth={1.5} /> Stop
            </button>
          ) : (
            <button className="btn btn-success panel-full" type="button" onClick={() => onRun(capsule)}>
              <Play size={14} strokeWidth={1.5} /> Run
            </button>
          )
        ) : (
          <div className="row-meta">CLI only - no app URL</div>
        )}
      </section>

      <section className="panel-section">
        {process ? (
          <button
            className="btn btn-ghost panel-full"
            type="button"
            onClick={() => onOpenLogs(capsule.id, process.pid)}
          >
            {process.active ? "View Logs" : "Last Log"}
            <ArrowRight size={14} strokeWidth={1.5} />
          </button>
        ) : (
          <button className="btn btn-ghost panel-full" type="button" disabled>
            Last Log <ArrowRight size={14} strokeWidth={1.5} />
          </button>
        )}
      </section>

      {process?.active ? (
        <section className="panel-section">
          <div className="kv">
            <span>PID:</span>
            <span>{process.pid}</span>
            <span>Started:</span>
            <span>{formatStarted(process.startedAt)}</span>
            <span>Uptime:</span>
            <span>{formatUptime(process.startedAt)}</span>
          </div>
        </section>
      ) : null}

      {mismatch ? (
        <section className="panel-section">
          <div className="warn-banner">
            <div style={{ display: "flex", alignItems: "center", gap: "6px", marginBottom: "6px" }}>
              <AlertTriangle size={14} strokeWidth={1.5} />
              <strong>Architecture Mismatch</strong>
            </div>
            <div>This capsule targets {capsule.osArch.join(", ")}.</div>
            <div>Current platform: {platform}</div>
          </div>
        </section>
      ) : null}

      <section className="panel-section">
        <div className="section-title" style={{ marginBottom: "8px" }}>
          Environment Variables
        </div>
        <div className="env-grid">
          {Object.entries(capsule.envHints).map(([key, fallback]) => (
            <label key={key} className="env-row">
              <span className="env-key">{key}</span>
              <input
                className="input env-input"
                value={envValues[key] ?? fallback}
                onChange={(event) => onEnvChange(capsule.id, key, event.target.value)}
              />
            </label>
          ))}
        </div>
      </section>

      <section className="panel-section">
        <div className="section-title" style={{ marginBottom: "8px" }}>
          Specs
        </div>
        <div className="spec-grid">
          <div className="spec-row">
            <span className="spec-key">Version</span>
            <span className="spec-value mono">{capsule.version}</span>
          </div>
          <div className="spec-row">
            <span className="spec-key">Size</span>
            <span className="spec-value mono">{capsule.size}</span>
          </div>
          <div className="spec-row">
            <span className="spec-key">OS / Arch</span>
            <span className="spec-value" style={{ display: "flex", gap: "6px", flexWrap: "wrap" }}>
              {capsule.osArch.map((entry) => (
                <span
                  key={entry}
                  className={`badge ${entry === platform ? "badge-accent" : "badge-muted"}`}
                >
                  {entry}
                </span>
              ))}
            </span>
          </div>
          <div className="spec-row">
            <span className="spec-key">Publisher</span>
            <span className="spec-value mono">{capsule.publisher}</span>
          </div>
          <div className="spec-row">
            <span className="spec-key">Trust</span>
            <span className="spec-value">
              <span
                className={`badge ${
                  capsule.trustLevel === "verified" || capsule.trustLevel === "signed"
                    ? "badge-verified"
                    : "badge-unverified"
                }`}
              >
                {capsule.trustLevel === "verified" || capsule.trustLevel === "signed"
                  ? "Verified"
                  : "Unverified"}
              </span>
            </span>
          </div>
          <div className="spec-row">
            <span className="spec-key">Compatibility</span>
            <span className={`spec-value ${mismatch ? "mismatch" : ""}`}>
              {mismatch ? `Mismatch (${platform})` : "Compatible"}
            </span>
          </div>
        </div>
      </section>

      <section className="panel-section">
        <div className="section-title" style={{ marginBottom: "8px" }}>
          Local Path
        </div>
        <div className="path-row">
          <span className="path-text">{capsule.localPath}</span>
          <button
            className="icon-btn"
            type="button"
            aria-label="Copy local path"
            onClick={() => onCopyPath(capsule.localPath)}
          >
            <Copy size={14} strokeWidth={1.5} />
          </button>
        </div>
      </section>
    </aside>
  );
}
