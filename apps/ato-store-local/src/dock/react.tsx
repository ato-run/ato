import "./styles.css";

import { useState, type ReactNode } from "react";
import {
  CheckCircle2,
  ExternalLink,
  Grid3X3,
  LayoutList,
  Package,
  Play,
  Search,
  ShieldCheck,
  Terminal,
  Zap,
} from "lucide-react";

import type {
  DockCapsuleDetail,
  DockCapsuleType,
  DockCatalogItem,
  DockRelease,
} from "./domain";

export type DockAction = {
  id: string;
  label: string;
  onAction?: () => void;
  href?: string;
  target?: string;
  disabled?: boolean;
  tone?: "primary" | "secondary" | "ghost" | "danger";
  icon?: ReactNode;
};

type DockCapsuleCardProps = {
  item: DockCatalogItem;
  actions?: DockAction[];
};

function DockCapsuleCard({ item, actions = [] }: DockCapsuleCardProps): JSX.Element {
  const [coverFailed, setCoverFailed] = useState(false);
  const [iconFailed, setIconFailed] = useState(false);
  const coverImage = item.coverImage?.trim();
  const iconImage = item.iconImage?.trim();
  const hasCoverImage = Boolean(coverImage) && !coverFailed;
  const hasIconImage = Boolean(iconImage) && !iconFailed;
  const hasCardLink = Boolean(item.href);

  const wrapPreview = (content: ReactNode) =>
    hasCardLink && actions.length > 0 ? (
      <a className="dockr-card-link" href={item.href}>
        {content}
      </a>
    ) : (
      content
    );

  const renderTitle = () => {
    if (hasCardLink && actions.length > 0) {
      return (
        <a className="dockr-card-link" href={item.href}>
          <h3 className="dockr-card-title">{item.title}</h3>
        </a>
      );
    }
    return <h3 className="dockr-card-title">{item.title}</h3>;
  };

  const body = (
    <article className="dockr-card">
      {wrapPreview(
        <div className="dockr-card-preview">
          {hasCoverImage ? (
            <img
              src={coverImage}
              alt={`${item.title} preview`}
              loading="lazy"
              onError={() => setCoverFailed(true)}
            />
          ) : (
            <div className="dockr-card-preview dockr-card-preview--placeholder" />
          )}
        </div>,
      )}
      <div className="dockr-card-body">
        <div className="dockr-card-header">
          <div className="dockr-card-icon">
            {hasIconImage ? (
              <img
                src={iconImage}
                alt={`${item.title} icon`}
                loading="lazy"
                onError={() => setIconFailed(true)}
              />
            ) : (
              <TypeIcon type={item.type} />
            )}
          </div>
          <div className="dockr-card-title-wrap">
            <div className="dockr-card-title-row">
              {renderTitle()}
              <span className={`dockr-pill dockr-pill--type-${item.type}`}>
                {formatTypeLabel(item.type)}
              </span>
            </div>
            <p className="dockr-card-subtitle">
              {item.publisher.displayName || item.publisher.handle}
              {" · "}
              {item.scopedId}
            </p>
          </div>
        </div>

        <p className="dockr-card-description">{item.description}</p>

        <div className="dockr-card-meta">
          {item.trustBadge ? (
            <span className="dockr-badge">
              <ShieldCheck size={12} strokeWidth={1.7} />
              {item.trustBadge}
            </span>
          ) : null}
          {item.visibility ? <span className="dockr-badge">{item.visibility}</span> : null}
          {item.aclCount && item.aclCount > 0 ? (
            <span className="dockr-badge">{item.aclCount} ACL</span>
          ) : null}
          {item.playgroundAvailable ? (
            <span className="dockr-badge">
              <Play size={12} strokeWidth={1.7} />
              Web
            </span>
          ) : null}
        </div>

        <div className="dockr-card-kpis">
          <div className="dockr-card-kpi">
            <div className="dockr-card-kpi-label">Version</div>
            <div className="dockr-card-kpi-value">v{item.version}</div>
          </div>
          <div className="dockr-card-kpi">
            <div className="dockr-card-kpi-label">
              {item.sizeLabel ? "Size" : "Downloads"}
            </div>
            <div className="dockr-card-kpi-value">
              {item.sizeLabel || formatDownloads(item.downloads)}
            </div>
          </div>
          <div className="dockr-card-kpi">
            <div className="dockr-card-kpi-label">
              {item.vouchesCount > 0 ? "Vouches" : "Updated"}
            </div>
            <div className="dockr-card-kpi-value">
              {item.vouchesCount > 0
                ? String(item.vouchesCount)
                : formatDateLabel(item.updatedAt)}
            </div>
          </div>
        </div>

        {actions.length > 0 ? (
          <div className="dockr-card-actions">
            {actions.map((action) => (
              <DockActionButton key={action.id} action={action} />
            ))}
          </div>
        ) : null}
      </div>
    </article>
  );

  if (!item.href || actions.length > 0) {
    return body;
  }

  return (
    <a className="dockr-card-link" href={item.href}>
      {body}
    </a>
  );
}

