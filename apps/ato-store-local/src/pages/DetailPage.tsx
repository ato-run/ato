import { useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronLeft,
  FileText,
  Play,
  ExternalLink,
  RotateCcw,
  Settings2,
  Shield,
  Square,
  Terminal,
  Trash2,
  Plus,
} from "lucide-react";
import {
  DockCapsuleDetailSummary,
  DockReadmePanel,
  DockReleaseTable,
} from "@ato/dock-react";
import { normalizeLocalDetail } from "@ato/dock-domain";
import {
  getPermissionModeMessage,
  PermissionModeSelector,
} from "../components/PermissionModeSelector";
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
  const [publisher, slug] = capsule.scopedId.split("/", 2);
  const sharedDetail = useMemo(
    () =>
      normalizeLocalDetail(
        {
          id: capsule.id,
          description: capsule.longDescription || capsule.description,
          latestVersion: capsule.version,
          readmeMarkdown: capsule.readme,
          readmeSource: capsule.readmeSource ?? null,
          releases: capsule.releases.map((release) => ({
            version: release.version,
            manifestHash: release.manifestHash,
            contentHash: release.contentHash,
            signatureStatus: release.signatureStatus,
            isCurrent: release.isCurrent,
            yankedAt: release.yankedAt,
          })),
          storeMetadata: {
            iconUrl: capsule.storeMetadata?.iconUrl,
            text: capsule.longDescription || capsule.storeMetadata?.text,
          },
        },
        {
          publisher: publisher || capsule.publisher,
          slug: slug || capsule.id,
          scopedId: capsule.scopedId,
          title: capsule.name,
          verified:
            capsule.trustLevel === "verified" || capsule.trustLevel === "signed",
          trustBadge: capsule.trustLevel,
          visibility: "local",
          type: capsule.type,
          version: capsule.version,
          description: capsule.longDescription || capsule.description,
          iconImage: capsule.storeMetadata?.iconUrl,
        },
      ),
    [capsule, publisher, slug],
  );

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
      <button className="icon-btn" type="button" onClick={onBack} aria-label="Back to catalog">
        <ChevronLeft size={15} strokeWidth={1.5} />
      </button>

      <DockCapsuleDetailSummary
        detail={{
          ...sharedDetail,
          updatedAt: process?.startedAt || sharedDetail.updatedAt,
          createdAt: process?.startedAt || sharedDetail.createdAt,
        }}
        metrics={[
          { label: "Status", value: status.label },
          { label: "Version", value: `v${capsule.version}` },
          { label: "Size", value: capsule.size },
        ]}
        actions={[
          {
            id: "delete",
            label: "Delete",
            tone: "danger",
            icon: <Trash2 size={13} strokeWidth={1.8} />,
            disabled: isRunning,
            onAction: () => onDelete(capsule),
          },
          ...(isRunning
            ? [
                {
                  id: "stop",
                  label: "Stop",
                  tone: "danger" as const,
                  icon: <Square size={13} strokeWidth={1.8} />,
                  onAction: () => onStop(capsule),
                },
                {
                  id: "open",
                  label: "Open",
                  tone: "secondary" as const,
                  icon: <ExternalLink size={13} strokeWidth={1.8} />,
                  disabled: !openReady,
                  onAction: () => onOpen(capsule, process),
                },
              ]
            : []),
          {
            id: "run",
            label: isRunning ? "Spawn another" : "Run",
            tone: "primary",
            icon: <Play size={13} strokeWidth={1.8} />,
            disabled: !canRun,
            onAction: () => onRun(capsule),
          },
        ]}
      />

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
          <DockReadmePanel
            markdown={sharedDetail.readmeMarkdown}
            source={sharedDetail.readmeSource}
            subtitle={sharedDetail.scopedId}
          />
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
          <DockReleaseTable
            releases={sharedDetail.releases}
            subtitle={`${capsule.releases.length} tracked releases`}
            renderActions={(release) => {
              const actionable = Boolean(release.manifestHash);
              return (
                <>
                  <button
                    className="btn btn-ghost"
                    type="button"
                    disabled={!actionable}
                    onClick={() =>
                      onRollbackRelease(capsule, {
                        version: release.version,
                        manifestHash: release.manifestHash,
                        contentHash: release.contentHash,
                        signatureStatus: release.signatureStatus,
                        isCurrent: release.isCurrent ?? false,
                        yankedAt: release.yankedAt,
                      })
                    }
                  >
                    Rollback
                  </button>
                  <button
                    className="btn btn-danger"
                    type="button"
                    disabled={!actionable || Boolean(release.yankedAt)}
                    onClick={() =>
                      onYankRelease(capsule, {
                        version: release.version,
                        manifestHash: release.manifestHash,
                        contentHash: release.contentHash,
                        signatureStatus: release.signatureStatus,
                        isCurrent: release.isCurrent ?? false,
                        yankedAt: release.yankedAt,
                      })
                    }
                  >
                    Yank
                  </button>
                </>
              );
            }}
          />
        </div>
      ) : null}
    </div>
  );
}
