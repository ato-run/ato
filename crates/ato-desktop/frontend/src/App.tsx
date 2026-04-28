import { startTransition, useEffect, useState } from "react";

type HostPanelView =
  | {
      kind: "launcher";
      path: string;
    }
  | {
      kind: "settings";
      path: string;
      section: SettingsSection;
    }
  | {
      kind: "capsule-detail";
      path: string;
      paneId: string;
      tab: CapsuleDetailTab;
    }
  | {
      kind: "unknown";
      path: string;
    };

type SettingsSection = {
  id: string;
  label: string;
  summary: string;
  detail: string;
};

type CapsuleDetailTab = {
  id: string;
  label: string;
  eyebrow: string;
  summary: string;
  detail: string;
};

type CapsuleDetailLogEntry = {
  stage: string;
  tone: string;
  message: string;
};

type CapsuleDetailNetworkEntry = {
  method: string;
  url: string;
  status?: number | null;
  durationMs?: number | null;
};

type CapsuleDetailUpdate = {
  kind: string;
  current?: string;
  latest?: string;
  targetHandle?: string;
  message?: string;
};

type CapsuleDetailPayload = {
  paneId: number;
  title: string;
  handle: string;
  canonicalHandle?: string | null;
  sourceLabel?: string | null;
  trustLabel: string;
  restricted: boolean;
  versionLabel: string;
  sessionLabel: string;
  sessionId?: string | null;
  adapter?: string | null;
  runtimeLabel?: string | null;
  displayStrategy?: string | null;
  servedBy?: string | null;
  routeLabel: string;
  manifestPath?: string | null;
  logPath?: string | null;
  localUrl?: string | null;
  healthcheckUrl?: string | null;
  invokeUrl?: string | null;
  quickOpenUrl?: string | null;
  capabilities: string[];
  logs: CapsuleDetailLogEntry[];
  network: CapsuleDetailNetworkEntry[];
  update?: CapsuleDetailUpdate | null;
};

type HostPanelPayload = {
  capsuleDetail?: CapsuleDetailPayload | null;
};

declare global {
  interface Window {
    __ATO_HOST_PANEL_PAYLOAD__?: HostPanelPayload;
    __ATO_HOST_PANEL_HYDRATE__?: (payload: HostPanelPayload) => void;
    __ATO_HOST_PANEL_NOTIFY__?: (message: { kind: string; path: string }) => void;
  }
}

const settingsSections: SettingsSection[] = [
  {
    id: "general",
    label: "General",
    summary: "Startup defaults, appearance, and update behavior.",
    detail: "Desktop-wide defaults live here: theme mode, launch-at-login, and release behavior.",
  },
  {
    id: "account",
    label: "Account",
    summary: "Identity, tokens, and device-level session state.",
    detail: "The account host panel will surface ato.run identity, handoff state, and local credentials.",
  },
  {
    id: "runtime",
    label: "Runtime",
    summary: "Execution environment and engine-level preferences.",
    detail: "Runtime settings will eventually control resolver behavior, execution engines, and launch policy.",
  },
  {
    id: "sandbox",
    label: "Sandbox",
    summary: "Filesystem, process, and network boundaries.",
    detail: "Sandbox policy is a host-owned concern, so this route belongs in the host panel shell.",
  },
  {
    id: "trust",
    label: "Trust",
    summary: "Registry provenance and local trust decisions.",
    detail: "Trust state will move out of GPUI overlays into a durable, addressable route surface.",
  },
  {
    id: "registry",
    label: "Registry",
    summary: "Store endpoints and discovery controls.",
    detail: "Registry configuration will render as a host-owned page instead of a native overlay.",
  },
  {
    id: "projection",
    label: "Delivery",
    summary: "Projection, sharing, and host-side delivery surfaces.",
    detail: "Delivery settings are kept host-owned so they can evolve without guest runtime coupling.",
  },
  {
    id: "developer",
    label: "Developer",
    summary: "Devtools toggles, diagnostics, and experimental shell features.",
    detail: "The developer route is a useful proving ground for host-panel-specific navigation and bridge work.",
  },
  {
    id: "about",
    label: "About",
    summary: "Versioning, release channel, and runtime metadata.",
    detail: "About remains a host route because it summarizes the desktop shell itself, not a guest capsule.",
  },
];