export type DockCatalogViewProps<T extends DockCatalogItem = DockCatalogItem> = {
  items: T[];
  viewMode?: "grid" | "list";
  onViewModeChange?: (mode: "grid" | "list") => void;
  query?: string;
  onQueryChange?: (value: string) => void;
  queryPlaceholder?: string;
  filterLabel?: string;
  filterValue?: string;
  filterOptions?: Array<{ value: string; label: string }>;
  onFilterChange?: (value: string) => void;
  emptyTitle?: string;
  emptyDescription?: string;
  loading?: boolean;
  error?: string | null;
  countLabel?: string;
  subtitle?: string;
  getActions?: (item: T) => DockAction[];
  renderItem?: (item: T, viewMode: "grid" | "list") => ReactNode;
};

export function DockCatalogView<T extends DockCatalogItem = DockCatalogItem>({
  items,
  viewMode = "grid",
  onViewModeChange,
  query,
  onQueryChange,
  queryPlaceholder = "Search capsules...",
  filterLabel,
  filterValue,
  filterOptions = [],
  onFilterChange,
  emptyTitle = "No capsules found.",
  emptyDescription = "Adjust your filters or publish a capsule to populate this view.",
  loading = false,
  error = null,
  countLabel,
  subtitle,
  getActions,
  renderItem,
}: DockCatalogViewProps<T>): JSX.Element {
  return (
    <div className="dockr-surface">
      <div className="dockr-toolbar">
        <div className="dockr-heading">
          <strong>{countLabel || `${items.length} capsules`}</strong>
          {subtitle ? <p>{subtitle}</p> : null}
        </div>
        <div className="dockr-controls">
          {filterOptions.length > 0 && onFilterChange ? (
            <label className="dockr-control">
              {filterLabel ? (
                <span className="dockr-control-label">{filterLabel}</span>
              ) : null}
              <select
                className="dockr-select"
                value={filterValue}
                onChange={(event) => onFilterChange(event.target.value)}
              >
                {filterOptions.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          {typeof query === "string" && onQueryChange ? (
            <label className="dockr-search">
              <Search size={14} strokeWidth={1.8} />
              <input
                className="dockr-input"
                value={query}
                onChange={(event) => onQueryChange(event.target.value)}
                placeholder={queryPlaceholder}
                aria-label={queryPlaceholder}
              />
            </label>
          ) : null}
          {onViewModeChange ? (
            <div className="dockr-view-toggle" role="group" aria-label="View mode">
              <button
                type="button"
                className={`dockr-view-button${viewMode === "grid" ? " is-active" : ""}`}
                onClick={() => onViewModeChange("grid")}
                aria-label="Grid view"
              >
                <Grid3X3 size={15} strokeWidth={1.8} />
              </button>
              <button
                type="button"
                className={`dockr-view-button${viewMode === "list" ? " is-active" : ""}`}
                onClick={() => onViewModeChange("list")}
                aria-label="List view"
              >
                <LayoutList size={15} strokeWidth={1.8} />
              </button>
            </div>
          ) : null}
        </div>
      </div>

      {loading ? <p className="dockr-note">Loading capsules...</p> : null}
      {error ? <p className="dockr-note dockr-note--error">{error}</p> : null}

      {!loading && items.length === 0 ? (
        <div className="dockr-empty">
          <p>{emptyTitle}</p>
          <span>{emptyDescription}</span>
        </div>
      ) : (
        <div className="dockr-list" data-view-mode={viewMode}>
          {items.map((item) =>
            renderItem ? (
              renderItem(item, viewMode)
            ) : (
              <DockCapsuleCard
                key={item.id}
                item={item}
                actions={getActions ? getActions(item) : undefined}
              />
            ),
          )}
        </div>
      )}
    </div>
  );
}

type DockCapsuleDetailSummaryProps = {
  detail: DockCapsuleDetail;
  metrics?: Array<{ label: string; value: string }>;
  actions?: DockAction[];
  children?: ReactNode;
};

export function DockCapsuleDetailSummary({
  detail,
  metrics = [],
  actions = [],
  children,
}: DockCapsuleDetailSummaryProps): JSX.Element {
  const resolvedMetrics = metrics.length > 0
    ? metrics
    : [
        { label: "Version", value: `v${detail.version}` },
        {
          label: detail.downloads > 0 ? "Downloads" : "Type",
          value: detail.downloads > 0 ? formatDownloads(detail.downloads) : formatTypeLabel(detail.type),
        },
        {
          label: "Updated",
          value: formatDateLabel(detail.updatedAt || detail.createdAt),
        },
      ];

  return (
    <section className="dockr-summary">
      <div className="dockr-summary-top">
        <div className="dockr-summary-head">
          <div className="dockr-summary-icon">
            {detail.iconImage ? (
              <img src={detail.iconImage} alt={`${detail.title} icon`} />
            ) : (
              <TypeIcon type={detail.type} />
            )}
          </div>
          <div>
            <div className="dockr-card-title-row">
              <h1 className="dockr-summary-title">{detail.title}</h1>
              <span className={`dockr-pill dockr-pill--type-${detail.type}`}>
                {formatTypeLabel(detail.type)}
              </span>
              {detail.trustBadge ? (
                <span className="dockr-badge">
                  <ShieldCheck size={12} strokeWidth={1.7} />
                  {detail.trustBadge}
                </span>
              ) : null}
              {detail.visibility ? (
                <span className="dockr-badge">{detail.visibility}</span>
              ) : null}
            </div>
            <p className="dockr-card-subtitle">
              {detail.publisher.displayName || detail.publisher.handle}
              {" · "}
              {detail.scopedId}
            </p>
            <p className="dockr-summary-description">{detail.description}</p>
          </div>
        </div>
        {actions.length > 0 ? (
          <div className="dockr-card-actions">
            {actions.map((action) => (
              <DockActionButton key={action.id} action={action} />
            ))}
          </div>
        ) : null}
      </div>

      <div className="dockr-summary-metrics">
        {resolvedMetrics.map((metric) => (
          <div key={metric.label} className="dockr-summary-metric">
            <div className="dockr-summary-metric-label">{metric.label}</div>
            <div className="dockr-summary-metric-value">{metric.value}</div>
          </div>
        ))}
      </div>

      {children}
    </section>
  );
}

type DockReadmePanelProps = {
  markdown: string;
  source?: string | null;
  title?: string;
  subtitle?: string;
};

export function DockReadmePanel({
  markdown,
  source,
  title = "README",
  subtitle,
}: DockReadmePanelProps): JSX.Element {
  return (
    <section className="dockr-panel">
      <div className="dockr-panel-header">
        <div>
          <h2 className="dockr-panel-title">{title}</h2>
          {subtitle || source ? (
            <p className="dockr-panel-subtitle">{subtitle || `source: ${source}`}</p>
          ) : null}
        </div>
      </div>
      <div className="dockr-panel-body">
        <div
          className="dockr-readme"
          dangerouslySetInnerHTML={{ __html: sanitizeRenderedHtml(renderMarkdown(markdown)) }}
        />
      </div>
    </section>
  );
}

type DockReleaseTableProps = {
  releases: DockRelease[];
  title?: string;
  subtitle?: string;
  emptyLabel?: string;
  renderActions?: (release: DockRelease) => ReactNode;
};

export function DockReleaseTable({
  releases,
  title = "Releases",
  subtitle,
  emptyLabel = "No release history is available yet.",
  renderActions,
}: DockReleaseTableProps): JSX.Element {
  return (
    <section className="dockr-panel">
      <div className="dockr-panel-header">
        <div>
          <h2 className="dockr-panel-title">{title}</h2>
          {subtitle ? <p className="dockr-panel-subtitle">{subtitle}</p> : null}
        </div>
      </div>
      <div className="dockr-panel-body">
        {releases.length === 0 ? (
          <p className="dockr-note">{emptyLabel}</p>
        ) : (
          <div className="dockr-release-list">
            {releases.map((release, index) => (
              <article
                key={`${release.version}-${release.contentHash}-${index}`}
                className="dockr-release-card"
              >
                <div className="dockr-release-head">
                  <div>
                    <div className="dockr-release-title-row">
                      <h3 className="dockr-release-title">v{release.version}</h3>
                      {release.isCurrent ? <span className="dockr-badge">Current</span> : null}
                      {release.yankedAt ? <span className="dockr-badge">Yanked</span> : null}
                    </div>
                    <p className="dockr-release-meta">
                      {release.createdAt
                        ? `Released ${formatDateLabel(release.createdAt)}`
                        : "Release metadata unavailable"}
                    </p>
                  </div>
                  <div className="dockr-release-stat">
                    <div>{release.sizeBytes ? formatBytesLabel(release.sizeBytes) : "—"}</div>
                    <div>Signature: {release.signatureStatus}</div>
                    <div>Status: {release.status || "unknown"}</div>
                  </div>
                </div>

                {release.releaseNotes ? (
                  <div className="dockr-release-notes">
                    <div
                      className="dockr-readme"
                      dangerouslySetInnerHTML={{
                        __html: sanitizeRenderedHtml(renderMarkdown(release.releaseNotes)),
                      }}
                    />
                  </div>
                ) : null}

                <div className="dockr-release-foot">
                  <div className="dockr-card-meta">
                    <span className="dockr-badge">
                      <CheckCircle2 size={12} strokeWidth={1.7} />
                      {release.contentHash}
                    </span>
                  </div>
                  {renderActions ? (
                    <div className="dockr-release-actions">{renderActions(release)}</div>
                  ) : null}
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

function DockActionButton({ action }: { action: DockAction }): JSX.Element {
  const className = `dockr-btn dockr-btn--${action.tone || "primary"}`;
  if (action.href) {
    return (
      <a
        className={className}
        href={action.disabled ? undefined : action.href}
        target={action.target}
        rel={action.target === "_blank" ? "noreferrer" : undefined}
        aria-disabled={action.disabled ? "true" : undefined}
      >
        {action.icon}
        {action.label}
      </a>
    );
  }
  return (
    <button
      type="button"
      className={className}
      onClick={action.onAction}
      disabled={action.disabled}
    >
      {action.icon}
      {action.label}
    </button>
  );
}

function TypeIcon({ type }: { type: DockCapsuleType }): JSX.Element {
  if (type === "webapp" || type === "webpage") {
    return <ExternalLink size={22} strokeWidth={1.7} />;
  }
  if (type === "cli") {
    return <Terminal size={22} strokeWidth={1.7} />;
  }
  if (type === "service") {
    return <Zap size={22} strokeWidth={1.7} />;
  }
  return <Package size={22} strokeWidth={1.7} />;
}

function formatTypeLabel(type: DockCapsuleType): string {
  switch (type) {
    case "webapp":
      return "Web App";
    case "webpage":
      return "Webpage";
    case "cli":
      return "CLI";
    case "agent":
      return "Agent";
    case "service":
      return "Service";
    default:
      return "App";
  }
}

function formatDownloads(count: number): string {
  if (!count) return "0";
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return String(count);
}

function formatDateLabel(value?: string): string {
  if (!value) return "—";
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return value;
  return new Date(timestamp).toLocaleDateString();
}

function formatBytesLabel(sizeBytes: number): string {
  return `${(sizeBytes / (1024 * 1024)).toFixed(1)} MB`;
}

const ALLOWED_TAGS = new Set([
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "p",
  "ul",
  "ol",
  "li",
  "blockquote",
  "hr",
  "br",
  "pre",
  "code",
  "a",
  "strong",
  "em",
  "del",
]);

const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "target", "rel"]),
};

function escapeHtml(input: string): string {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function renderInline(input: string): string {
  let value = escapeHtml(input);

  value = value.replace(
    /\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
    (_match, label: string, url: string) =>
      `<a href="${url}" target="_blank" rel="noopener noreferrer">${label}</a>`,
  );
  value = value.replace(/`([^`]+)`/g, "<code>$1</code>");
  value = value.replace(/~~([^~]+)~~/g, "<del>$1</del>");
  value = value.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  value = value.replace(/__([^_]+)__/g, "<strong>$1</strong>");
  value = value.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  value = value.replace(/_([^_]+)_/g, "<em>$1</em>");

  return value;
}

function flushParagraph(paragraph: string[]): string {
  if (paragraph.length === 0) return "";
  return `<p>${renderInline(paragraph.join(" "))}</p>`;
}

function renderMarkdown(markdown: string): string {
  const lines = markdown.split(/\r?\n/);
  const blocks: string[] = [];
  const paragraph: string[] = [];
  const codeLines: string[] = [];
  const quoteLines: string[] = [];

  let inCode = false;
  let listMode: "ul" | "ol" | null = null;

  const closeList = () => {
    if (!listMode) return;
    blocks.push(`</${listMode}>`);
    listMode = null;
  };

  const flushParagraphBlock = () => {
    const html = flushParagraph(paragraph);
    if (html) blocks.push(html);
    paragraph.length = 0;
  };

  const flushQuoteBlock = () => {
    if (quoteLines.length === 0) return;
    blocks.push(
      `<blockquote>${quoteLines
        .map((line) => `<p>${renderInline(line)}</p>`)
        .join("")}</blockquote>`,
    );
    quoteLines.length = 0;
  };

  for (const rawLine of lines) {
    const line = rawLine.trimEnd();
    const lineStart = line.trimStart();

    if (lineStart.startsWith("```")) {
      if (inCode) {
        blocks.push(`<pre><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`);
        codeLines.length = 0;
        inCode = false;
      } else {
        flushQuoteBlock();
        closeList();
        flushParagraphBlock();
        inCode = true;
      }
      continue;
    }

    if (inCode) {
      codeLines.push(rawLine);
      continue;
    }

    if (!lineStart) {
      flushQuoteBlock();
      closeList();
      flushParagraphBlock();
      continue;
    }

    const quote = lineStart.match(/^>\s?(.*)$/);
    if (quote) {
      closeList();
      flushParagraphBlock();
      quoteLines.push(quote[1]);
      continue;
    }
    flushQuoteBlock();

    const heading = lineStart.match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      closeList();
      flushParagraphBlock();
      const level = heading[1].length;
      blocks.push(`<h${level}>${renderInline(heading[2])}</h${level}>`);
      continue;
    }

    const hr = lineStart.match(/^([-*_])\1{2,}$/);
    if (hr) {
      closeList();
      flushParagraphBlock();
      blocks.push("<hr />");
      continue;
    }

    const ul = lineStart.match(/^[-*]\s+(.+)$/);
    if (ul) {
      flushParagraphBlock();
      if (listMode !== "ul") {
        closeList();
        listMode = "ul";
        blocks.push("<ul>");
      }
      blocks.push(`<li>${renderInline(ul[1])}</li>`);
      continue;
    }

    const ol = lineStart.match(/^\d+\.\s+(.+)$/);
    if (ol) {
      flushParagraphBlock();
      if (listMode !== "ol") {
        closeList();
        listMode = "ol";
        blocks.push("<ol>");
      }
      blocks.push(`<li>${renderInline(ol[1])}</li>`);
      continue;
    }

    closeList();
    paragraph.push(lineStart.trim());
  }

  flushQuoteBlock();
  closeList();
  flushParagraphBlock();
  if (inCode) {
    blocks.push(`<pre><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`);
  }

  return blocks.join("\n");
}

function sanitizeHtmlFallback(input: string): string {
  return input
    .replace(/<script[\s\S]*?>[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?>[\s\S]*?<\/style>/gi, "")
    .replace(/\s(on\w+)=("[^"]*"|'[^']*')/gi, "")
    .replace(/javascript:/gi, "");
}

function sanitizeRenderedHtml(input: string): string {
  if (typeof window === "undefined" || typeof window.DOMParser === "undefined") {
    return sanitizeHtmlFallback(input);
  }

  const parser = new window.DOMParser();
  const document = parser.parseFromString(`<div>${input}</div>`, "text/html");
  const root = document.body.firstElementChild;
  if (!root) return "";

  const sanitizeNode = (node: ChildNode) => {
    const children = Array.from(node.childNodes);
    for (const child of children) {
      sanitizeNode(child);
    }

    if (node.nodeType !== 1) return;
    const element = node as HTMLElement;
    const tag = element.tagName.toLowerCase();

    if (!ALLOWED_TAGS.has(tag)) {
      const parent = element.parentNode;
      if (!parent) return;
      while (element.firstChild) {
        parent.insertBefore(element.firstChild, element);
      }
      parent.removeChild(element);
      return;
    }

    for (const attr of Array.from(element.attributes)) {
      const attrName = attr.name.toLowerCase();
      if (!ALLOWED_ATTRS[tag]?.has(attrName)) {
        element.removeAttribute(attr.name);
      }
    }

    if (tag === "a") {
      const href = element.getAttribute("href") ?? "";
      if (!/^https?:\/\//i.test(href)) {
        element.removeAttribute("href");
      }
      element.setAttribute("target", "_blank");
      element.setAttribute("rel", "noopener noreferrer");
    }
  };

  for (const child of Array.from(root.childNodes)) {
    sanitizeNode(child);
  }

  return root.innerHTML;
}