import { useEffect, useMemo, useRef, useState } from "react";
import {
  Box,
  ChevronDown,
  ChevronLeft,
  Shield,
  FileText,
  Globe,
  Hash,
  Package,
  Play,
  ExternalLink,
  RotateCcw,
  Settings2,
  Square,
  Terminal,
  Trash2,
  Plus,
  Zap,
} from "lucide-react";
import {
  getPermissionModeMessage,
  PermissionModeSelector,
} from "../components/PermissionModeSelector";
import { ReadmeRenderer } from "../components/ReadmeRenderer";
import { getProcessStatusMeta } from "../types";
import type {
  Capsule,
  CapsuleRelease,
  Process,
  ProcessLogLine,
  RunPermissionMode,
} from "../types";

interface DetailPageProps {
  capsule: Capsule;
  isMobile: boolean;
  process: Process | undefined;
  envValues: Record<string, string>;
  baseEnvKeys: string[];
  requiredEnvKeys: string[];
  storeMetadataIconPath: string;
  storeMetadataText: string;
  logs: ProcessLogLine[];
  selectedTarget: string;
  selectedPort: string;
  selectedPermissionMode: RunPermissionMode;
  canRun: boolean;
  openReady: boolean;
  hasRuntimeConfigChanges: boolean;
  isSavingRuntimeConfig: boolean;
  onBack: () => void;
  onRun: (capsule: Capsule) => void;
  onStop: (capsule: Capsule) => void;
  onOpen: (capsule: Capsule, process?: Process) => void;
  onDelete: (capsule: Capsule) => void;
  onRollbackRelease: (capsule: Capsule, release: CapsuleRelease) => void;
  onYankRelease: (capsule: Capsule, release: CapsuleRelease) => void;
  onSaveStoreMetadata: (capsule: Capsule, iconPath: string, text: string) => Promise<void>;
  onClearLogs: () => void;
  onEnvChange: (capsuleId: string, target: string, key: string, value: string) => void;
  onEnvAdd: (capsuleId: string, target: string, key: string, value: string) => void;
  onEnvRemove: (capsuleId: string, target: string, key: string) => void;
  onTargetChange: (capsuleId: string, target: string) => void;
  onPortChange: (capsuleId: string, target: string, value: string) => void;
  onPermissionModeChange: (
    capsuleId: string,
    target: string,
    value: RunPermissionMode,
  ) => void;
  onSaveRuntimeConfig: (capsule: Capsule) => void;
}

type DetailTab = "logs" | "docs" | "config" | "releases";

function logTextClass(level: string): string {
  const normalized = level.toLowerCase();
  if (normalized === "info") {
    return "terminal-row-text info";
  }
  if (normalized === "warn") {
    return "terminal-row-text warn";
  }
  if (normalized === "error" || normalized === "sigterm") {
    return "terminal-row-text error";
  }
  return "terminal-row-text";
}

