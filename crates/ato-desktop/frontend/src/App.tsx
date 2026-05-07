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
  iconSource?: string | null;
};

type OpenCapsuleSummary = {
  paneId: number;
  title: string;
  handle: string;
  sessionLabel: string;
  runtimeLabel: string | null;
  logCount: number;
};

type LauncherData = {
  openCapsules: OpenCapsuleSummary[];
  authStatus: string;
  publisherHandle: string | null;
};

type HostPanelPayload = {
  capsuleDetail?: CapsuleDetailPayload | null;
  launcherData?: LauncherData | null;
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
    detail:
      "Overview is the landing tab for the in-tab capsule overlay. It is the fastest place to answer what is running, where it came from, and whether it looks healthy.",
  },
  {
    id: "permissions",
    label: "Permissions",
    eyebrow: "Capability Boundary",
    summary: "Filesystem, network, and host capability grants associated with this pane.",
    detail:
      "Permissions is where the frontend host panel summarizes granted capabilities and trust boundaries for the current capsule pane.",
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


const recommendedCapsules = [
  {
    name: "markdown-preview",
    desc: "Live Markdown renderer with math support",
    color: "#2563eb",
    icon: "file-text",
    installed: false,
  },
  {
    name: "git-stats",
    desc: "Local repository contribution analytics",
    color: "#dc2626",
    icon: "git-branch",
    installed: false,
  },
  {
    name: "color-converter",
    desc: "HEX / RGB / HSL / OKLCH color tool",
    color: "#7c3aed",
    icon: "palette",
    installed: true,
  },
  {
    name: "json-formatter",
    desc: "Pretty-print, minify, and validate JSON",
    color: "#059669",
    icon: "json",
    installed: false,
  },
  {
    name: "jwt-decoder",
    desc: "Decode and inspect JWT tokens",
    color: "#d97706",
    icon: "key",
    installed: false,
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
  let segments = path.split("/").filter(Boolean);

  // Strip leading "panel" if present (e.g., /panel/settings/general)
  if (segments[0] === "panel") {
    segments = segments.slice(1);
  }

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

function toast(message: string) {
  const box = document.getElementById("toastBox");
  if (!box) return;
  const el = document.createElement("div");
  el.className = "toast";
  el.innerHTML = `<span class="iconify" data-icon="lucide:info" style="font-size:14px"></span>${message}`;
  box.appendChild(el);
  setTimeout(() => el.remove(), 2800);
}

function getIconForLanguage(iconBg: string): string {
  const icons: Record<string, string> = {
    py: "lucide:languages",
    js: "lucide:image",
    rs: "lucide:book-open",
    go: "lucide:database",
  };
  return icons[iconBg] || "lucide:package";
}

function getSettingsIcon(id: string): string {
  const icons: Record<string, string> = {
    general: "sliders-horizontal",
    account: "user",
    runtime: "cpu",
    sandbox: "shield",
    trust: "fingerprint",
    registry: "package",
    projection: "monitor",
    developer: "code",
    about: "info",
  };
  return icons[id] || "circle";
}

function rpick(e: React.MouseEvent<HTMLButtonElement>) {
  const parent = (e.target as HTMLElement).closest(".rg, .rg-v");
  if (!parent) return;
  parent.querySelectorAll(".rp").forEach((el) => el.classList.remove("active"));
  (e.target as HTMLElement).classList.add("active");
}

function App() {
  const [path, setPath] = useState(window.location.pathname);
  const [payload, setPayload] = useState<HostPanelPayload | null>(readHostPanelPayload);
  const view = parseHostPanelView(path);

  useEffect(() => {
    const onPopState = () => setPath(window.location.pathname);
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  useEffect(() => {
    const onPayload = (e: Event) =>
      setPayload((e as CustomEvent<HostPanelPayload>).detail);
    window.addEventListener("ato-host-panel-payload", onPayload);
    return () => window.removeEventListener("ato-host-panel-payload", onPayload);
  }, []);

  return (
    <>
      {view.kind === "launcher" && renderLauncher(setPath, payload?.launcherData ?? null)}
      {view.kind === "settings" && renderSettings(view.section, path, setPath)}
      {view.kind === "capsule-detail" && renderCapsuleDetail(view.paneId, view.tab, path, setPath, payload?.capsuleDetail ?? null, payload)}
      {view.kind === "unknown" && <div className="home" style={{ padding: "48px" }}>Unknown route: {path}</div>}

      {/* Toast Container */}
      <div className="toast-box" id="toastBox"></div>
    </>
  );
}

function getIconForRuntime(runtimeLabel: string | null): string {
  const label = (runtimeLabel ?? "").toLowerCase();
  if (label.includes("python")) return "py";
  if (label.includes("node") || label.includes("deno") || label.includes("bun")) return "js";
  if (label.includes("rust")) return "rs";
  if (label.includes("go")) return "go";
  return "default";
}

function sessionBadge(sessionLabel: string): { text: string; status: string } {
  switch (sessionLabel) {
    case "Mounted":
      return { text: "Running", status: "running" };
    case "Launching":
    case "Resolving":
    case "Materializing":
      return { text: sessionLabel, status: "starting" };
    case "Closed":
      return { text: "Stopped", status: "stopped" };
    case "LaunchFailed":
      return { text: "Failed", status: "failed" };
    default:
      return { text: sessionLabel, status: "stopped" };
  }
}

function renderLauncher(setPath: (path: string) => void, launcherData: LauncherData | null) {
  const openCapsules = launcherData?.openCapsules ?? [];
  return (
    <div className="home">
      <div className="home-hero">
        <div className="omnibar">
          <input
            className="omnibar-input"
            type="text"
            placeholder="Search capsules, paste a GitHub URL…"
            onFocus={() => toast("Search focused")}
          />
          <span className="iconify omnibar-icon" data-icon="lucide:search"></span>
          <div className="omnibar-kbd">
            <span className="kbd">⌘</span>
            <span className="kbd">K</span>
          </div>
        </div>
        <div className="omnibar-hint">
          Press <span className="kbd">Enter</span> to search
          <span className="sep">·</span> <span className="kbd">⌘</span>
          <span className="kbd">V</span> to paste URL
        </div>
      </div>

      <div className="sec-head">
        <span className="sec-title">Open Capsules</span>
        <button className="sec-link" onClick={() => toast("View all capsules")}>
          View all →
        </button>
      </div>
      <div className="cards-grid">
        {openCapsules.length === 0 ? (
          <div className="launcher-empty">No capsules are currently open.</div>
        ) : (
          openCapsules.map((capsule) => {
            const iconKey = getIconForRuntime(capsule.runtimeLabel);
            const badge = sessionBadge(capsule.sessionLabel);
            return (
              <div
                className="cc"
                key={capsule.paneId}
                onClick={() => toast(`Switching to ${capsule.title}…`)}
              >
                <div className="cc-top">
                  <div className={`cc-icon ${iconKey}`}>
                    <span className="iconify" data-icon={getIconForLanguage(iconKey)}></span>
                  </div>
                  <span className={`badge ${badge.status}`}>{badge.text}</span>
                </div>
                <div className="cc-name">{capsule.title}</div>
                <div className="cc-author">{capsule.handle}</div>
                <div className="cc-meta">
                  <span className="cc-rt">{capsule.runtimeLabel ?? "—"}</span>
                  {capsule.logCount > 0 && (
                    <span className="cc-sz">{capsule.logCount} logs</span>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>

      <div className="sec-head">
        <span className="sec-title">Quick Actions</span>
      </div>
      <div className="qa-grid">
        <button
          className="qa"
          onClick={() => {
            const input = document.querySelector(".omnibar-input") as HTMLInputElement;
            input?.focus();
            toast("Paste a GitHub URL to install");
          }}
        >
          <div className="qa-ico gh">
            <span className="iconify" data-icon="mdi:github"></span>
          </div>
          <div>
            <div className="qa-t">Install from GitHub</div>
            <div className="qa-d">Paste a repository URL above</div>
          </div>
        </button>
        <button className="qa" onClick={() => toast("Opening local workspace…")}>
          <div className="qa-ico local">
            <span className="iconify" data-icon="lucide:folder-open"></span>
          </div>
          <div>
            <div className="qa-t">Open Local Capsule</div>
            <div className="qa-d">Browse ~/.ato/workspaces</div>
          </div>
        </button>
      </div>

      <div className="sec-head">
        <span className="sec-title">Recommended</span>
        <button className="sec-link" onClick={() => toast("Browse store…")}>
          Browse store →
        </button>
      </div>
      <div className="reco">
        {recommendedCapsules.map((item) => (
          <div className="reco-row" key={item.name} onClick={() => toast(`Installing ${item.name}…`)}>
            <div
              className="reco-ico"
              style={{ background: `linear-gradient(135deg, ${item.color}, ${item.color}dd)` }}
            >
              <span className="iconify" data-icon={`lucide:${item.icon}`}></span>
            </div>
            <div className="reco-info">
              <div className="reco-n">{item.name}</div>
              <div className="reco-d">{item.desc}</div>
            </div>
            <button
              className={`reco-btn ${item.installed ? "installed" : ""}`}
              onClick={(e) => {
                e.stopPropagation();
                toast(item.installed ? "Already installed" : "Installing…");
              }}
            >
              <span className="iconify" data-icon={item.installed ? "lucide:check" : "lucide:plus"}></span>
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

function renderSettings(section: SettingsSection, path: string, setPath: (path: string) => void) {
  const renderSettingContent = () => {
    switch (section.id) {
      case "general":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Startup</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Launch at login</span>
                  </div>
                  <div className="sc">
                    <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Show in menu bar / system tray</span>
                  </div>
                  <div className="sc">
                    <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Show What's New on update</span>
                  </div>
                  <div className="sc">
                    <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Appearance</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Language</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      System <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Theme</span>
                  </div>
                  <div className="sc">
                    <div className="rg">
                      <button className="rp active" onClick={(e) => rpick(e)}>System</button>
                      <button className="rp" onClick={(e) => rpick(e)}>Light</button>
                      <button className="rp" onClick={(e) => rpick(e)}>Dark</button>
                    </div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Updates</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Update channel</span>
                  </div>
                  <div className="sc">
                    <div className="rg">
                      <button className="rp active" onClick={(e) => rpick(e)}>Stable</button>
                      <button className="rp" onClick={(e) => rpick(e)}>Beta</button>
                      <button className="rp" onClick={(e) => rpick(e)}>Nightly</button>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Automatic updates</span>
                    <span className="sl-d">Includes bundled CLI</span>
                  </div>
                  <div className="sc">
                    <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
              </div>
            </div>
          </>
        );

      case "account":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Authentication</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">
                      Signed in as <strong style={{ color: "var(--accent)" }}>@kyotori</strong>
                    </span>
                    <span className="sl-d">ato.run/u/kyotori</span>
                  </div>
                  <div className="sc">
                    <button className="btn danger" onClick={() => toast("Signing out…")}>
                      Sign Out
                    </button>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Device name</span>
                  </div>
                  <div className="sc">
                    <input className="ti" defaultValue="macbook-pro-m2" style={{ width: "160px" }} />
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Credentials</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">ATO_TOKEN</span>
                    <span className="sl-d">Stored in system keychain</span>
                  </div>
                  <div className="sc">
                    <span
                      style={{
                        fontSize: "11px",
                        fontFamily: "'JetBrains Mono', monospace",
                        color: "var(--text-muted)",
                      }}
                    >
                      ••••••••••
                    </span>
                    <button className="btn danger" onClick={() => toast("Token revoked")}>
                      Revoke
                    </button>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">credentials.toml</span>
                  </div>
                  <div className="sc" style={{ gap: "5px" }}>
                    <div className="pd">~/.config/ato/credentials.toml</div>
                    <button className="pc" onClick={() => toast("Path copied")}>
                      <span className="iconify" data-icon="lucide:copy" style={{ fontSize: "11px" }}></span>
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </>
        );

      case "runtime":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Cache</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Cache location</span>
                  </div>
                  <div className="sc" style={{ gap: "5px" }}>
                    <div className="pd">~/.ato/cache</div>
                    <button className="pc" onClick={() => toast("Path copied")}>
                      <span className="iconify" data-icon="lucide:copy" style={{ fontSize: "11px" }}></span>
                    </button>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Cache size limit</span>
                  </div>
                  <div className="sc">
                    <div className="slider-row">
                      <input type="range" className="slider" min="1" max="50" defaultValue="10" />
                      <span className="sv">10 GB</span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl"></div>
                  <div className="sc">
                    <button className="btn" onClick={() => toast("Cache cleared — 4.2 GB freed")}>
                      <span className="iconify" data-icon="lucide:trash-2" style={{ fontSize: "11px" }}></span>
                      Clear Cache
                    </button>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Workspace</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Workspace root</span>
                  </div>
                  <div className="sc" style={{ gap: "5px" }}>
                    <div className="pd">~/.ato/workspaces</div>
                    <button className="pc" onClick={() => toast("Path copied")}>
                      <span className="iconify" data-icon="lucide:copy" style={{ fontSize: "11px" }}></span>
                    </button>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Watch debounce</span>
                    <span className="sl-d">CAPSULE_WATCH_DEBOUNCE_MS</span>
                  </div>
                  <div className="sc">
                    <div className="slider-row">
                      <input type="range" className="slider" min="50" max="2000" defaultValue="300" step="50" />
                      <span className="sv">300 ms</span>
                    </div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Sandbox Tier Policy</div>
                <div className="sr" style={{ alignItems: "flex-start" }}>
                  <div className="sl">
                    <span className="sl-t">Default tier policy</span>
                    <span className="sl-d">Controls which capsule tiers can execute</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      Tier 1 only <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Unsafe execution prompt</span>
                    <span className="sl-d">When --unsafe is required</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      Always confirm <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">CAPSULE_ALLOW_UNSAFE</span>
                    <span className="sl-d">Env var shadow</span>
                  </div>
                  <div className="sc">
                    <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
              </div>
            </div>
          </>
        );

      case "sandbox":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Nacelle</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Nacelle engine</span>
                    <span className="sl-d">Required for Tier 2 execution</span>
                  </div>
                  <div className="sc">
                    <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Network Egress</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Default egress policy</span>
                  </div>
                  <div className="sc">
                    <div className="rg">
                      <button className="rp active">Deny-all</button>
                      <button className="rp">Allowlist</button>
                      <button className="rp">Proxy-only</button>
                    </div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Tailnet</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Tailnet sidecar</span>
                  </div>
                  <div className="sc">
                    <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Headscale control plane</span>
                  </div>
                  <div className="sc">
                    <input className="ti mono" defaultValue="https://hs.ato.run" style={{ width: "210px" }} />
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Host Bridge Sockets</div>
                <div className="sr" style={{ borderTop: "none", paddingTop: "4px" }}>
                  <div className="svc-info">
                    <div className="svc-dot on"></div>
                    <span className="svc-name">nacelle.sock</span>
                  </div>
                  <span className="svc-meta">pid 48291 — listening</span>
                </div>
              </div>
            </div>
          </>
        );

      case "trust":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Revocation</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Update frequency</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      24 hours <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Revocation source</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      DNS TXT (default) <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Unknown publisher TOFU</span>
                    <span className="sl-d">First-time trust</span>
                  </div>
                  <div className="sc">
                    <div className="rg">
                      <button className="rp active">Always prompt</button>
                      <button className="rp">Auto-trust</button>
                      <button className="rp">Always reject</button>
                    </div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Known Publishers</div>
                <table className="tt">
                  <thead>
                    <tr>
                      <th>Publisher</th>
                      <th>Petname</th>
                      <th>Fingerprint</th>
                      <th>State</th>
                      <th></th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr>
                      <td style={{ color: "var(--text-primary)", fontWeight: "500" }}>figma.com</td>
                      <td>
                        <input className="pe" defaultValue="Figma" />
                      </td>
                      <td className="fp">sha256:a3f2…8c1d</td>
                      <td>
                        <span className="ts ok">
                          <span className="d"></span>Verified
                        </span>
                      </td>
                      <td>
                        <button className="btn danger" style={{ padding: "0 8px", fontSize: "9.5px" }} onClick={() => toast("Revoked")}>
                          Revoke
                        </button>
                      </td>
                    </tr>
                    <tr>
                      <td style={{ color: "var(--text-primary)", fontWeight: "500" }}>linear.app</td>
                      <td>
                        <input className="pe" defaultValue="Linear" />
                      </td>
                      <td className="fp">sha256:7b1c…d4e2</td>
                      <td>
                        <span className="ts ok">
                          <span className="d"></span>Verified
                        </span>
                      </td>
                      <td>
                        <button className="btn danger" style={{ padding: "0 8px", fontSize: "9.5px" }} onClick={() => toast("Revoked")}>
                          Revoke
                        </button>
                      </td>
                    </tr>
                    <tr>
                      <td style={{ color: "var(--text-primary)", fontWeight: "500" }}>untrusted.dev</td>
                      <td>
                        <input className="pe" defaultValue="" />
                      </td>
                      <td className="fp">sha256:e9a0…3f7b</td>
                      <td>
                        <span className="ts no">
                          <span className="d"></span>Untrusted
                        </span>
                      </td>
                      <td>
                        <button className="btn" style={{ padding: "0 8px", fontSize: "9.5px" }} onClick={() => toast("Revoked")}>
                          Revoke
                        </button>
                      </td>
                    </tr>
                    <tr>
                      <td style={{ color: "var(--text-primary)", fontWeight: "500" }}>registry.local</td>
                      <td>
                        <input className="pe" defaultValue="Local Dev" />
                      </td>
                      <td className="fp">sha256:f122…a891</td>
                      <td>
                        <span className="ts uk">
                          <span className="d"></span>Unknown
                        </span>
                      </td>
                      <td>
                        <button className="btn" style={{ padding: "0 8px", fontSize: "9.5px" }} onClick={() => toast("Revoked")}>
                          Revoke
                        </button>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
            </div>
          </>
        );

      case "registry":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Store API</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Store API URL</span>
                  </div>
                  <div className="sc">
                    <input className="ti mono" defaultValue="https://api.ato.run" style={{ width: "210px" }} />
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Store site URL</span>
                  </div>
                  <div className="sc">
                    <input className="ti mono" defaultValue="https://ato.run" style={{ width: "210px" }} />
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Private Registries</div>
                <div style={{ fontSize: "11.5px", color: "var(--text-muted)", marginBottom: "12px" }}>
                  No private registries configured.
                </div>
                <div className="sr" style={{ borderTop: "none", paddingTop: "0" }}>
                  <div className="sl"></div>
                  <div className="sc">
                    <button className="btn" onClick={() => toast("Add registry…")}>
                      <span className="iconify" data-icon="lucide:plus" style={{ fontSize: "11px" }}></span>
                      Add Registry
                    </button>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Local registry port</span>
                    <span className="sl-d">ato registry serve</span>
                  </div>
                  <div className="sc">
                    <input className="ti mono" defaultValue="8080" style={{ width: "70px" }} />
                  </div>
                </div>
              </div>
            </div>
          </>
        );

      case "projection":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Projection</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Enable projection by default</span>
                    <span className="sl-d">ato install --project</span>
                  </div>
                  <div className="sc">
                    <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Projection directory</span>
                  </div>
                  <div className="sc" style={{ gap: "5px" }}>
                    <div className="pd">/Applications</div>
                    <button className="pc" onClick={() => toast("Path copied")}>
                      <span className="iconify" data-icon="lucide:copy" style={{ fontSize: "11px" }}></span>
                    </button>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Installed Capsules</div>
                <div style={{ fontSize: "11.5px", color: "var(--text-muted)", marginBottom: "12px" }}>
                  No projected capsules installed.
                </div>
                <div className="sr" style={{ borderTop: "none", paddingTop: "0" }}>
                  <div className="sl"></div>
                  <div className="sc">
                    <button className="btn" onClick={() => toast("Install…")}>
                      <span className="iconify" data-icon="lucide:download" style={{ fontSize: "11px" }}></span>
                      Install Capsule
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </>
        );

      case "developer":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Logging</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Log level</span>
                  </div>
                  <div className="sc">
                    <div className="sel" onClick={() => toast("Dropdown")}>
                      warn <span className="iconify" data-icon="lucide:chevron-down"></span>
                    </div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Log output</span>
                  </div>
                  <div className="sc" style={{ gap: "5px" }}>
                    <div className="pd">stderr</div>
                    <button className="pc">
                      <span className="iconify" data-icon="lucide:copy" style={{ fontSize: "11px" }}></span>
                    </button>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Telemetry</div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Send crash reports</span>
                  </div>
                  <div className="sc">
                    <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
                <div className="sr">
                  <div className="sl">
                    <span className="sl-t">Include usage statistics</span>
                  </div>
                  <div className="sc">
                    <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Experimental Features</div>
                <div className="flag-row">
                  <div>
                    <span className="flag-label">Parallel branch execution</span>
                    <span className="flag-desc">Run multiple agent branches</span>
                  </div>
                  <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                </div>
                <div className="flag-row">
                  <div>
                    <span className="flag-label">Projected file preview</span>
                    <span className="flag-desc">Preview native-installed files</span>
                  </div>
                  <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                </div>
                <div className="flag-row">
                  <div>
                    <span className="flag-label">Hot-reload capsules</span>
                    <span className="flag-desc">Auto-reload on source change</span>
                  </div>
                  <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                </div>
                <div className="flag-row">
                  <div>
                    <span className="flag-label">Incremental GC</span>
                    <span className="flag-desc">Experimental collector</span>
                  </div>
                  <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                </div>
              </div>
            </div>
          </>
        );

      case "about":
        return (
          <>
            <div className="s-card">
              <div className="s-card-sec">
                <div className="s-card-title">Version</div>
                <div className="dg">
                  <div className="dc">
                    <div className="dc-l">ato</div>
                    <div className="dc-v">0.24.1</div>
                  </div>
                  <div className="dc">
                    <div className="dc-l">Build</div>
                    <div className="dc-v sub">a3f2c8d (main)</div>
                  </div>
                </div>
                <div className="dg" style={{ marginTop: "6px" }}>
                  <div className="dc">
                    <div className="dc-l">Nacelle</div>
                    <div className="dc-v sub">0.18.0</div>
                  </div>
                  <div className="dc">
                    <div className="dc-l">Protocol</div>
                    <div className="dc-v sub">capsule://v2.1</div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">System</div>
                <div className="dg">
                  <div className="dc">
                    <div className="dc-l">OS</div>
                    <div className="dc-v sub">macOS 15.3.1</div>
                  </div>
                  <div className="dc">
                    <div className="dc-l">Arch</div>
                    <div className="dc-v sub">arm64</div>
                  </div>
                </div>
                <div className="dg" style={{ marginTop: "6px" }}>
                  <div className="dc">
                    <div className="dc-l">Kernel</div>
                    <div className="dc-v sub">24.3.0</div>
                  </div>
                  <div className="dc">
                    <div className="dc-l">Device</div>
                    <div className="dc-v sub">MacBook Pro M2</div>
                  </div>
                </div>
              </div>
              <div className="s-card-sec">
                <div className="s-card-title">Running Services</div>
                <div className="svc-row">
                  <div className="svc-info">
                    <div className="svc-dot on"></div>
                    <span className="svc-name">nacelle</span>
                  </div>
                  <span className="svc-meta">pid 48291</span>
                </div>
                <div className="svc-row">
                  <div className="svc-info">
                    <div className="svc-dot on"></div>
                    <span className="svc-name">ato-tsnetd</span>
                  </div>
                  <span className="svc-meta" style={{ color: "var(--green)" }}>
                    connected
                  </span>
                </div>
                <div className="svc-row">
                  <div className="svc-info">
                    <div className="svc-dot off"></div>
                    <span className="svc-name">ato-registry</span>
                  </div>
                  <span className="svc-meta">idle</span>
                </div>
              </div>
              <button className="diag-copy" onClick={() => toast("Diagnostics copied to clipboard")}>
                <span className="iconify" data-icon="lucide:clipboard-copy" style={{ fontSize: "14px" }}></span>
                Copy Diagnostics for Bug Report
              </button>
              <div className="license">
                © 2025 ato.run · Open source under Apache 2.0 · Built with Rust + Wry
                <br />
                Trust-on-First-Use · Zero-Default-Trust · Capsule Protocol v2.1
              </div>
            </div>
          </>
        );

      default:
        return null;
    }
  };

  return (
    <div className="settings visible">
      <div className="ss">
        <div className="ss-search">
          <div className="ss-search-box">
            <span className="iconify" data-icon="lucide:search"></span>
            <input type="text" placeholder="Search…" id="ssSearch" />
          </div>
        </div>
        <div className="ss-sec">
          <div className="ss-sec-title">Settings</div>
          {["general", "account", "runtime", "sandbox", "trust"].map((id) => {
            const sec = settingsSections.find((s) => s.id === id);
            if (!sec) return null;
            const isActive = section.id === id;
            const badges: Record<string, string> = { runtime: "Core", trust: "3" };
            return (
              <button
                key={id}
                className={`ss-item ${isActive ? "active" : ""}`}
                data-tab={id}
                onClick={() => navigateHostPanel(`/settings/${id}`, setPath)}
              >
                <span className="iconify" data-icon={`lucide:${getSettingsIcon(id)}`}></span>
                {sec.label}
                {badges[id] && <span className="ss-badge">{badges[id]}</span>}
              </button>
            );
          })}
        </div>
        <div className="ss-sec">
          <div className="ss-sec-title">System</div>
          {["registry", "projection", "developer", "about"].map((id) => {
            const sec = settingsSections.find((s) => s.id === id);
            if (!sec) return null;
            const isActive = section.id === id;
            return (
              <button
                key={id}
                className={`ss-item ${isActive ? "active" : ""}`}
                data-tab={id}
                onClick={() => navigateHostPanel(`/settings/${id}`, setPath)}
              >
                <span className="iconify" data-icon={`lucide:${getSettingsIcon(id)}`}></span>
                {sec.label}
              </button>
            );
          })}
        </div>
      </div>

      <div className="s-content">
        <div id={`tab-${section.id}`} className="sp active">
          <div className="sp-title">{section.label}</div>
          {renderSettingContent()}
        </div>
      </div>
    </div>
  );
}

function renderCapsuleOverview(detail: CapsuleDetailPayload | null) {
  return (
    <div>
      {/* Status */}
      <div style={{ display: "flex", alignItems: "center", gap: "16px", marginBottom: "32px" }}>
        <span className="badge running" style={{ fontSize: "12px", padding: "4px 10px" }}>
          <div className="status-dot"></div> Running
        </span>
        <span style={{ color: "var(--text-muted)", fontSize: "13px" }}>Uptime: 2h 15m</span>
        <span style={{ color: "var(--text-ghost)" }}>|</span>
        <span style={{ color: "var(--text-muted)", fontSize: "13px" }}>Started: Today, 14:02</span>
      </div>

      {/* 4 Cards */}
      <div className="card-grid">
        <div className="card">
          <div className="card-label"><span className="iconify" data-icon="lucide:cpu"></span> Runtime</div>
          <div className="card-value">{detail?.runtimeLabel ?? "Python 3.12"}</div>
          <div className="card-sub">source-reference</div>
          <div style={{ marginTop: "12px" }}>
            <span className="badge neutral" style={{ color: "var(--amber)", borderColor: "var(--amber-dim)" }}>--unsafe mode</span>
          </div>
        </div>
        <div className="card">
          <div className="card-label"><span className="iconify" data-icon="lucide:activity"></span> Resources</div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline" }}>
            <div className="card-value">12%<span style={{ fontSize: "12px", color: "var(--text-muted)", fontWeight: "400" }}> CPU</span></div>
            <div className="card-sub">142 MB RAM</div>
          </div>
          <div className="spark-line">
            <div className="spark-bar" style={{ height: "30%" }}></div>
            <div className="spark-bar" style={{ height: "40%" }}></div>
            <div className="spark-bar" style={{ height: "35%" }}></div>
            <div className="spark-bar" style={{ height: "50%" }}></div>
            <div className="spark-bar" style={{ height: "60%" }}></div>
            <div className="spark-bar" style={{ height: "45%" }}></div>
            <div className="spark-bar" style={{ height: "70%" }}></div>
            <div className="spark-bar" style={{ height: "80%" }}></div>
            <div className="spark-bar" style={{ height: "65%" }}></div>
            <div className="spark-bar" style={{ height: "85%" }}></div>
          </div>
        </div>
        <div className="card">
          <div className="card-label"><span className="iconify" data-icon="lucide:globe"></span> Network</div>
          <div className="card-value">3</div>
          <div className="card-sub">Active connections</div>
          <div style={{ marginTop: "16px", fontSize: "12px", color: "var(--text-muted)", display: "flex", justifyContent: "space-between" }}>
            <span>Egress (1h)</span><span className="mono text-primary">1.2 MB</span>
          </div>
        </div>
        <div className="card">
          <div className="card-label"><span className="iconify" data-icon="lucide:hard-drive"></span> Storage</div>
          <div className="card-value">2.1 GB</div>
          <div className="card-sub">Across 4 mounts</div>
          <div style={{ marginTop: "16px", fontSize: "12px", color: "var(--text-muted)", display: "flex", justifyContent: "space-between" }}>
            <span>state.models</span><span className="mono text-primary">2.0 GB</span>
          </div>
        </div>
      </div>

      {/* Recent Activity */}
      <div className="section">
        <h2 className="section-title">Recent Activity</h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <tbody>
              <tr>
                <td style={{ width: "40px" }}><span className="iconify text-green" data-icon="lucide:play" style={{ fontSize: "16px" }}></span></td>
                <td>Capsule started successfully</td>
                <td style={{ textAlign: "right", color: "var(--text-ghost)" }}>2h 15m ago</td>
              </tr>
              <tr>
                <td><span className="iconify text-amber" data-icon="lucide:unlock" style={{ fontSize: "16px" }}></span></td>
                <td>Session granted <strong>Network Egress</strong> to <span className="mono">api.github.com</span></td>
                <td style={{ textAlign: "right", color: "var(--text-ghost)" }}>2h 16m ago</td>
              </tr>
              <tr>
                <td><span className="iconify text-accent" data-icon="lucide:arrow-up-circle" style={{ fontSize: "16px" }}></span></td>
                <td>Updated to version <strong>v1.4.0</strong></td>
                <td style={{ textAlign: "right", color: "var(--text-ghost)" }}>Yesterday</td>
              </tr>
              <tr>
                <td><span className="iconify" data-icon="lucide:square" style={{ fontSize: "16px", color: "var(--text-muted)" }}></span></td>
                <td>Capsule stopped manually</td>
                <td style={{ textAlign: "right", color: "var(--text-ghost)" }}>Yesterday</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

function renderCapsulePermissions(detail: CapsuleDetailPayload | null) {
  return (
    <div>
      {/* Network */}
      <div className="section">
        <h2 className="section-title">
          <span><span className="iconify" data-icon="lucide:globe" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>Network</span>
          <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
            <span style={{ fontSize: "12px", color: "var(--rose)", fontWeight: 500 }}>Block all egress</span>
            <div className="tog" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
          </div>
        </h2>
        <div className="data-table-wrap" style={{ marginBottom: "16px" }}>
          <table className="data-table">
            <thead><tr><th>Rule Type</th><th>Target</th><th>Status / Usage</th><th></th></tr></thead>
            <tbody>
              <tr>
                <td>Egress Allow (L7)</td>
                <td className="mono">api.github.com</td>
                <td><span className="badge neutral" style={{ color: "var(--text-muted)" }}>0 B · Inactive</span></td>
                <td style={{ textAlign: "right" }}><button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:minus-circle"></span></button></td>
              </tr>
              <tr>
                <td>Egress Allow (L7)</td>
                <td className="mono">huggingface.co</td>
                <td><span className="badge running">Active</span></td>
                <td style={{ textAlign: "right" }}><button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:minus-circle"></span></button></td>
              </tr>
            </tbody>
          </table>
        </div>
        <div className="data-table-wrap">
          <div style={{ padding: "12px 16px", borderBottom: "1px solid var(--border-default)", background: "var(--bg-surface)", fontSize: "12px", fontWeight: 600, color: "var(--text-secondary)" }}>Live Connections</div>
          <table className="data-table">
            <tbody>
              <tr>
                <td><span className="badge neutral">web</span></td>
                <td className="mono">github.com:443</td>
                <td className="mono" style={{ color: "var(--text-muted)" }}>1.2 MB</td>
                <td style={{ textAlign: "right", color: "var(--text-ghost)" }}>1m ago</td>
              </tr>
              <tr>
                <td><span className="badge neutral">worker</span></td>
                <td className="mono">hs.ato.run:443</td>
                <td><span className="badge tailnet"><span className="iconify" data-icon="lucide:network"></span> Tailnet</span></td>
                <td style={{ textAlign: "right", color: "var(--green)" }}>Live</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      {/* Filesystem */}
      <div className="section">
        <h2 className="section-title"><span className="iconify" data-icon="lucide:hard-drive" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>Filesystem</h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <thead><tr><th>Mount Type</th><th>Path</th><th>Size / Durability</th><th>Actions</th></tr></thead>
            <tbody>
              <tr>
                <td><span className="badge neutral">state.data</span></td>
                <td className="mono">~/.ato/data/libretranslate/...</td>
                <td>1.2 GB <span style={{ color: "var(--text-ghost)", marginLeft: "8px" }}>Persistent</span></td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm secondary">Reveal</button>
                  <button className="btn sm danger" style={{ marginLeft: "4px" }}>Wipe</button>
                </td>
              </tr>
              <tr>
                <td><span className="badge neutral">state.cache</span></td>
                <td className="mono">~/.ato/cache/libretranslate/...</td>
                <td>892 MB <span style={{ color: "var(--text-ghost)", marginLeft: "8px" }}>Ephemeral</span></td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm secondary">Reveal</button>
                  <button className="btn sm danger" style={{ marginLeft: "4px" }}>Wipe</button>
                </td>
              </tr>
              <tr>
                <td><span className="badge neutral" style={{ color: "var(--amber)", borderColor: "var(--amber-dim)" }}>Read-Only</span></td>
                <td className="mono">~/.config/ato/models</td>
                <td>-- <span style={{ color: "var(--text-ghost)", marginLeft: "8px" }}>Host Path</span></td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm secondary">Reveal</button>
                  <button className="btn sm ghost icon-only" style={{ marginLeft: "4px" }}><span className="iconify" data-icon="lucide:minus-circle"></span></button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      {/* Environment */}
      <div className="section">
        <h2 className="section-title"><span className="iconify" data-icon="lucide:terminal" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>Environment</h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <thead><tr><th>Variable</th><th>Value</th><th>Actions</th></tr></thead>
            <tbody>
              <tr>
                <td className="mono">HF_TOKEN</td>
                <td><span className="input-masked">••••••••••••••</span></td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm ghost icon-only" title="Reveal once"><span className="iconify" data-icon="lucide:eye"></span></button>
                  <button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:edit-2"></span></button>
                  <button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:minus-circle"></span></button>
                </td>
              </tr>
              <tr>
                <td className="mono">MODEL_DIR</td>
                <td className="mono" style={{ color: "var(--text-muted)" }}>/models</td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:edit-2"></span></button>
                  <button className="btn sm ghost icon-only"><span className="iconify" data-icon="lucide:minus-circle"></span></button>
                </td>
              </tr>
              <tr>
                <td colSpan={3} style={{ textAlign: "center", padding: "8px" }}>
                  <button className="btn ghost sm" style={{ width: "100%", border: "1px dashed var(--border-medium)" }}>+ Add allow_env</button>
                </td>
              </tr>
            </tbody>
          </table>
          <div style={{ padding: "10px 16px", background: "var(--bg-base)", borderTop: "1px solid var(--border-default)", textAlign: "center" }}>
            <button className="btn ghost sm" style={{ fontSize: "11px" }}>Show baseline environment (PATH, LANG, HOME...)</button>
          </div>
        </div>
      </div>

      {/* Roles & Capabilities */}
      <div className="section">
        <h2 className="section-title"><span className="iconify" data-icon="lucide:shield" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>Role & Capabilities</h2>
        <div className="data-table-wrap" style={{ marginBottom: "24px" }}>
          <table className="data-table">
            <thead><tr><th>Capability</th><th>Status</th><th>Source</th><th></th></tr></thead>
            <tbody>
              <tr>
                <td className="mono" style={{ color: "var(--text-primary)" }}>spawn-process</td>
                <td><span className="badge verified">Granted</span></td>
                <td><span className="badge neutral" style={{ color: "var(--text-muted)" }}>Manifest</span></td>
                <td style={{ textAlign: "right" }}><button className="btn sm danger">Revoke</button></td>
              </tr>
              <tr>
                <td className="mono" style={{ color: "var(--text-primary)" }}>read-file</td>
                <td><span className="badge verified">Granted</span></td>
                <td><span className="badge neutral" style={{ color: "var(--text-muted)" }}>User Session</span></td>
                <td style={{ textAlign: "right" }}><button className="btn sm danger">Revoke</button></td>
              </tr>
              <tr>
                <td className="mono" style={{ color: "var(--text-muted)" }}>write-file</td>
                <td><span className="badge untrusted">Denied</span></td>
                <td><span className="badge neutral" style={{ color: "var(--text-muted)" }}>Manifest</span></td>
                <td style={{ textAlign: "right" }}></td>
              </tr>
            </tbody>
          </table>
        </div>
        <button className="btn danger" style={{ width: "100%" }} onClick={() => toast("Resetting to manifest defaults...")}>
          <span className="iconify" data-icon="lucide:rotate-ccw"></span> Reset to manifest defaults
        </button>
      </div>
    </div>
  );
}

function renderCapsuleLogs(detail: CapsuleDetailPayload | null, capsuleSettings: any | null) {
  const logs: CapsuleDetailLogEntry[] = capsuleSettings?.activity ?? detail?.logs ?? [];
  const stages = Array.from(new Set(logs.map((entry) => entry.stage).filter(Boolean)));
  const errorCount = logs.filter((entry) => normalizeLogTone(entry.tone) === "err").length;

  return (
    <div className="log-container">
      {/* Filter Bar */}
      <div style={{ padding: "16px 24px", borderBottom: "1px solid var(--border-default)", background: "var(--bg-base)" }}>
        <div className="log-toolbar">
          <div style={{ position: "relative", flex: 1 }}>
            <span className="iconify" data-icon="lucide:search" style={{ position: "absolute", left: "12px", top: "10px", color: "var(--text-ghost)" }}></span>
            <input type="text" className="log-input" placeholder="Filter logs by text..." />
          </div>
          <select className="log-sel">
            <option>All Levels</option>
            <option>Error</option>
            <option>Warn</option>
            <option>Info</option>
          </select>
          <select className="log-sel">
            <option>Last 1 hour</option>
            <option>Last 24 hours</option>
            <option>All time</option>
          </select>
          <div style={{ width: "1px", background: "var(--border-medium)", margin: "0 4px" }}></div>
          <button className="btn secondary" onClick={(e) => e.currentTarget.classList.toggle("primary")}>
            <span className="iconify" data-icon="lucide:arrow-down-to-line"></span> Follow
          </button>
          <button className="btn secondary icon-only" title="Export"><span className="iconify" data-icon="lucide:download"></span></button>
          <button className="btn secondary icon-only" title="Clear"><span className="iconify" data-icon="lucide:trash-2"></span></button>
        </div>
        <div className="log-pills">
          <div className="log-pill active">
            All stages {errorCount > 0 && <span style={{ background: "var(--rose)", color: "#fff", padding: "2px 6px", borderRadius: "10px", fontSize: "10px", fontWeight: 700 }}>{errorCount}</span>}
          </div>
          {stages.map((stage) => (
            <div className="log-pill" key={stage}>{stage}</div>
          ))}
        </div>
      </div>
    
      {/* Log Stream */}
      <div className="log-viewer scrollable">
        {logs.length === 0 ? (
          <div className="log-empty">No capsule logs recorded yet.</div>
        ) : (
          logs.map((entry, index) => {
            const tone = normalizeLogTone(entry.tone);
            return (
              <div className="log-row" key={`${entry.stage}-${index}-${entry.message}`}>
                <span className="lr-time">{String(index + 1).padStart(4, "0")}</span>
                <span className={`lr-svc ${entry.stage}`}>{entry.stage || "runtime"}</span>
                <span className={`lr-lvl ${tone}`}>{logLevelLabel(tone)}</span>
                <span className="lr-msg">{entry.message}</span>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function normalizeLogTone(tone: string | undefined): "info" | "warn" | "err" {
  const normalized = (tone ?? "").toLowerCase();
  if (normalized === "error" || normalized === "err") return "err";
  if (normalized === "warning" || normalized === "warn") return "warn";
  return "info";
}

function logLevelLabel(tone: "info" | "warn" | "err") {
  if (tone === "err") return "ERR";
  if (tone === "warn") return "WARN";
  return "INFO";
}

function renderCapsuleUpdate(detail: CapsuleDetailPayload | null) {
  return (
    <div>
      {/* Version Cards */}
      <div className="card-grid" style={{ gridTemplateColumns: "1fr 1fr" }}>
        <div className="card" style={{ borderColor: "var(--border-medium)", background: "var(--bg-base)" }}>
          <div className="card-label">Current Version</div>
          <div className="card-value">{detail?.versionLabel ?? "v1.4.0"}</div>
          <div className="card-sub" style={{ marginTop: "8px" }}>Signed by <span style={{ color: "var(--text-primary)" }}>argosopentech</span></div>
          <div className="card-sub">Installed on Apr 28, 2026</div>
        </div>
        <div className="card" style={{ borderColor: "var(--accent)", background: "var(--accent-dim)" }}>
          <div className="card-label" style={{ color: "var(--accent)" }}>Available Version</div>
          <div className="card-value text-primary">v1.4.1</div>
          <div className="card-sub" style={{ marginTop: "8px" }}>Published 2 hours ago</div>
          <div style={{ marginTop: "12px", display: "flex", gap: "8px" }}>
            <button className="btn primary sm" onClick={() => toast("Downloading update...")}>Update Now</button>
            <button className="btn secondary sm">Pin to v1.4.0</button>
          </div>
        </div>
      </div>

      {/* Update Settings */}
      <div className="section">
        <h2 className="section-title">Update Settings</h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <tbody>
              <tr>
                <td>Update Channel</td>
                <td style={{ textAlign: "right" }}>
                  <select style={{ background: "var(--bg-input)", border: "1px solid var(--border-medium)", color: "var(--text-secondary)", borderRadius: "4px", padding: "4px 8px", fontSize: "13px", outline: "none" }}>
                    <option>Stable</option>
                    <option>Beta</option>
                    <option>Nightly</option>
                  </select>
                </td>
              </tr>
              <tr>
                <td>Auto-update this capsule</td>
                <td style={{ textAlign: "right" }}>
                  <div className="tog on" onClick={(e) => e.currentTarget.classList.toggle("on")}></div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      {/* Changelog */}
      <div className="section">
        <h2 className="section-title">Changelog (v1.4.1)</h2>
        <div style={{ background: "var(--bg-card)", border: "1px solid var(--border-default)", borderRadius: "var(--radius-lg)", padding: "20px", fontSize: "13px", lineHeight: "1.6", color: "var(--text-body)" }}>
          <h3 style={{ color: "var(--text-primary)", marginBottom: "12px", fontSize: "16px" }}>Fixes and Improvements</h3>
          <ul style={{ marginLeft: "20px", color: "var(--text-muted)" }}>
            <li>Fixed a memory leak in the translation worker process.</li>
            <li>Updated base Python image to address CVE-2026-1234.</li>
            <li>Added support for direct Tailnet connections.</li>
          </ul>
        </div>
      </div>

      {/* Install History */}
      <div className="section">
        <h2 className="section-title">Install History</h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <thead><tr><th>Version</th><th>Date</th><th>Source</th><th></th></tr></thead>
            <tbody>
              <tr>
                <td className="mono text-primary">v1.4.0</td>
                <td>Apr 28, 2026</td>
                <td className="mono text-muted">registry.ato.run</td>
                <td style={{ textAlign: "right" }}><span className="badge neutral">Current</span></td>
              </tr>
              <tr>
                <td className="mono text-muted">v1.3.8</td>
                <td>Mar 15, 2026</td>
                <td className="mono text-muted">registry.ato.run</td>
                <td style={{ textAlign: "right" }}><button className="btn sm secondary">Rollback</button></td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
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
    <div>
      {/* Inbound */}
      <div className="section">
        <h2 className="section-title">
          <span className="iconify" data-icon="lucide:arrow-down-to-line" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>
          Inbound (Connect to this capsule)
        </h2>
        
        <div className="card" style={{ marginBottom: "16px", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <div>
            <div className="card-label">Public URL (Localhost)</div>
            <div className="mono text-primary" style={{ fontSize: "14px" }}>{detail?.localUrl ?? "http://127.0.0.1:5000"}</div>
          </div>
          <button className="btn secondary icon-only"><span className="iconify" data-icon="lucide:copy"></span></button>
        </div>
        <div className="card" style={{ marginBottom: "24px", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <div>
            <div className="card-label">Tailnet URL (Zero Trust)</div>
            <div className="mono text-cyan" style={{ fontSize: "14px" }}>{detail?.quickOpenUrl ?? "https://libretranslate.mytailnet.ts.net"}</div>
          </div>
          <button className="btn secondary icon-only"><span className="iconify" data-icon="lucide:copy"></span></button>
        </div>

        <div className="data-table-wrap" style={{ marginBottom: "16px" }}>
          <div style={{ padding: "12px 16px", borderBottom: "1px solid var(--border-default)", background: "var(--bg-surface)", fontSize: "12px", fontWeight: 600, color: "var(--text-secondary)", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span>API Keys</span>
            <button className="btn sm primary">Generate Key</button>
          </div>
          <table className="data-table">
            <tbody>
              <tr>
                <td>Raycast Extension</td>
                <td className="mono text-ghost">ato_sk_••••••••</td>
                <td style={{ textAlign: "right" }}><button className="btn sm danger">Revoke</button></td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      {/* Outbound */}
      <div className="section">
        <h2 className="section-title">
          <span className="iconify" data-icon="lucide:arrow-up-from-line" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>
          Outbound (External Services)
        </h2>
        
        <div className="data-table-wrap">
          <div style={{ padding: "12px 16px", borderBottom: "1px solid var(--border-default)", background: "var(--bg-surface)", fontSize: "12px", fontWeight: 600, color: "var(--text-secondary)" }}>External API Credentials</div>
          <table className="data-table">
            <thead><tr><th>Purpose / Alias</th><th>Injection Method</th><th>Value</th><th></th></tr></thead>
            <tbody>
              <tr>
                <td className="text-primary font-medium">HuggingFace Hub</td>
                <td><span className="badge neutral">Environment Var</span> <span className="mono text-muted" style={{ marginLeft: "4px" }}>HF_TOKEN</span></td>
                <td><span className="input-masked">••••••••••••</span></td>
                <td style={{ textAlign: "right" }}>
                  <button className="btn sm secondary">Rotate</button>
                  <button className="btn sm danger" style={{ marginLeft: "4px" }}>Revoke</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      {/* IPC */}
      <div className="section">
        <h2 className="section-title">
          <span className="iconify" data-icon="lucide:cpu" style={{ marginRight: "8px", verticalAlign: "-2px" }}></span>
          IPC (Inter-Process Communication)
        </h2>
        <div className="data-table-wrap">
          <table className="data-table">
            <thead><tr><th>Session ID</th><th>Connected From</th><th>Status</th><th></th></tr></thead>
            <tbody>
              <tr>
                <td className="mono text-muted">sess_9f8a2b</td>
                <td className="text-primary font-medium">ato-cli-broker</td>
                <td><span className="badge running">Active</span></td>
                <td style={{ textAlign: "right" }}><button className="btn sm danger">Disconnect</button></td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

function renderCapsuleDetail(
  paneId: string,
  tab: CapsuleDetailTab,
  path: string,
  setPath: (path: string) => void,
  detail: CapsuleDetailPayload | null,
  fullPayload?: HostPanelPayload | null,
) {
  const title = detail?.title ?? `Pane ${paneId}`;
  const rawPayload = fullPayload ?? readHostPanelPayload();
  const iconSource: string | null =
    detail?.iconSource ??
    (rawPayload as any)?.capsuleSettings?.identity?.iconSource ??
    null;

  return (
    <div className="capsule-detail-screen">
      {/* Header */}
      <div className="header">
        <div className="header-drag"></div>
        <div className="header-content">
          <div className="identity">
            <div className="id-icon">
                {iconSource
                  ? <>
                      <img src={iconSource} className="id-icon-img" alt="" onError={(e) => {
                        (e.target as HTMLImageElement).style.display = "none";
                        const fb = (e.target as HTMLImageElement).nextElementSibling as HTMLElement | null;
                        if (fb) fb.removeAttribute("hidden");
                      }} />
                      <span className="iconify" data-icon="lucide:package" hidden></span>
                    </>
                  : <span className="iconify" data-icon="lucide:package"></span>}
              </div>
            <div className="id-info">
              <div className="id-title-row">
                <h1 className="id-title">{title}</h1>
                {/* #42: trust badge tone derived from real host data.
                    `restricted` capsules render as red; `Trusted` (host-confirmed)
                    renders as green; everything else (untrusted / unknown) renders
                    as neutral. The historical hardcoded "Verified" badge is gone —
                    we never had verification for v0.5.0. */}
                {detail?.restricted ? (
                  <span className="badge danger" title="Restricted by host trust policy">
                    <span className="iconify" data-icon="lucide:shield-alert"></span> Restricted
                  </span>
                ) : detail?.trustLabel === "Trusted" ? (
                  <span className="badge verified" title="Trust state confirmed by host">
                    <span className="iconify" data-icon="lucide:shield-check"></span> Trusted
                  </span>
                ) : detail?.trustLabel ? (
                  <span className="badge neutral" title="Trust state reported by host">
                    <span className="iconify" data-icon="lucide:shield"></span> {detail.trustLabel}
                  </span>
                ) : null}
              </div>
              <div className="id-meta">
                {/* #42: drop the v1.4.0 fallback — the host sends the real
                    string (which may already be "unversioned"). */}
                <span>{detail?.versionLabel ?? "—"}</span>
                <span>•</span>
                {/* #42: publisher / handle uses real canonicalHandle (or
                    handle) — never the hardcoded @kyotori. */}
                <span style={{ color: "var(--text-muted)" }}>
                  {detail?.canonicalHandle ?? detail?.handle ?? "—"}
                </span>
                {/* #42: removed the hardcoded "Tier 2" pill. Real runtime
                    tier is not surfaced for v0.5.0; v0.6.0 wires it to the
                    host snapshot. */}
              </div>
            </div>
          </div>
          {/* #43: header action buttons.
              - "Open Browser" is wired when the host actually exposes a
                local URL; otherwise the button is disabled with a tooltip
                that explains why (instead of toasting a fake URL).
              - "Restart" / "Stop" need a host IPC channel that does not
                exist in v0.5.0. Disabled with "Coming in v0.6.0" so the
                UI does not pretend the capsule was restarted/stopped when
                in fact nothing happened. v0.6.0 wires them through the
                same IPC track as the settings tabs (#49). */}
          <div className="global-actions">
            <button
              className="btn ghost icon-only"
              title={detail?.localUrl ? `Open ${detail.localUrl} in your default browser` : "No local URL exposed by this capsule"}
              disabled={!detail?.localUrl}
              onClick={() => {
                if (detail?.localUrl) {
                  window.open(detail.localUrl, "_blank", "noopener,noreferrer");
                }
              }}
            >
              <span className="iconify" data-icon="lucide:external-link"></span>
            </button>
            <button
              className="btn"
              title="Restart — coming in v0.6.0 (host IPC not yet wired)"
              disabled
            >
              <span className="iconify" data-icon="lucide:rotate-cw"></span>
            </button>
            <button
              className="btn danger"
              title="Stop — coming in v0.6.0 (host IPC not yet wired)"
              disabled
            >
              <span className="iconify" data-icon="lucide:square"></span> Stop
            </button>
          </div>
        </div>

        {/* Navigation */}
        <div className="nav">
          {capsuleDetailTabs.map((item) => {
            const active = item.id === tab.id;
            return (
              <button
                key={item.id}
                className={`nav-item ${active ? "active" : ""}`}
                onClick={(event) => {
                  event.preventDefault();
                  navigateHostPanel(`/capsule/${paneId}/${item.id}`, setPath);
                }}
              >
                {item.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* Content */}
      <div className="content">
        {/* 1. Overview */}
        <div id="tab-overview" className={`tab-pane scrollable ${tab.id === "overview" ? "active" : ""}`}>
          {tab.id === "overview" && renderCapsuleOverview(detail)}
        </div>

        {/* 2. Permissions */}
        <div id="tab-permissions" className={`tab-pane scrollable ${tab.id === "permissions" ? "active" : ""}`}>
          {tab.id === "permissions" && renderCapsulePermissions(detail)}
        </div>

        {/* 3. Logs */}
        <div id="tab-logs" className={`tab-pane ${tab.id === "logs" ? "active" : ""}`} style={{ padding: 0 }}>
          {tab.id === "logs" && renderCapsuleLogs(detail, rawPayload)}
        </div>

        {/* 4. Update */}
        <div id="tab-update" className={`tab-pane scrollable ${tab.id === "update" ? "active" : ""}`}>
          {tab.id === "update" && renderCapsuleUpdate(detail)}
        </div>

        {/* 5. API */}
        <div id="tab-api" className={`tab-pane scrollable ${tab.id === "api" ? "active" : ""}`}>
          {tab.id === "api" && renderCapsuleApi(detail)}
        </div>
      </div>
    </div>
  );
}

export default App;
