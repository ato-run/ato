import type { CatalogViewMode, OsFilter } from "../types";

interface ToolbarProps {
  filter: OsFilter;
  onFilterChange: (filter: OsFilter) => void;
  total: number;
  viewMode: CatalogViewMode;
  onViewModeChange: (viewMode: CatalogViewMode) => void;
}

const FILTERS: Array<{ key: OsFilter; label: string }> = [
  { key: "all", label: "All" },
  { key: "macos", label: "macOS" },
  { key: "linux", label: "Linux" },
  { key: "windows", label: "Windows" },
];

export function Toolbar({
  filter,
  onFilterChange,
  total,
  viewMode,
  onViewModeChange,
}: ToolbarProps): JSX.Element {
  return (
    <div className="toolbar">
      <div className="filter-tabs" role="tablist" aria-label="OS filter">
        {FILTERS.map((entry) => (
          <button
            key={entry.key}
            type="button"
            className={`filter-tab ${filter === entry.key ? "active" : ""}`}
            onClick={() => onFilterChange(entry.key)}
          >
            {entry.label}
          </button>
        ))}
      </div>
      <div className="toolbar-spacer" />
      <span className="mono row-meta">{total} capsules</span>
      <div className="filter-tabs" role="tablist" aria-label="Catalog view mode">
        <button
          type="button"
          className={`filter-tab ${viewMode === "list" ? "active" : ""}`}
          onClick={() => onViewModeChange("list")}
        >
          List
        </button>
        <button
          type="button"
          className={`filter-tab ${viewMode === "grid" ? "active" : ""}`}
          onClick={() => onViewModeChange("grid")}
        >
          Grid
        </button>
      </div>
    </div>
  );
}