function IconForCapsule({ iconKey }: { iconKey: Capsule["iconKey"] }): JSX.Element {
  const common = { size: 18, strokeWidth: 1.5 };
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

function requiresPermissionGrant(capsule: Capsule, targetLabel: string): boolean {
  const target = capsule.targets.find((entry) => entry.label === targetLabel);
  if (!target) {
    return false;
  }
  const runtime = target.runtime.trim().toLowerCase();
  const driver = target.driver.trim().toLowerCase();
  return (
    (runtime === "source" && (driver === "python" || driver === "native")) ||
    (runtime === "web" && driver === "python")
  );
}

function signatureTone(signatureStatus: string): string {
  const normalized = signatureStatus.trim().toLowerCase();
  if (normalized === "verified" || normalized === "signed") {
    return "ready";
  }
  if (normalized.includes("warn") || normalized.includes("pending")) {
    return "starting";
  }
  if (normalized.includes("invalid") || normalized.includes("fail") || normalized.includes("error")) {
    return "failed";
  }
  return "unknown";
}

export function DetailPage({
  capsule,
  isMobile,
  process,
  envValues,
  baseEnvKeys,
  requiredEnvKeys,
  storeMetadataIconPath,
  storeMetadataText,
  logs,
  selectedTarget,
  selectedPort,
  selectedPermissionMode,
  canRun,
  openReady,
  hasRuntimeConfigChanges,
  isSavingRuntimeConfig,
  onBack,
  onRun,
  onStop,
  onOpen,
  onDelete,
  onRollbackRelease,
  onYankRelease,
  onSaveStoreMetadata,
  onClearLogs,
  onEnvChange,
  onEnvAdd,
  onEnvRemove,
  onTargetChange,
  onPortChange,
  onPermissionModeChange,
  onSaveRuntimeConfig,
}: DetailPageProps): JSX.Element {
  const [tab, setTab] = useState<DetailTab>("docs");
  const [runtimeConfigOpen, setRuntimeConfigOpen] = useState(true);
  const logScrollRef = useRef<HTMLDivElement | null>(null);
  const status = getProcessStatusMeta(process?.status ?? "stopped");
  const isRunning = Boolean(process?.active);
  const selectedTargetMeta = useMemo(
    () => capsule.targets.find((target) => target.label === selectedTarget),
    [capsule.targets, selectedTarget],
  );
  const targetNeedsPermissionGrant = requiresPermissionGrant(
    capsule,
    selectedTarget,
  );
  const selectedPermissionMessage = getPermissionModeMessage(selectedPermissionMode);

  useEffect(() => {
    if (tab !== "logs" || !logScrollRef.current) {
      return;
    }
    const node = logScrollRef.current;
    node.scrollTop = node.scrollHeight;
  }, [logs, tab]);

  const envRows = useMemo(() => {
    const required = new Set(requiredEnvKeys);
    const base = new Set(baseEnvKeys);
    const keys = new Set<string>(requiredEnvKeys);
    Object.keys(envValues).forEach((key) => keys.add(key));
    return Array.from(keys).map((key) => ({
      key,
      value: envValues[key] ?? "",
      required: required.has(key),
      base: base.has(key),
    }));
  }, [baseEnvKeys, envValues, requiredEnvKeys]);
  const [draftEnvKey, setDraftEnvKey] = useState("");
  const [draftEnvValue, setDraftEnvValue] = useState("");
  const [metadataIconPathInput, setMetadataIconPathInput] = useState(storeMetadataIconPath);
  const [metadataTextInput, setMetadataTextInput] = useState(storeMetadataText);
  const [metadataSaving, setMetadataSaving] = useState(false);
  const [metadataError, setMetadataError] = useState("");

  useEffect(() => {
    setDraftEnvKey("");
    setDraftEnvValue("");
  }, [capsule.id, selectedTarget]);

  useEffect(() => {
    setMetadataIconPathInput(storeMetadataIconPath);
    setMetadataTextInput(storeMetadataText);
    setMetadataError("");
  }, [capsule.id, storeMetadataIconPath, storeMetadataText]);

  useEffect(() => {
    setRuntimeConfigOpen(true);
  }, [capsule.id]);

  const toggleRuntimeConfigCard = (): void => {
    if (!isMobile) {
      return;
    }
    setRuntimeConfigOpen((prev) => !prev);
  };

  return (
    <div className="detail-page">
      <header className="detail-header">
        <button className="icon-btn" type="button" onClick={onBack} aria-label="Back to catalog">
          <ChevronLeft size={15} strokeWidth={1.5} />
        </button>
        <div className="detail-icon">
          {capsule.storeMetadata?.iconUrl ? (
            <img
              src={capsule.storeMetadata.iconUrl}
              alt={`${capsule.scopedId} icon`}
              className="detail-icon-image"
            />
          ) : (
            <IconForCapsule iconKey={capsule.iconKey} />
          )}
        </div>
        <div>
          <div className="detail-title-row">
            <span className="detail-title">{capsule.scopedId}</span>
            <span className={`status-pill status-${status.tone}`}>
              <span className={`status-pill-dot ${status.active ? "active" : ""}`} />
              {status.label}
            </span>
          </div>
          <div className="detail-meta">
            <span className="detail-meta-item">
              <Hash size={11} strokeWidth={1.5} />
              {capsule.version}
            </span>
            <span className="detail-meta-sep">·</span>
            <span className="detail-meta-item">{capsule.publisher}</span>
            <span className="detail-meta-sep">·</span>
            <span className="detail-meta-item">{capsule.size}</span>
          </div>
        </div>
        <div className="detail-actions">
          <button className="btn btn-danger" type="button" onClick={() => onDelete(capsule)} disabled={isRunning}>
            <Trash2 size={12} strokeWidth={2} /> Delete
          </button>
          {isRunning ? (
            <button className="btn btn-danger" type="button" onClick={() => onStop(capsule)}>
              <Square size={12} strokeWidth={2} /> Stop
            </button>
          ) : null}
          {isRunning ? (
            <button
              className="btn btn-ghost"
              type="button"
              onClick={() => onOpen(capsule, process)}
              disabled={!openReady}
            >
              <ExternalLink size={12} strokeWidth={2} /> Open
            </button>
          ) : null}
          <button className="btn btn-success" type="button" onClick={() => onRun(capsule)} disabled={!canRun}>
            <Play size={12} strokeWidth={2} /> {isRunning ? "Spawn another" : "Run"}
          </button>
        </div>
      </header>

      <div className="tabs" role="tablist" aria-label="Detail tabs">
        <button
          className={`tab ${tab === "docs" ? "active" : ""}`}
          type="button"
          onClick={() => setTab("docs")}
        >
          <FileText size={13} strokeWidth={1.5} /> Readme
        </button>
        <button
          className={`tab ${tab === "config" ? "active" : ""}`}
          type="button"
          onClick={() => setTab("config")}
        >
          <Settings2 size={13} strokeWidth={1.5} /> Configuration
        </button>
        <button
          className={`tab ${tab === "releases" ? "active" : ""}`}
          type="button"
          onClick={() => setTab("releases")}
        >
          <Shield size={13} strokeWidth={1.5} /> Releases &amp; Security
        </button>
        <button
          className={`tab ${tab === "logs" ? "active" : ""}`}
          type="button"
          onClick={() => setTab("logs")}
        >
          <Terminal size={13} strokeWidth={1.5} /> Logs
        </button>
      </div>

      {tab === "logs" ? (
        <div className="terminal" role="tabpanel" aria-label="Log output">
          <div className="terminal-bar">
            <span className="terminal-bar-title">stdout · {capsule.scopedId}</span>
            <button className="btn btn-ghost terminal-clear" type="button" onClick={onClearLogs}>
              <RotateCcw size={11} strokeWidth={1.5} /> Clear
            </button>
          </div>
          <div ref={logScrollRef} className="terminal-body" aria-live="polite">
            {logs.length === 0 ? (
              <div className="term-empty">— no output yet —</div>
            ) : (
              logs.map((line) => (
                <div key={`${line.index}-${line.timestamp}`} className="terminal-row">
                  <span className="terminal-row-number">{line.index}</span>
                  <span className={logTextClass(line.level)}>
                    [{line.timestamp}] {line.level} {line.message}
                  </span>
                </div>
              ))
            )}
          </div>
        </div>
      ) : null}

      {tab === "docs" ? (
        <div className="docs-pane" role="tabpanel" aria-label="Readme">
          <div className="docs-card">
            <ReadmeRenderer readme={capsule.readme} />
          </div>
        </div>
      ) : null}

      {tab === "config" ? (
        <div
          className="config-pane config-pane-runtime-only"
          role="tabpanel"
          aria-label="Configuration"
        >
          <section
            className={`config-section config-section-scrollable ${
              isMobile && !runtimeConfigOpen ? "collapsed" : ""
            }`}
          >
            <button
              className="config-section-header config-section-toggle"
              type="button"
              aria-expanded={!isMobile || runtimeConfigOpen}
              aria-controls="config-card-runtime"
              onClick={toggleRuntimeConfigCard}
            >
              <span className="config-section-title">
                <Settings2 size={13} strokeWidth={1.5} /> Runtime Configuration
              </span>
              {isMobile ? (
                <ChevronDown
                  className={`config-section-chevron ${runtimeConfigOpen ? "open" : ""}`}
                  size={14}
                  strokeWidth={1.5}
                />
              ) : null}
            </button>
            {!isMobile || runtimeConfigOpen ? (
              <div className="env-body" id="config-card-runtime">
                <label className="env-row">
                  <span className="env-key">TARGET</span>
                  <select
                    className="input env-input"
                    value={selectedTarget}
                    onChange={(event) => onTargetChange(capsule.id, event.target.value)}
                  >
                    {capsule.targets.map((target) => (
                      <option key={target.label} value={target.label}>
                        {target.label} ({target.runtime}
                        {target.driver ? `/${target.driver}` : ""})
                      </option>
                    ))}
                  </select>
                </label>

                <label className="env-row">
                  <span className="env-key">PORT</span>
                  <input
                    className="input env-input"
                    value={selectedPort}
                    inputMode="numeric"
                    pattern="[0-9]*"
                    onChange={(event) =>
                      onPortChange(capsule.id, selectedTarget, event.target.value)
                    }
                  />
                </label>

                {targetNeedsPermissionGrant ? (
                  <>
                    <div className="env-row env-row-multiline">
                      <span className="env-key">PERMISSIONS</span>
                      <PermissionModeSelector
                        name={`detail-permission-mode-${capsule.id}`}
                        value={selectedPermissionMode}
                        onChange={(value) =>
                          onPermissionModeChange(capsule.id, selectedTarget, value)
                        }
                      />
                    </div>
                    <p
                      className={`env-help ${
                        selectedPermissionMessage.tone === "warn"
                          ? "env-help-warn"
                          : selectedPermissionMessage.tone === "error"
                            ? "env-help-error"
                            : ""
                      }`}
                    >
                      {selectedPermissionMessage.text}
                    </p>
                  </>
                ) : (
                  <p className="env-help">
                    {selectedTargetMeta
                      ? `This target runs as ${selectedTargetMeta.runtime}${
                          selectedTargetMeta.driver
                            ? `/${selectedTargetMeta.driver}`
                            : ""
                        } and does not need extra permission flags.`
                      : "This target does not need extra permission flags."}
                  </p>
                )}

                <p className="env-help">
                  Overrides applied to the next spawned instance.
                </p>
                {envRows.map((row) => (
                  <div key={row.key} className="env-row env-row-kv">
                    <input
                      className="input env-key-input"
                      value={row.key}
                      disabled
                      aria-label={`Environment key ${row.key}`}
                    />
                    <input
                      className="input env-input"
                      value={row.value}
                      onChange={(event) =>
                        onEnvChange(
                          capsule.id,
                          selectedTarget,
                          row.key,
                          event.target.value,
                        )
                      }
                    />
                    <button
                      className="icon-btn env-remove-btn"
                      type="button"
                      aria-label={`Remove ${row.key}`}
                      disabled={row.required || row.base}
                      onClick={() =>
                        onEnvRemove(capsule.id, selectedTarget, row.key)
                      }
                    >
                      <Trash2 size={13} strokeWidth={1.5} />
                    </button>
                  </div>
                ))}
                <div className="env-row env-row-kv env-row-add">
                  <input
                    className="input env-key-input"
                    value={draftEnvKey}
                    placeholder="KEY"
                    onChange={(event) => setDraftEnvKey(event.target.value)}
                  />
                  <input
                    className="input env-input"
                    value={draftEnvValue}
                    placeholder="value"
                    onChange={(event) => setDraftEnvValue(event.target.value)}
                  />
                  <button
                    className="icon-btn"
                    type="button"
                    aria-label="Add environment variable"
                    disabled={draftEnvKey.trim().length === 0}
                    onClick={() => {
                      const key = draftEnvKey.trim();
                      if (!key) {
                        return;
                      }
                      onEnvAdd(capsule.id, selectedTarget, key, draftEnvValue);
                      setDraftEnvKey("");
                      setDraftEnvValue("");
                    }}
                  >
                    <Plus size={13} strokeWidth={1.5} />
                  </button>
                </div>

                <div className="env-divider" />
                <button
                  className="btn btn-primary"
                  type="button"
                  disabled={!hasRuntimeConfigChanges || isSavingRuntimeConfig}
                  onClick={() => onSaveRuntimeConfig(capsule)}
                >
                  {isSavingRuntimeConfig ? "Saving..." : "Save configuration"}
                </button>

                <div className="env-divider" />
                <label className="env-row">
                  <span className="env-key">ICON_FILE_PATH</span>
                  <input
                    className="input env-input"
                    value={metadataIconPathInput}
                    placeholder="~/icons/sample.png"
                    onChange={(event) => setMetadataIconPathInput(event.target.value)}
                  />
                </label>
                <label className="env-row env-row-multiline">
                  <span className="env-key">TEXT</span>
                  <textarea
                    className="input env-input env-textarea"
                    value={metadataTextInput}
                    placeholder="Store listing description"
                    onChange={(event) => setMetadataTextInput(event.target.value)}
                  />
                </label>
                {metadataError ? <p className="env-error">{metadataError}</p> : null}
                <button
                  className="btn btn-primary"
                  type="button"
                  disabled={metadataSaving}
                  onClick={() => {
                    setMetadataSaving(true);
                    setMetadataError("");
                    void onSaveStoreMetadata(
                      capsule,
                      metadataIconPathInput,
                      metadataTextInput,
                    )
                      .catch((error: unknown) => {
                        const message =
                          error instanceof Error
                            ? error.message
                            : "metadata update failed";
                        setMetadataError(message);
                      })
                      .finally(() => setMetadataSaving(false));
                  }}
                >
                  {metadataSaving ? "Saving..." : "Save store.metadata"}
                </button>
              </div>
            ) : null}
          </section>
        </div>
      ) : null}

      {tab === "releases" ? (
        <div className="docs-pane" role="tabpanel" aria-label="Releases and Security">
          <div className="docs-card releases-card">
            <div className="releases-summary-grid">
              <div className="release-summary-tile">
                <span className="section-title">Latest</span>
                <strong>{capsule.version}</strong>
              </div>
              <div className="release-summary-tile">
                <span className="section-title">Tracked releases</span>
                <strong>{capsule.releases.length}</strong>
              </div>
              <div className="release-summary-tile">
                <span className="section-title">Trust</span>
                <strong>{capsule.trustLevel === "verified" || capsule.trustLevel === "signed" ? "Verified" : "Unverified"}</strong>
              </div>
            </div>

            {capsule.releases.length === 0 ? (
              <div className="release-empty">No release history is available for this capsule yet.</div>
            ) : (
              <div className="release-table-wrap">
                <table className="release-table">
                  <thead>
                    <tr>
                      <th>Version</th>
                      <th>Content Hash</th>
                      <th>Signature</th>
                      <th style={{ textAlign: "right" }}>Operations</th>
                    </tr>
                  </thead>
                  <tbody>
                    {capsule.releases.map((release) => {
                      const tone = signatureTone(release.signatureStatus);
                      const actionable = Boolean(release.manifestHash);
                      return (
                        <tr key={`${release.version}-${release.contentHash}`}>
                          <td className="mono">
                            <div className="release-version-cell">
                              <span>{release.version}</span>
                              {release.isCurrent ? <span className="badge badge-current">Current</span> : null}
                              {release.yankedAt ? <span className="badge badge-yanked">Yanked</span> : null}
                            </div>
                            {release.yankedAt ? <div className="row-meta">yanked at {new Date(release.yankedAt).toLocaleString()}</div> : null}
                          </td>
                          <td className="mono release-hash-cell">{release.contentHash}</td>
                          <td>
                            <span className={`badge status-badge status-${tone}`}>{release.signatureStatus}</span>
                          </td>
                          <td>
                            <div className="release-actions">
                              <button
                                className="btn btn-ghost"
                                type="button"
                                disabled={!actionable}
                                onClick={() => onRollbackRelease(capsule, release)}
                              >
                                Rollback
                              </button>
                              <button
                                className="btn btn-danger"
                                type="button"
                                disabled={!actionable || Boolean(release.yankedAt)}
                                onClick={() => onYankRelease(capsule, release)}
                              >
                                Yank
                              </button>
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </div>
      ) : null}
    </div>
  );
}
