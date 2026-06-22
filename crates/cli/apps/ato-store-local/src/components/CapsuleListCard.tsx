import { Box, ExternalLink, Globe, Package, Play, Search, Square, Trash2, Zap } from "lucide-react";

import { PlatformBadge } from "./PlatformBadge";
import { getProcessStatusMeta } from "../types";
import type { Capsule, Process } from "../types";

interface CapsuleListCardProps {
  capsule: Capsule;
  process: Process | undefined;
  openReady: boolean;
  platform: string;
  onRun: (capsule: Capsule) => void;
  onStop: (capsule: Capsule) => void;
  onOpen: (capsule: Capsule, process?: Process) => void;
  onInspect: (capsule: Capsule) => void;
  onDelete: (capsule: Capsule) => void;
}

function IconForCapsule({ iconKey }: { iconKey: Capsule["iconKey"] }): JSX.Element {
  const common = { size: 16, strokeWidth: 1.5 };
  if (iconKey === "globe") {
    return <Globe {...common} />;
  }
  if (iconKey === "zap") {
    return <Zap {...common} />;
  }
  if (iconKey === "box") {
    return <Box {...common} />;
  }
  return <Package {...common} />;
}

export function CapsuleListCard({
  capsule,
  process,
  openReady,
  platform,
  onRun,
  onStop,
  onOpen,
  onInspect,
  onDelete,
}: CapsuleListCardProps): JSX.Element {
  const status = getProcessStatusMeta(process?.status ?? "stopped");
  const isRunning = Boolean(process?.active);
  return (
    <article
      className="capsule-list-card"
      role="button"
      tabIndex={0}
      onClick={() => onInspect(capsule)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          onInspect(capsule);
        }
      }}
    >
      <div className="capsule-list-header">
        <span
          className={`table-dot status-${status.tone} ${status.active ? "active" : ""}`}
          aria-label={status.label}
        />
        <div className="capsule-cell-icon">
          {capsule.storeMetadata?.iconUrl ? (
            <img
              src={capsule.storeMetadata.iconUrl}
              alt={`${capsule.scopedId} icon`}
              className="capsule-cell-icon-img"
            />
          ) : (
            <IconForCapsule iconKey={capsule.iconKey} />
          )}
        </div>
        <div className="capsule-list-title-wrap">
          <div className="row-id">{capsule.scopedId}</div>
          <div className="row-desc capsule-list-desc">{capsule.description}</div>
          <div className="row-status-wrap">
            <span className={`badge status-badge status-${status.tone}`}>{status.label}</span>
            {process?.targetLabel ? <span className="row-meta">target={process.targetLabel}</span> : null}
          </div>
        </div>
      </div>

      <div className="capsule-list-meta">
        <span className="row-meta">Version: {capsule.version}</span>
        <span className="row-meta">Size: {capsule.size}</span>
      </div>

      <div className="compat-list capsule-list-compat">
        {capsule.osArch.map((entry) => (
          <PlatformBadge key={entry} osArch={entry} active={entry === platform} />
        ))}
      </div>

      <div className="actions-list capsule-list-actions" onClick={(event) => event.stopPropagation()}>
        {isRunning ? (
          <button className="btn btn-danger" type="button" onClick={() => onStop(capsule)}>
            <Square size={14} strokeWidth={1.5} /> Stop
          </button>
        ) : (
          <button className="btn btn-success" type="button" onClick={() => onRun(capsule)}>
            <Play size={14} strokeWidth={1.5} /> Run
          </button>
        )}
        {isRunning ? (
          <button
            className="btn btn-ghost"
            type="button"
            onClick={() => onOpen(capsule, process)}
            disabled={!openReady}
          >
            <ExternalLink size={14} strokeWidth={1.5} /> Open
          </button>
        ) : null}
        <button className="btn btn-ghost" type="button" onClick={() => onInspect(capsule)}>
          <Search size={14} strokeWidth={1.5} /> Inspect
        </button>
        <button className="btn btn-danger" type="button" onClick={() => onDelete(capsule)}>
          <Trash2 size={14} strokeWidth={1.5} /> Delete
        </button>
      </div>
    </article>
  );
}
