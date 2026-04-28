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
      tabLabel: string;
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

function normalizeSegment(value: string | undefined): string {
  return (value ?? "").trim().toLowerCase();
}

function parseHostPanelView(): HostPanelView {
  const path = window.location.pathname || "/launcher";
  const segments = path.split("/").filter(Boolean);

  if (segments.length === 0 || segments[0] === "launcher") {
    return { kind: "launcher", path };
  }

  if (segments[0] === "settings") {
    const section =
      sectionById.get(normalizeSegment(segments[1])) ?? settingsSections[0];
    return {
      kind: "settings",
      path,
      section,
    };
  }

  if (segments[0] === "capsule" && segments[1]) {
    const tab = normalizeSegment(segments[2]) || "overview";
    return {
      kind: "capsule-detail",
      path,
      paneId: segments[1],
      tabLabel: tab.replace(/-/g, " "),
    };
  }

  return { kind: "unknown", path };
}

function renderRoutePill(path: string) {
  return (
    <div className="route-pill" aria-label="Current host panel route">
      <span className="route-pill__label">Route</span>
      <strong>{`capsule-host://panel${path}`}</strong>
    </div>
  );
}

function renderLauncher() {
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
          <a className="info-card info-card--link" href={card.href} key={card.title}>
            <p className="info-card__eyebrow">Host Route</p>
            <h2>{card.title}</h2>
            <p>{card.detail}</p>
          </a>
        ))}
      </section>
    </>
  );
}

function renderSettings(section: SettingsSection, path: string) {
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
            <li>Persistence restores this section after restart via host panel route replay.</li>
            <li>Bridge commands and live settings data can land here without reviving native overlays.</li>
          </ul>
        </article>
      </section>
    </>
  );
}

function renderCapsuleDetail(paneId: string, tabLabel: string, path: string) {
  return (
    <section className="hero-card hero-card--detail">
      <div>
        <p className="eyebrow">Ato Desktop / Capsule Detail</p>
        <h1>{`Pane ${paneId}`}</h1>
        <p className="lede">
          This placeholder route reserves a host-owned detail surface for pane
          metadata, permissions, logs, and update state.
        </p>
      </div>
      <div className="detail-stack">
        {renderRoutePill(path)}
        <div className="route-meta-card">
          <span className="route-meta-card__label">Active Tab</span>
          <strong>{tabLabel}</strong>
        </div>
      </div>
    </section>
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
  const view = parseHostPanelView();

  return (
    <main className="app-shell">
      {view.kind === "launcher" && renderLauncher()}
      {view.kind === "settings" && renderSettings(view.section, view.path)}
      {view.kind === "capsule-detail" &&
        renderCapsuleDetail(view.paneId, view.tabLabel, view.path)}
      {view.kind === "unknown" && renderUnknown(view.path)}
    </main>
  );
}