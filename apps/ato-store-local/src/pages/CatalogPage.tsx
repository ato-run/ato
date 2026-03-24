import {
  Eye,
  Play,
  Square,
  Trash2,
} from "lucide-react";
import { DockCatalogView, type DockAction } from "../dock/react";
import {
  normalizeLocalCatalogItem,
  type DockCatalogItem,
} from "../dock/domain";

import type { Capsule, CatalogViewMode, OsFilter, Process } from "../types";

function latestProcessForCapsule(processes: Process[], capsuleId: string): Process | undefined {
  return [...processes]
    .filter((process) => process.capsuleId === capsuleId)
    .sort((left, right) => right.startedAt.localeCompare(left.startedAt))[0];
}

function toCatalogItem(capsule: Capsule): DockCatalogItem {
  const [publisher, slug] = capsule.scopedId.split("/", 2);
  return normalizeLocalCatalogItem({
    id: capsule.id,
    slug: slug || capsule.id,
    scopedId: capsule.scopedId,
    name: capsule.name,
    description: capsule.longDescription || capsule.description,
    publisher: {
      handle: publisher || capsule.publisher,
      verified: capsule.trustLevel === "verified" || capsule.trustLevel === "signed",
    },
    type: capsule.type === "service" ? "service" : capsule.type,
    latestVersion: capsule.version,
    size: capsule.size,
    storeMetadata: {
      iconUrl: capsule.storeMetadata?.iconUrl,
      text: capsule.longDescription || capsule.storeMetadata?.text,
    },
  }, {
    href: `/capsule/${encodeURIComponent(capsule.id)}`,
    trustBadge: capsule.trustLevel,
    visibility: "local",
  });
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
  writeAuthRequired: boolean;
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
  onInspect: _onInspect,
  onDelete,
  publishCommand,
  writeAuthRequired,
  onCopyCommand,
}: CatalogPageProps): JSX.Element {
  const items = capsules.map(toCatalogItem);
  const actionsById = new Map(
    capsules.map((capsule) => {
      const process = latestProcessForCapsule(processes, capsule.id);
      const actions: DockAction[] = [
        process?.active
          ? {
              id: `${capsule.id}-stop`,
              label: "Stop",
              tone: "danger",
              icon: <Square size={13} strokeWidth={1.8} />,
              onAction: () => onStop(capsule),
            }
          : {
              id: `${capsule.id}-run`,
              label: "Run",
              tone: "primary",
              icon: <Play size={13} strokeWidth={1.8} />,
              onAction: () => onRun(capsule),
            },
        {
          id: `${capsule.id}-detail`,
          label: "Detail",
          tone: "ghost",
          href: `/capsule/${encodeURIComponent(capsule.id)}`,
          icon: <Eye size={13} strokeWidth={1.8} />,
        },
        {
          id: `${capsule.id}-open`,
          label: "Open",
          tone: "secondary",
          icon: <Eye size={13} strokeWidth={1.8} />,
          disabled: !isOpenReady(process),
          onAction: () => onOpen(capsule, process),
        },
        {
          id: `${capsule.id}-delete`,
          label: "Delete",
          tone: "ghost",
          icon: <Trash2 size={13} strokeWidth={1.8} />,
          onAction: () => onDelete(capsule),
        },
      ];
      return [capsule.id, actions] as const;
    }),
  );

  if (capsules.length === 0) {
    return (
      <div className="empty-state" role="status">
        <h3>No capsules in this Dock.</h3>
        <p>To publish your first capsule, run:</p>
        {writeAuthRequired ? (
          <p>If this Dock requires write auth, run <code>ato login</code> first. The publish command below will reuse your saved CLI session.</p>
        ) : null}
        <pre>{publishCommand}</pre>
        <button className="btn btn-ghost" type="button" onClick={onCopyCommand}>
          Copy command
        </button>
      </div>
    );
  }

  const subtitle = isMobile
    ? `Showing ${items.length} capsules on ${platform}.`
    : `Shared store-web visual layer on top of local runtime actions (${platform}).`;

  return (
    <DockCatalogView
      items={items}
      viewMode={viewMode}
      onViewModeChange={onViewModeChange}
      filterLabel="Platform"
      filterValue={filter}
      filterOptions={[
        { value: "all", label: "All" },
        { value: "macos", label: "macOS" },
        { value: "linux", label: "Linux" },
        { value: "windows", label: "Windows" },
      ]}
      onFilterChange={(value) => onFilterChange(value as OsFilter)}
      countLabel={`${items.length} local capsules`}
      subtitle={subtitle}
      getActions={(item) => actionsById.get(item.id) ?? []}
    />
  );
}
