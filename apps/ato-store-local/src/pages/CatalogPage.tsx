import { Copy } from "lucide-react";
import type { Capsule, CatalogViewMode, OsFilter, Process } from "../types";
import { CapsuleIconCard } from "../components/CapsuleIconCard";
import { CapsuleListCard } from "../components/CapsuleListCard";
import { CapsuleRow } from "../components/CapsuleRow";
import { Toolbar } from "../components/Toolbar";

function latestProcessForCapsule(processes: Process[], capsuleId: string): Process | undefined {
  return [...processes]
    .filter((process) => process.capsuleId === capsuleId)
    .sort((left, right) => right.startedAt.localeCompare(left.startedAt))[0];
}

interface CatalogPageProps {
  capsules: Capsule[];
  processes: Process[];
  platform: string;
  isMobile: boolean;
  viewMode: CatalogViewMode;
  filter: OsFilter;
  onFilterChange: (filter: OsFilter) => void;
  onViewModeChange: (mode: CatalogViewMode) => void;
  onRun: (capsule: Capsule) => void;
  onStop: (capsule: Capsule) => void;
  onOpen: (capsule: Capsule, process?: Process) => void;
  isOpenReady: (process?: Process) => boolean;
  onInspect: (capsule: Capsule) => void;
  onDelete: (capsule: Capsule) => void;
  publishCommand: string;
  onCopyCommand: () => void;
}

export function CatalogPage({
  capsules,
  processes,
  platform,
  isMobile,
  viewMode,
  filter,
  onFilterChange,
  onViewModeChange,
  onRun,
  onStop,
  onOpen,
  isOpenReady,
  onInspect,
  onDelete,
  publishCommand,
  onCopyCommand,
}: CatalogPageProps): JSX.Element {
  return (
    <div>
      <Toolbar
        filter={filter}
        onFilterChange={onFilterChange}
        total={capsules.length}
        viewMode={viewMode}
        onViewModeChange={onViewModeChange}
      />

      {capsules.length === 0 ? (
        <div className="empty-state" role="status">
          <h3>No capsules in this registry.</h3>
          <p>To publish your first capsule, run:</p>
          <pre>{publishCommand}</pre>
          <button className="btn btn-ghost" type="button" onClick={onCopyCommand}>
            <Copy size={14} strokeWidth={1.5} /> Copy command
          </button>
        </div>
      ) : viewMode === "list" && isMobile ? (
        <div className="mobile-list">
          {capsules.map((capsule) => {
            const process = latestProcessForCapsule(processes, capsule.id);
            return (
              <CapsuleListCard
                key={capsule.id}
                capsule={capsule}
                process={process}
                openReady={isOpenReady(process)}
                platform={platform}
                onRun={onRun}
                onStop={onStop}
                onOpen={onOpen}
                onInspect={onInspect}
                onDelete={onDelete}
              />
            );
          })}
        </div>
      ) : viewMode === "list" ? (
        <div className="table-card">
          <table className="catalog-table">
            <thead>
              <tr>
                <th style={{ width: "36px" }} />
                <th>Capsule</th>
                <th>Version</th>
                <th>Platforms</th>
                <th>Size</th>
                <th style={{ textAlign: "right" }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {capsules.map((capsule) => {
                const process = latestProcessForCapsule(processes, capsule.id);
                return (
                  <CapsuleRow
                    key={capsule.id}
                    capsule={capsule}
                    process={process}
                    openReady={isOpenReady(process)}
                    platform={platform}
                    onRun={onRun}
                    onStop={onStop}
                    onOpen={onOpen}
                    onInspect={onInspect}
                    onDelete={onDelete}
                  />
                );
              })}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="grid-view">
          {capsules.map((capsule) => (
            <CapsuleIconCard key={capsule.id} capsule={capsule} onClick={onInspect} />
          ))}
        </div>
      )}
    </div>
  );
}
