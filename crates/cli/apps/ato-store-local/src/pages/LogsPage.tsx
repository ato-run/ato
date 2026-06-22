import { ArrowLeft } from "lucide-react";
import type { ProcessLogLine } from "../types";

interface LogsPageProps {
  scopedId: string;
  pid: number;
  startedAt: string;
  logs: ProcessLogLine[];
  onBack: () => void;
  onClear: () => void;
}

function classForLevel(level: string): string {
  const key = level.toLowerCase();
  if (key === "info") {
    return "log-level info";
  }
  if (key === "warn") {
    return "log-level warn";
  }
  if (key === "error") {
    return "log-level error";
  }
  if (key === "sigterm") {
    return "log-level sigterm";
  }
  return "log-level";
}

export function LogsPage({
  scopedId,
  pid,
  startedAt,
  logs,
  onBack,
  onClear,
}: LogsPageProps): JSX.Element {
  return (
    <div className="logs-card">
      <div className="logs-head">
        <button className="btn btn-ghost" type="button" onClick={onBack}>
          <ArrowLeft size={14} strokeWidth={1.5} /> Back
        </button>
        <div className="logs-title">
          {scopedId} · PID {pid} · Started {new Date(startedAt).toLocaleTimeString()}
        </div>
      </div>

      <div className="logs-body" aria-live="polite">
        {logs.length === 0 ? (
          <div className="log-empty">- no output yet -</div>
        ) : (
          logs.map((line) => (
            <div key={`${line.index}-${line.timestamp}`} className="log-row">
              <span className="log-ln">{line.index}</span>
              <span className="log-time">[{line.timestamp}]</span>
              <span className={classForLevel(line.level)}>{line.level}</span>
              <span className="log-msg">{line.message}</span>
            </div>
          ))
        )}
      </div>

      <div className="logs-footer">
        <div className="logs-spacer" />
        <button className="btn btn-ghost" type="button" onClick={onClear}>
          Clear
        </button>
      </div>
    </div>
  );
}