const capsuleDetailTabs: CapsuleDetailTab[] = [
  {
    id: "overview",
    label: "Overview",
    eyebrow: "Session Summary",
    summary: "Route, trust, runtime, and the quick state snapshot for the active pane.",
    detail: "Overview is the landing tab for the in-tab capsule overlay. It is the fastest place to answer what is running, where it came from, and whether it looks healthy.",
  },
  {
    id: "permissions",
    label: "Permissions",
    eyebrow: "Capability Boundary",
    summary: "Filesystem, network, and host capability grants associated with this pane.",
    detail: "Permissions is where the frontend host panel summarizes granted capabilities and trust boundaries for the current capsule pane.",
  },
  {
    id: "logs",
    label: "Logs",
    eyebrow: "Runtime Timeline",
    summary: "Recent process and lifecycle log lines for the selected capsule pane.",
    detail: "Logs is backed by the host-side capsule log stream so the overlay remains useful even when the guest page itself is unhealthy.",
  },
  {
    id: "update",
    label: "Update",
    eyebrow: "Version Drift",
    summary: "Snapshot label, release freshness, and update actions for the mounted capsule.",
    detail: "Update reflects the host-side version check state currently tracked for this pane.",
  },
  {
    id: "api",
    label: "API",
    eyebrow: "Host Bridge",
    summary: "IPC surfaces, local invoke URLs, and host-visible integration endpoints.",
    detail: "API surfaces are host-owned and remain inspectable here even when guest UI code is broken.",
  },
];

const launcherCards = [
  {
    title: "Launcher",
    detail: "Host-owned landing surface for new tabs and desktop-level affordances.",
    href: "/launcher",
  },
  {
    title: "Settings",
    detail: "Addressable singleton task rendered at capsule-host://panel/settings/<section>.",
    href: "/settings/general",
  },
  {
    title: "Capsule Detail",
    detail: "Per-pane host metadata surface for logs, permissions, and lifecycle state.",
    href: "/capsule/7/overview",
  },
];

const sectionById = new Map(settingsSections.map((section) => [section.id, section]));
const capsuleDetailTabById = new Map(capsuleDetailTabs.map((tab) => [tab.id, tab]));

function normalizeSegment(value: string | undefined): string {
  return (value ?? "").trim().toLowerCase();
}

function readHostPanelPayload(): HostPanelPayload | null {
  return window.__ATO_HOST_PANEL_PAYLOAD__ ?? null;
}

function parseHostPanelView(path: string): HostPanelView {
  const segments = path.split("/").filter(Boolean);

  if (segments.length === 0 || segments[0] === "launcher") {
    return { kind: "launcher", path };
  }

  if (segments[0] === "settings") {
    const section = sectionById.get(normalizeSegment(segments[1])) ?? settingsSections[0];
    return { kind: "settings", path, section };
  }

  if (segments[0] === "capsule" && segments[1]) {
    const tab = capsuleDetailTabById.get(normalizeSegment(segments[2])) ?? capsuleDetailTabs[0];
    return { kind: "capsule-detail", path, paneId: segments[1], tab };
  }

  return { kind: "unknown", path };
}

function navigateHostPanel(path: string, setPath: (path: string) => void) {
  if (window.location.pathname === path) {
    return;
  }
  window.history.pushState({}, "", path);
  startTransition(() => {
    setPath(path);
  });
  window.__ATO_HOST_PANEL_NOTIFY__?.({ kind: "route-change", path });
}

function renderRoutePill(path: string) {
  return (
    <div className="route-pill" aria-label="Current host panel route">
      <span className="route-pill__label">Route</span>
      <strong>{`capsule-host://panel${path}`}</strong>
    </div>
  );
}

function renderLauncher(setPath: (path: string) => void) {
  return (
    <>
      <section className="hero-card hero-card--launcher">
        <div>
          <p className="eyebrow">Ato Desktop / Host Panel</p>
          <h1>Launcher</h1>
          <p className="lede">
            New tabs now resolve through a host-owned route. This view is the
            shell-side landing surface that can evolve independently from guest
            capsules and native GPUI overlays.
          </p>
        </div>
        {renderRoutePill("/launcher")}
      </section>

      <section className="info-grid" aria-label="Available host panel routes">
        {launcherCards.map((card) => (
          <a
            className="info-card info-card--link"
            href={card.href}
            key={card.title}
            onClick={(event) => {
              event.preventDefault();
              navigateHostPanel(card.href, setPath);
            }}
          >
            <p className="info-card__eyebrow">Host Route</p>
            <h2>{card.title}</h2>
            <p>{card.detail}</p>
          </a>
        ))}
      </section>
    </>
  );
}

function renderSettings(section: SettingsSection, path: string, setPath: (path: string) => void) {
  return (
    <>
      <section className="hero-card hero-card--settings">
        <div>
          <p className="eyebrow">Ato Desktop / Settings</p>
          <h1>{section.label}</h1>
          <p className="lede">{section.detail}</p>
        </div>
        {renderRoutePill(path)}
      </section>

      <section className="settings-layout" aria-label="Settings sections">
        <nav className="settings-nav">
          {settingsSections.map((item) => {
            const active = item.id === section.id;
            return (
              <a
                aria-current={active ? "page" : undefined}
                className={active ? "settings-link settings-link--active" : "settings-link"}
                href={`/settings/${item.id}`}
                key={item.id}
                onClick={(event) => {
                  event.preventDefault();
                  navigateHostPanel(`/settings/${item.id}`, setPath);
                }}
              >
                <span className="settings-link__title">{item.label}</span>
                <span className="settings-link__summary">{item.summary}</span>
              </a>
            );
          })}
        </nav>

        <article className="settings-detail">
          <p className="settings-detail__eyebrow">Current Section</p>
          <h2>{section.label}</h2>
          <p>{section.detail}</p>
          <ul className="detail-list">
            <li>Route-driven singleton task selection is now modeled in desktop state.</li>
            <li>Frontend route changes report back to the shell so native task titles stay in sync.</li>
            <li>Bridge-driven live data can land here without reviving native overlays.</li>
          </ul>
        </article>
      </section>
    </>
  );
}

function renderCapsuleOverview(detail: CapsuleDetailPayload | null) {
  const cards = [
    {
      label: "Route",
      value: detail?.routeLabel ?? "pending",
      body: detail?.sourceLabel ?? "No source metadata yet.",
    },
    {
      label: "Session",
      value: detail?.sessionLabel ?? "unknown",
      body: detail?.runtimeLabel ?? "Runtime metadata is still loading.",
    },
    {
      label: "Trust",
      value: detail?.trustLabel ?? "pending",
      body: detail?.versionLabel ?? "Version metadata unavailable.",
    },
  ];

  return (
    <div className="capsule-detail-stack">
      <div className="capsule-summary-grid">
        {cards.map((card) => (
          <section className="capsule-summary-card" key={card.label}>
            <span className="route-meta-card__label">{card.label}</span>
            <strong>{card.value}</strong>
            <p>{card.body}</p>
          </section>
        ))}
      </div>
      <section className="capsule-data-card">
        <p className="route-meta-card__label">Handle</p>
        <strong>{detail?.canonicalHandle ?? detail?.handle ?? "Unknown handle"}</strong>
        <p>{detail?.title ?? "The active pane title will appear here once available."}</p>
      </section>
    </div>
  );
}

function renderCapsulePermissions(detail: CapsuleDetailPayload | null) {
  const capabilities = detail?.capabilities ?? [];

  return (
    <div className="capsule-detail-stack">
      <section className="capsule-data-card">
        <p className="route-meta-card__label">Capability Grants</p>
        {capabilities.length > 0 ? (
          <div className="capsule-chip-row">
            {capabilities.map((capability) => (
              <span className="capsule-chip" key={capability}>
                {capability}
              </span>
            ))}
          </div>
        ) : (
          <p className="capsule-empty">No explicit capability grants are attached to this pane.</p>
        )}
      </section>
      <section className="capsule-data-card">
        <p className="route-meta-card__label">Boundary</p>
        <dl className="capsule-kv-list">
          <div>
            <dt>Restricted</dt>
            <dd>{detail?.restricted ? "yes" : "no"}</dd>
          </div>
          <div>
            <dt>Adapter</dt>
            <dd>{detail?.adapter ?? "none"}</dd>
          </div>
          <div>
            <dt>Served By</dt>
            <dd>{detail?.servedBy ?? "n/a"}</dd>
          </div>
        </dl>
      </section>
    </div>
  );
}

function renderCapsuleLogs(detail: CapsuleDetailPayload | null) {
  const logs = detail?.logs ?? [];

  return logs.length > 0 ? (
    <div className="capsule-log-list">
      {logs.map((entry, index) => (
        <article className="capsule-log-entry" key={`${entry.stage}-${index}`}>
          <div className="capsule-log-entry__meta">
            <span>{entry.stage}</span>
            <span>{entry.tone}</span>
          </div>
          <p>{entry.message}</p>
        </article>
      ))}
    </div>
  ) : (
    <p className="capsule-empty">No runtime log lines have been captured for this pane yet.</p>
  );
}

function renderCapsuleUpdate(detail: CapsuleDetailPayload | null) {
  const update = detail?.update;

  return (
    <section className="capsule-data-card">
      <p className="route-meta-card__label">Update Status</p>
      <strong>{update?.kind ?? "idle"}</strong>
      <dl className="capsule-kv-list">
        <div>
          <dt>Current</dt>
          <dd>{update?.current ?? detail?.versionLabel ?? "unknown"}</dd>
        </div>
        <div>
          <dt>Latest</dt>
          <dd>{update?.latest ?? "n/a"}</dd>
        </div>
        <div>
          <dt>Target</dt>
          <dd>{update?.targetHandle ?? "n/a"}</dd>
        </div>
      </dl>
      {update?.message ? <p>{update.message}</p> : null}
    </section>
  );
}

function renderCapsuleApi(detail: CapsuleDetailPayload | null) {
  const endpoints = [
    ["Invoke", detail?.invokeUrl],
    ["Local", detail?.localUrl],
    ["Healthcheck", detail?.healthcheckUrl],
    ["Manifest", detail?.manifestPath],
    ["Logs", detail?.logPath],
  ] as const;

  return (
    <div className="capsule-detail-stack">
      <section className="capsule-data-card">
        <p className="route-meta-card__label">Endpoints</p>
        <dl className="capsule-kv-list">
          {endpoints.map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value ?? "n/a"}</dd>
            </div>
          ))}
        </dl>
      </section>
      <section className="capsule-data-card">
        <p className="route-meta-card__label">Network</p>
        {(detail?.network ?? []).length > 0 ? (
          <div className="capsule-log-list">
            {detail!.network.map((entry, index) => (
              <article className="capsule-log-entry" key={`${entry.method}-${index}`}>
                <div className="capsule-log-entry__meta">
                  <span>{entry.method}</span>
                  <span>{entry.status ?? "pending"}</span>
                </div>
                <p>{entry.url}</p>
              </article>
            ))}
          </div>
        ) : (
          <p className="capsule-empty">No host-visible network events have been recorded.</p>
        )}
      </section>
    </div>
  );
}

function renderCapsuleDetail(
  paneId: string,
  tab: CapsuleDetailTab,
  path: string,
  setPath: (path: string) => void,
  detail: CapsuleDetailPayload | null,
) {
  return (
    <>
      <section className="hero-card hero-card--detail">
        <div>
          <p className="eyebrow">Ato Desktop / Capsule Detail</p>
          <h1>{detail?.title ?? `Pane ${paneId}`}</h1>
          <p className="lede">{tab.detail}</p>
        </div>
        <div className="detail-stack">
          {renderRoutePill(path)}
          <div className="route-meta-card">
            <span className="route-meta-card__label">Active Tab</span>
            <strong>{tab.label}</strong>
          </div>
        </div>
      </section>

      <section className="capsule-detail-layout" aria-label="Capsule detail sections">
        <nav className="capsule-tab-nav">
          {capsuleDetailTabs.map((item) => {
            const active = item.id === tab.id;
            return (
              <a
                aria-current={active ? "page" : undefined}
                className={active ? "capsule-tab-link capsule-tab-link--active" : "capsule-tab-link"}
                href={`/capsule/${paneId}/${item.id}`}
                key={item.id}
                onClick={(event) => {
                  event.preventDefault();
                  navigateHostPanel(`/capsule/${paneId}/${item.id}`, setPath);
                }}
              >
                <span className="capsule-tab-link__title">{item.label}</span>
                <span className="capsule-tab-link__summary">{item.summary}</span>
              </a>
            );
          })}
        </nav>

        <article className="capsule-detail-panel">
          <p className="settings-detail__eyebrow">{tab.eyebrow}</p>
          <h2>{tab.label}</h2>
          <p>{detail?.routeLabel ?? tab.detail}</p>

          {tab.id === "overview" && renderCapsuleOverview(detail)}
          {tab.id === "permissions" && renderCapsulePermissions(detail)}
          {tab.id === "logs" && renderCapsuleLogs(detail)}
          {tab.id === "update" && renderCapsuleUpdate(detail)}
          {tab.id === "api" && renderCapsuleApi(detail)}
        </article>
      </section>
    </>
  );
}

function renderUnknown(path: string) {
  return (
    <section className="hero-card hero-card--detail">
      <div>
        <p className="eyebrow">Ato Desktop / Host Panel</p>
        <h1>Unknown Route</h1>
        <p className="lede">
          The desktop asked for a host panel route that this scaffold does not
          recognize yet.
        </p>
      </div>
      {renderRoutePill(path)}
    </section>
  );
}

export default function App() {
  const [path, setPath] = useState(() => window.location.pathname || "/launcher");
  const [payload, setPayload] = useState<HostPanelPayload | null>(() => readHostPanelPayload());

  useEffect(() => {
    const onPopState = () => {
      startTransition(() => {
        setPath(window.location.pathname || "/launcher");
      });
    };
    const onPayload = (event: Event) => {
      const detail = (event as CustomEvent<HostPanelPayload>).detail ?? readHostPanelPayload();
      startTransition(() => {
        setPayload(detail);
      });
    };

    window.addEventListener("popstate", onPopState);
    window.addEventListener("ato-host-panel-payload", onPayload as EventListener);
    window.__ATO_HOST_PANEL_NOTIFY__?.({
      kind: "route-change",
      path: window.location.pathname || "/launcher",
    });

    return () => {
      window.removeEventListener("popstate", onPopState);
      window.removeEventListener("ato-host-panel-payload", onPayload as EventListener);
    };
  }, []);

  const view = parseHostPanelView(path);
  const capsuleDetail = payload?.capsuleDetail ?? null;

  return (
    <main className="app-shell">
      {view.kind === "launcher" && renderLauncher(setPath)}
      {view.kind === "settings" && renderSettings(view.section, view.path, setPath)}
      {view.kind === "capsule-detail" &&
        renderCapsuleDetail(view.paneId, view.tab, view.path, setPath, capsuleDetail)}
      {view.kind === "unknown" && renderUnknown(view.path)}
    </main>
  );
}