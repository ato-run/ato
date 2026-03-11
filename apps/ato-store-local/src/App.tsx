import { useCallback, useEffect, useMemo, useState } from "react";
import { Menu } from "lucide-react";
import { ConfirmActionModal } from "./components/ConfirmActionModal";
import {
  getPermissionModeMessage,
  PermissionModeSelector,
} from "./components/PermissionModeSelector";
import { Sidebar } from "./components/Sidebar";
import { ProcessDrawer } from "./components/ProcessDrawer";
import { Toast, type ToastState } from "./components/Toast";
import { CatalogPage } from "./pages/CatalogPage";
import { DetailPage } from "./pages/DetailPage";
import { LogsPage } from "./pages/LogsPage";
import type {
  Capsule,
  CapsuleRelease,
  CapsuleTarget,
  CatalogViewMode,
  OsFilter,
  Process,
  ProcessLogLine,
  ProcessStatus,
  RunPermissionMode,
} from "./types";
import { detectPlatform, toOsFilterLabel } from "./utils/platform";
import { loadRegistryAuthToken, saveRegistryAuthToken } from "./utils/storage";

type Route =
  | { kind: "catalog" }
  | { kind: "detail"; capsuleId: string }
  | { kind: "logs"; processId: string };

type ConfirmState =
  | {
      kind: "run";
      capsule: Capsule;
      target: string;
      runtime: string;
      driver: string;
      permissionMode: RunPermissionMode;
      requiresPermissionGrant: boolean;
      port: number | undefined;
      env: Record<string, string>;
    }
  | {
      kind: "stop";
      process: Process;
    }
  | {
      kind: "delete";
      capsule: Capsule;
    }
  | {
      kind: "save-config";
      capsule: Capsule;
      runtimeConfig: CapsuleRuntimeOverride | undefined;
    }
  | {
      kind: "rollback-release";
      capsule: Capsule;
      release: CapsuleRelease;
    }
  | {
      kind: "yank-release";
      capsule: Capsule;
      release: CapsuleRelease;
    };

function nowIso(): string {
  return new Date().toISOString();
}

function nowTime(): string {
  return new Date().toLocaleTimeString("en-US", { hour12: false });
}

interface ApiPublisher {
  handle: string;
  verified?: boolean;
}

interface ApiStoreMetadata {
  icon_path?: string;
  iconPath?: string;
  text?: string;
  icon_url?: string;
  iconUrl?: string;
}

interface ApiSearchCapsuleRow {
  id: string;
  slug: string;
  scoped_id?: string;
  scopedId?: string;
  name: string;
  description: string;
  category?: string;
  type?: string;
  latestVersion?: string;
  latest_version?: string;
  latestSizeBytes?: number;
  latest_size_bytes?: number;
  sizeBytes?: number;
  size_bytes?: number;
  size?: string | number;
  publisher: ApiPublisher;
  store_metadata?: ApiStoreMetadata;
  storeMetadata?: ApiStoreMetadata;
}

interface ApiSearchResponse {
  capsules: ApiSearchCapsuleRow[];
}

interface ApiDetailResponse {
  id?: string;
  description?: string;
  latestVersion?: string;
  latest_version?: string;
  releases?: ApiReleaseRow[];
  manifest?: unknown;
  manifestToml?: string;
  manifest_toml?: string;
  repository?: string;
  readmeMarkdown?: string;
  readme_markdown?: string;
  readmeSource?: "artifact" | "none";
  readme_source?: "artifact" | "none";
  store_metadata?: ApiStoreMetadata;
  storeMetadata?: ApiStoreMetadata;
  runtime_config?: ApiRuntimeConfig;
  runtimeConfig?: ApiRuntimeConfig;
}

interface ApiRuntimeConfig {
  selected_target?: string;
  selectedTarget?: string;
  targets?: Record<string, ApiRuntimeTargetConfig>;
}

interface ApiRuntimeTargetConfig {
  port?: number | string | null;
  env?: Record<string, string>;
  permission_mode?: string | null;
  permissionMode?: string | null;
}

interface ApiReleaseRow {
  version: string;
  manifest_hash?: string;
  manifestHash?: string;
  content_hash?: string;
  contentHash?: string;
  signature_status?: string;
  signatureStatus?: string;
  is_current?: boolean;
  isCurrent?: boolean;
  yanked_at?: string;
  yankedAt?: string;
}

interface ApiProcessRow {
  id: string;
  name: string;
  pid: number;
  status: ProcessStatus;
  active: boolean;
  runtime: string;
  started_at: string;
  scoped_id?: string;
  target_label?: string;
}

interface ApiRunResponse {
  accepted: boolean;
  scoped_id: string;
  requested_target?: string;
  requested_port?: number;
}

interface ApiProcessLogsResponse {
  lines: string[];
}

interface ApiUrlReadyResponse {
  ready: boolean;
  status?: number;
  error?: string;
}

interface ApiDeleteCapsuleResponse {
  deleted: boolean;
  scoped_id: string;
}

interface ApiStoreMetadataResponse {
  updated?: boolean;
  scoped_id?: string;
  store_metadata?: ApiStoreMetadata;
  storeMetadata?: ApiStoreMetadata;
}

interface ApiRollbackResponse {
  scoped_id?: string;
  manifest_hash?: string;
  target_manifest_hash?: string;
  to_epoch?: number;
  pointer?: {
    manifest_hash?: string;
    epoch?: number;
  };
}

interface ApiYankResponse {
  scoped_id?: string;
  target_manifest_hash?: string;
  yanked?: boolean;
}

interface ApiWellKnownResponse {
  write_auth_required?: boolean;
  writeAuthRequired?: boolean;
}

const WEB_RUNTIME_DEFAULT_OS_ARCH: string[] = [
  "darwin/arm64",
  "darwin/x64",
  "linux/x64",
  "windows/x64",
];

interface RuntimeTargetOverride {
  port?: number;
  env: Record<string, string>;
  permissionMode?: Exclude<RunPermissionMode, "standard">;
}

interface CapsuleRuntimeOverride {
  selectedTarget?: string;
  targets: Record<string, RuntimeTargetOverride>;
}

type RuntimeOverrideStore = Record<string, CapsuleRuntimeOverride>;

function mapCapsuleType(raw: string | undefined): Capsule["type"] {
  const type = (raw ?? "").toLowerCase();
  if (type.includes("cli")) {
    return "cli";
  }
  if (type.includes("service")) {
    return "service";
  }
  return "webapp";
}

function mapIconKey(
  rawType: Capsule["type"],
  category: string | undefined,
): Capsule["iconKey"] {
  if (rawType === "cli") {
    return "package";
  }
  if (rawType === "service") {
    return "zap";
  }
  if ((category ?? "").toLowerCase().includes("web")) {
    return "globe";
  }
  return "box";
}

function toScopedId(row: ApiSearchCapsuleRow): string {
  return row.scopedId ?? row.scoped_id ?? `${row.publisher.handle}/${row.slug}`;
}

function mapStoreMetadata(
  value: ApiStoreMetadata | undefined,
): Capsule["storeMetadata"] {
  if (!value) {
    return undefined;
  }
  const iconPath = value.iconPath ?? value.icon_path;
  const text = value.text;
  const iconUrl = value.iconUrl ?? value.icon_url;
  if (!iconPath && !text && !iconUrl) {
    return undefined;
  }
  return { iconPath, text, iconUrl };
}

function formatSizeLabel(sizeBytes: unknown): string {
  if (
    typeof sizeBytes === "string" &&
    sizeBytes.trim().length > 0 &&
    !/^\d+$/.test(sizeBytes.trim())
  ) {
    return sizeBytes.trim();
  }
  if (
    typeof sizeBytes !== "number" ||
    !Number.isFinite(sizeBytes) ||
    sizeBytes <= 0
  ) {
    return "-";
  }
  const mb = sizeBytes / (1024 * 1024);
  if (mb >= 10) {
    return `${mb.toFixed(1)} MB`;
  }
  if (mb >= 1) {
    return `${mb.toFixed(2)} MB`;
  }
  const kb = sizeBytes / 1024;
  if (kb >= 1) {
    return `${kb.toFixed(0)} KB`;
  }
  return `${Math.floor(sizeBytes)} B`;
}

function mapReleaseRow(row: ApiReleaseRow): CapsuleRelease {
  return {
    version: row.version,
    manifestHash: row.manifestHash ?? row.manifest_hash,
    contentHash: row.contentHash ?? row.content_hash ?? "-",
    signatureStatus: row.signatureStatus ?? row.signature_status ?? "unknown",
    isCurrent: row.isCurrent ?? row.is_current ?? false,
    yankedAt: row.yankedAt ?? row.yanked_at,
  };
}

function baseReadme(scopedId: string, description: string): string {
  return `# ${scopedId}\n\n${description || "No description"}\n`;
}

function mapSearchRowToCapsule(
  row: ApiSearchCapsuleRow,
  platform: string,
): Capsule {
  const scopedId = toScopedId(row);
  const capsuleType = mapCapsuleType(row.type);
  const storeMetadata = mapStoreMetadata(
    row.storeMetadata ?? row.store_metadata,
  );
  const description = storeMetadata?.text ?? row.description ?? "";
  return {
    id: row.id,
    scopedId,
    name: row.name || row.slug,
    publisher: row.publisher.handle,
    iconKey: mapIconKey(capsuleType, row.category),
    description,
    longDescription: description,
    type: capsuleType,
    version: row.latestVersion ?? row.latest_version ?? "-",
    size: formatSizeLabel(
      row.latestSizeBytes ??
        row.latest_size_bytes ??
        row.sizeBytes ??
        row.size_bytes ??
        row.size,
    ),
    osArch: [platform],
    envHints: {},
    readme: baseReadme(scopedId, description),
    targets: [],
    releases: [],
    detailLoaded: false,
    localPath: "-",
    appUrl: null,
    trustLevel: row.publisher.verified ? "verified" : "unverified",
    storeMetadata,
  };
}

function parseStringRecord(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object") {
    return {};
  }
  const out: Record<string, string> = {};
  Object.entries(value as Record<string, unknown>).forEach(([key, val]) => {
    if (!key.trim()) {
      return;
    }
    out[key] = typeof val === "string" ? val : JSON.stringify(val);
  });
  return out;
}

function parseNumberPort(value: unknown): number | null {
  if (
    typeof value === "number" &&
    Number.isFinite(value) &&
    value >= 1 &&
    value <= 65535
  ) {
    return Math.floor(value);
  }
  if (typeof value === "string" && /^\d+$/.test(value.trim())) {
    const parsed = Number(value.trim());
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535) {
      return parsed;
    }
  }
  return null;
}

function normalizeRunPermissionMode(
  value: unknown,
): Exclude<RunPermissionMode, "standard"> | undefined {
  const normalized =
    typeof value === "string" ? value.trim().toLowerCase() : "";
  if (normalized === "sandbox") {
    return "sandbox";
  }
  if (normalized === "dangerous") {
    return "dangerous";
  }
  return undefined;
}

function parseTargetDriver(target: Record<string, unknown>): string {
  const explicit =
    typeof target.driver === "string" ? target.driver.trim().toLowerCase() : "";
  if (explicit) {
    return explicit;
  }
  const language =
    typeof target.language === "string"
      ? target.language.trim().toLowerCase()
      : "";
  if (language === "python" || language === "python3") {
    return "python";
  }
  if (language === "node" || language === "nodejs") {
    return "node";
  }
  if (language === "deno") {
    return "deno";
  }
  return "";
}

function requiresPermissionGrant(target: CapsuleTarget | undefined): boolean {
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
function cloneRuntimeOverride(
  value: CapsuleRuntimeOverride | undefined,
): CapsuleRuntimeOverride {
  if (!value) {
    return { targets: {} };
  }
  const targets: Record<string, RuntimeTargetOverride> = {};
  Object.entries(value.targets).forEach(([label, target]) => {
    targets[label] = {
      port: target.port,
      env: { ...target.env },
      permissionMode: target.permissionMode,
    };
  });
  return {
    selectedTarget: value.selectedTarget,
    targets,
  };
}

function compactRuntimeOverride(
  value: CapsuleRuntimeOverride,
): CapsuleRuntimeOverride | undefined {
  const targets: Record<string, RuntimeTargetOverride> = {};
  Object.entries(value.targets).forEach(([rawLabel, target]) => {
    const label = rawLabel.trim();
    if (!label) {
      return;
    }
    const env: Record<string, string> = {};
    Object.entries(target.env).forEach(([rawKey, envValue]) => {
      const key = rawKey.trim();
      if (!key) {
        return;
      }
      env[key] = envValue;
    });
    const port = parseNumberPort(target.port);
    const permissionMode = normalizeRunPermissionMode(target.permissionMode);
    if (!port && Object.keys(env).length === 0 && !permissionMode) {
      return;
    }
    targets[label] = {
      ...(port ? { port } : {}),
      env,
      ...(permissionMode ? { permissionMode } : {}),
    };
  });

  const selectedTarget = value.selectedTarget?.trim();
  if (!selectedTarget && Object.keys(targets).length === 0) {
    return undefined;
  }
  return {
    ...(selectedTarget ? { selectedTarget } : {}),
    targets,
  };
}

function runtimeOverrideFromApi(
  value: ApiRuntimeConfig | undefined,
): CapsuleRuntimeOverride | undefined {
  if (!value) {
    return undefined;
  }
  const selectedTarget = (
    value.selectedTarget ??
    value.selected_target ??
    ""
  ).trim();
  const targets: Record<string, RuntimeTargetOverride> = {};
  Object.entries(value.targets ?? {}).forEach(([rawLabel, rawTarget]) => {
    const label = rawLabel.trim();
    if (!label) {
      return;
    }
    const env: Record<string, string> = {};
    Object.entries(rawTarget?.env ?? {}).forEach(([rawKey, envValue]) => {
      const key = rawKey.trim();
      if (!key) {
        return;
      }
      env[key] = typeof envValue === "string" ? envValue : String(envValue);
    });
    const port = parseNumberPort(rawTarget?.port ?? null);
    const permissionMode = normalizeRunPermissionMode(
      rawTarget?.permissionMode ?? rawTarget?.permission_mode ?? null,
    );
    if (!port && Object.keys(env).length === 0 && !permissionMode) {
      return;
    }
    targets[label] = {
      ...(port ? { port } : {}),
      env,
      ...(permissionMode ? { permissionMode } : {}),
    };
  });
  const normalized = compactRuntimeOverride({
    ...(selectedTarget ? { selectedTarget } : {}),
    targets,
  });
  return normalized;
}

function rawStringList(value: unknown): string[] {
  if (typeof value === "string") {
    return value
      .split(",")
      .map((entry) => entry.trim())
      .filter((entry) => entry.length > 0);
  }
  if (Array.isArray(value)) {
    return value.flatMap((entry) => rawStringList(entry));
  }
  return [];
}

function normalizeOs(value: string): "darwin" | "linux" | "windows" | null {
  const normalized = value.trim().toLowerCase();
  if (
    normalized === "darwin" ||
    normalized === "macos" ||
    normalized === "mac"
  ) {
    return "darwin";
  }
  if (normalized === "linux") {
    return "linux";
  }
  if (
    normalized === "windows" ||
    normalized === "win" ||
    normalized === "win32"
  ) {
    return "windows";
  }
  return null;
}

function normalizeArch(value: string): "x64" | "arm64" | null {
  const normalized = value.trim().toLowerCase();
  if (
    normalized === "x64" ||
    normalized === "amd64" ||
    normalized === "x86_64"
  ) {
    return "x64";
  }
  if (normalized === "arm64" || normalized === "aarch64") {
    return "arm64";
  }
  return null;
}

function collectOsArchStrings(value: unknown, out: Set<string>): void {
  if (typeof value === "string") {
    const normalized = value.toLowerCase();
    if (/^(darwin|linux|windows)\/(x64|arm64)$/.test(normalized)) {
      out.add(normalized);
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((entry) => collectOsArchStrings(entry, out));
    return;
  }
  if (value && typeof value === "object") {
    const objectValue = value as Record<string, unknown>;
    const osValues = rawStringList(objectValue.os)
      .map(normalizeOs)
      .filter((entry): entry is NonNullable<typeof entry> => Boolean(entry));
    const archValues = rawStringList(objectValue.arch)
      .map(normalizeArch)
      .filter((entry): entry is NonNullable<typeof entry> => Boolean(entry));
    if (osValues.length > 0 && archValues.length > 0) {
      osValues.forEach((os) => {
        archValues.forEach((arch) => out.add(`${os}/${arch}`));
      });
    }
    Object.values(objectValue).forEach((entry) =>
      collectOsArchStrings(entry, out),
    );
  }
}

function inferOsArchFromTargets(targets: CapsuleTarget[]): string[] {
  const hasWebTarget = targets.some(
    (target) => target.runtime.trim().toLowerCase() === "web",
  );
  if (hasWebTarget) {
    return WEB_RUNTIME_DEFAULT_OS_ARCH;
  }
  return [];
}

function parseTargets(manifest: unknown): {
  defaultTarget?: string;
  targets: CapsuleTarget[];
} {
  if (!manifest || typeof manifest !== "object") {
    return { targets: [] };
  }
  const root = manifest as Record<string, unknown>;
  const targets = root.targets;
  if (!targets || typeof targets !== "object") {
    return { targets: [] };
  }

  const table = targets as Record<string, unknown>;
  const services =
    root.services && typeof root.services === "object"
      ? (root.services as Record<string, unknown>)
      : {};
  const globalPort = parseNumberPort(table.port ?? root.port);
  const globalEnv = parseStringRecord(table.env ?? root.env);
  const parsedTargets: CapsuleTarget[] = [];

  for (const [key, value] of Object.entries(table)) {
    if (key === "port" || key === "env" || key === "preference") {
      continue;
    }
    if (!value || typeof value !== "object") {
      continue;
    }
    const target = value as Record<string, unknown>;
    const runtime = String(target.runtime ?? "").trim();
    if (!runtime) {
      continue;
    }
    const env = {
      ...globalEnv,
      ...parseStringRecord(target.env),
    };
    const requiredEnvSet = new Set<string>();
    const requiredRaw = target.required_env;
    if (Array.isArray(requiredRaw)) {
      requiredRaw.forEach((value) => {
        if (typeof value !== "string") {
          return;
        }
        const key = value.trim();
        if (!key) {
          return;
        }
        requiredEnvSet.add(key);
      });
    }
    const legacyRequired = env.ATO_ORCH_REQUIRED_ENVS;
    if (typeof legacyRequired === "string") {
      legacyRequired
        .split(",")
        .map((value) => value.trim())
        .filter((value) => value.length > 0)
        .forEach((key) => requiredEnvSet.add(key));
    }
    parsedTargets.push({
      label: key,
      runtime,
      driver: parseTargetDriver(target),
      port: parseNumberPort(target.port) ?? globalPort,
      env,
      requiredEnv: Array.from(requiredEnvSet),
    });
  }

  const defaultTarget =
    typeof root.default_target === "string"
      ? root.default_target
      : typeof root.defaultTarget === "string"
        ? root.defaultTarget
        : undefined;

  const requiredEnvByTarget = new Map<string, Set<string>>();
  parsedTargets.forEach((target) => {
    requiredEnvByTarget.set(target.label, new Set(target.requiredEnv));
  });

  let mainServiceTarget: string | undefined;
  for (const [serviceName, rawService] of Object.entries(services)) {
    if (!rawService || typeof rawService !== "object") {
      continue;
    }
    const service = rawService as Record<string, unknown>;
    const targetLabelRaw =
      typeof service.target === "string"
        ? service.target
        : serviceName === "main"
          ? defaultTarget
          : undefined;
    const targetLabel = targetLabelRaw?.trim();
    if (!targetLabel) {
      continue;
    }
    if (serviceName === "main") {
      mainServiceTarget = targetLabel;
    }
    if (!requiredEnvByTarget.has(targetLabel)) {
      requiredEnvByTarget.set(targetLabel, new Set<string>());
    }
  }

  if (Object.keys(services).length > 0) {
    const aggregateTarget =
      mainServiceTarget ?? defaultTarget ?? parsedTargets[0]?.label;
    if (aggregateTarget) {
      const aggregate =
        requiredEnvByTarget.get(aggregateTarget) ?? new Set<string>();
      for (const keys of requiredEnvByTarget.values()) {
        keys.forEach((key) => aggregate.add(key));
      }
      requiredEnvByTarget.set(aggregateTarget, aggregate);
    }
  }

  parsedTargets.forEach((target) => {
    target.requiredEnv = Array.from(
      requiredEnvByTarget.get(target.label) ?? new Set(target.requiredEnv),
    );
  });

  return { defaultTarget, targets: parsedTargets };
}

function parseRoute(): Route {
  const { pathname, search } = window.location;
  if (pathname.startsWith("/capsule/")) {
    const capsuleId = decodeURIComponent(pathname.replace("/capsule/", ""));
    return { kind: "detail", capsuleId };
  }
  if (pathname === "/logs") {
    const params = new URLSearchParams(search);
    const processId = params.get("processId") ?? "";
    return { kind: "logs", processId };
  }
  return { kind: "catalog" };
}

function latestProcessForCapsule(
  processes: Process[],
  capsuleId: string,
): Process | undefined {
  return [...processes]
    .filter((process) => process.capsuleId === capsuleId)
    .sort((left, right) => right.startedAt.localeCompare(left.startedAt))[0];
}

function parseLogLine(line: string, index: number): ProcessLogLine {
  const matched = line.match(/^\[([^\]]+)\]\s+([A-Za-z]+)\s+(.*)$/);
  if (matched) {
    return {
      index,
      timestamp: matched[1],
      level: matched[2].toUpperCase(),
      message: matched[3],
    };
  }
  return {
    index,
    timestamp: nowTime(),
    level: "INFO",
    message: line,
  };
}

function toApiProcess(row: ApiProcessRow, catalogCapsules: Capsule[]): Process {
  const scopedId = row.scoped_id ?? row.name;
  const matchedCapsule = catalogCapsules.find(
    (capsule) => capsule.scopedId === scopedId,
  );
  return {
    id: row.id,
    name: row.name,
    pid: row.pid,
    capsuleId: matchedCapsule?.id ?? scopedId,
    scopedId,
    active: row.active,
    status: row.status,
    startedAt: row.started_at,
    lastSeenAt: nowIso(),
    runtime: row.runtime,
    targetLabel: row.target_label,
  };
}

function parseScopedId(
  scopedId: string,
): { publisher: string; slug: string } | null {
  const parts = scopedId.split("/");
  if (parts.length !== 2) {
    return null;
  }
  return { publisher: parts[0], slug: parts[1] };
}

interface ActionFailureError extends Error {
  copyText?: string;
}

function createActionFailure(
  message: string,
  copyText?: string,
): ActionFailureError {
  const error = new Error(message) as ActionFailureError;
  error.copyText = copyText ?? message;
  return error;
}

function parseActionErrorResponse(
  text: string,
  fallbackMessage: string,
): { message: string; copyText: string } {
  const trimmed = text.trim();
  if (!trimmed) {
    return { message: fallbackMessage, copyText: fallbackMessage };
  }
  try {
    const parsed = JSON.parse(trimmed) as {
      error?: unknown;
      message?: unknown;
    };
    const message =
      typeof parsed.message === "string" && parsed.message.trim().length > 0
        ? parsed.message.trim()
        : fallbackMessage;
    const code =
      typeof parsed.error === "string" && parsed.error.trim().length > 0
        ? parsed.error.trim()
        : "";
    return {
      message: code ? `${message} (${code})` : message,
      copyText: JSON.stringify(parsed, null, 2),
    };
  } catch {
    return { message: trimmed, copyText: trimmed };
  }
}

export default function App(): JSX.Element {
  const platform = useMemo(() => detectPlatform(), []);
  const [route, setRoute] = useState<Route>(() => parseRoute());
  const [isMobileViewport, setIsMobileViewport] = useState<boolean>(
    () => window.matchMedia("(max-width: 768px)").matches,
  );
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);
  const [catalogCapsules, setCatalogCapsules] = useState<Capsule[]>([]);
  const [processes, setProcesses] = useState<Process[]>([]);
  const [openReadyByProcessId, setOpenReadyByProcessId] = useState<
    Record<string, boolean>
  >({});
  const [runtimeOverrides, setRuntimeOverrides] =
    useState<RuntimeOverrideStore>({});
  const [dirtyRuntimeConfigCapsules, setDirtyRuntimeConfigCapsules] = useState<
    Record<string, true>
  >({});
  const [logsByProcessId, setLogsByProcessId] = useState<
    Record<string, ProcessLogLine[]>
  >({});
  const [isLoadingCapsules, setIsLoadingCapsules] = useState(true);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<OsFilter>("all");
  const [viewMode, setViewMode] = useState<CatalogViewMode>("list");
  const [toast, setToast] = useState<ToastState | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [isSubmittingConfirm, setIsSubmittingConfirm] = useState(false);
  const [writeAuthRequired, setWriteAuthRequired] = useState(false);
  const [registryAuthToken, setRegistryAuthToken] = useState<string>(() => {
    const params = new URLSearchParams(window.location.search);
    const fromQuery = params.get("authToken") ?? params.get("token");
    if (fromQuery && fromQuery.trim()) {
      const token = fromQuery.trim();
      saveRegistryAuthToken(token);
      return token;
    }
    return loadRegistryAuthToken();
  });

  const createWriteHeaders = useCallback(
    (contentType?: string): Record<string, string> => {
      const headers: Record<string, string> = {};
      if (contentType) {
        headers["content-type"] = contentType;
      }
      if (registryAuthToken.trim()) {
        headers.authorization = `Bearer ${registryAuthToken.trim()}`;
      }
      return headers;
    },
    [registryAuthToken],
  );

  const updateRegistryAuthToken = useCallback((value: string): void => {
    setRegistryAuthToken(value);
    saveRegistryAuthToken(value);
  }, []);

  const showSuccessToast = useCallback((message: string): void => {
    setToast({
      kind: "success",
      message,
    });
  }, []);

  const showErrorToast = useCallback(
    (message: string, copyText?: string): void => {
      setToast({
        kind: "error",
        message,
        copyText: copyText ?? message,
        sticky: true,
      });
    },
    [],
  );

  useEffect(() => {
    const onPopState = (): void => setRoute(parseRoute());
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  useEffect(() => {
    const mediaQuery = window.matchMedia("(max-width: 768px)");
    const applyMatch = (matches: boolean): void => {
      setIsMobileViewport(matches);
      if (!matches) {
        setMobileSidebarOpen(false);
      }
    };
    applyMatch(mediaQuery.matches);
    const onChange = (event: MediaQueryListEvent): void => {
      applyMatch(event.matches);
    };
    mediaQuery.addEventListener("change", onChange);
    return () => mediaQuery.removeEventListener("change", onChange);
  }, []);

  useEffect(() => {
    if (!isMobileViewport || !mobileSidebarOpen) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setMobileSidebarOpen(false);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isMobileViewport, mobileSidebarOpen]);

  useEffect(() => {
    void (async () => {
      try {
        const response = await fetch("/.well-known/capsule.json");
        if (!response.ok) {
          return;
        }
        const payload = (await response.json()) as ApiWellKnownResponse;
        const required =
          payload.writeAuthRequired ?? payload.write_auth_required ?? false;
        setWriteAuthRequired(Boolean(required));
      } catch {
        // ignore
      }
    })();
  }, []);

  useEffect(() => {
    if (!toast || toast.sticky) {
      return;
    }
    const timer = window.setTimeout(() => setToast(null), 2500);
    return () => window.clearTimeout(timer);
  }, [toast]);

  const loadCatalogCapsules = useCallback(async (): Promise<void> => {
    setIsLoadingCapsules(true);
    try {
      const response = await fetch("/v1/manifest/capsules?limit=200");
      const payload = (await response.json()) as ApiSearchResponse;
      const rows = Array.isArray(payload.capsules) ? payload.capsules : [];
      setCatalogCapsules(
        rows.map((row) => mapSearchRowToCapsule(row, platform)),
      );
    } catch (error) {
      const message =
        error instanceof Error ? error.message : "failed to fetch capsules";
      showErrorToast(`一覧取得失敗: ${message}`, message);
      setCatalogCapsules([]);
    } finally {
      setIsLoadingCapsules(false);
    }
  }, [platform, showErrorToast]);

  useEffect(() => {
    void loadCatalogCapsules();
  }, [loadCatalogCapsules]);

  const loadProcesses = useCallback(async (): Promise<void> => {
    try {
      const response = await fetch("/v1/local/processes");
      if (!response.ok) {
        return;
      }
      const rows = (await response.json()) as ApiProcessRow[];
      if (!Array.isArray(rows)) {
        setProcesses([]);
        return;
      }
      setProcesses(rows.map((row) => toApiProcess(row, catalogCapsules)));
    } catch {
      // ignore polling failures
    }
  }, [catalogCapsules]);

  useEffect(() => {
    void loadProcesses();
    const timer = window.setInterval(() => {
      void loadProcesses();
    }, 1800);
    return () => window.clearInterval(timer);
  }, [loadProcesses]);

  const fetchProcessLogs = useCallback(
    async (processId: string): Promise<void> => {
      if (!processId) {
        return;
      }
      try {
        const response = await fetch(
          `/v1/local/processes/${encodeURIComponent(processId)}/logs?tail=500`,
        );
        if (!response.ok) {
          return;
        }
        const payload = (await response.json()) as ApiProcessLogsResponse;
        const lines = Array.isArray(payload.lines) ? payload.lines : [];
        const parsed = lines.map((line, index) =>
          parseLogLine(line, index + 1),
        );
        setLogsByProcessId((prev) => ({
          ...prev,
          [processId]: parsed,
        }));
      } catch {
        // ignore
      }
    },
    [],
  );

  useEffect(() => {
    if (route.kind !== "detail") {
      return;
    }
    const capsule = catalogCapsules.find(
      (entry) => entry.id === route.capsuleId,
    );
    if (!capsule) {
      return;
    }
    const process = latestProcessForCapsule(processes, capsule.id);
    if (!process) {
      return;
    }
    void fetchProcessLogs(process.id);
    const timer = window.setInterval(() => {
      void fetchProcessLogs(process.id);
    }, 1400);
    return () => window.clearInterval(timer);
  }, [catalogCapsules, fetchProcessLogs, processes, route]);

  useEffect(() => {
    if (route.kind !== "logs" || !route.processId) {
      return;
    }
    void fetchProcessLogs(route.processId);
    const timer = window.setInterval(() => {
      void fetchProcessLogs(route.processId);
    }, 1400);
    return () => window.clearInterval(timer);
  }, [fetchProcessLogs, route]);

  const visibleCapsules = useMemo(() => {
    return catalogCapsules.filter((capsule) => {
      const query = search.trim().toLowerCase();
      const matchesSearch =
        query.length === 0 ||
        capsule.scopedId.toLowerCase().includes(query) ||
        capsule.description.toLowerCase().includes(query);
      const matchesFilter =
        filter === "all" ||
        capsule.osArch.length === 0 ||
        capsule.osArch.some((entry) => toOsFilterLabel(entry) === filter);
      return matchesSearch && matchesFilter;
    });
  }, [catalogCapsules, filter, search]);

  const navigate = (path: string): void => {
    window.history.pushState(null, "", path);
    setRoute(parseRoute());
  };

  const resolveSelectedTarget = useCallback(
    (capsule: Capsule): string => {
      if (capsule.targets.length === 0) {
        return "";
      }
      const stored = runtimeOverrides[capsule.id]?.selectedTarget;
      if (stored && capsule.targets.some((target) => target.label === stored)) {
        return stored;
      }
      if (
        capsule.defaultTarget &&
        capsule.targets.some((target) => target.label === capsule.defaultTarget)
      ) {
        return capsule.defaultTarget;
      }
      return capsule.targets[0].label;
    },
    [runtimeOverrides],
  );

  const resolveEnvValues = useCallback(
    (capsule: Capsule, targetLabel: string): Record<string, string> => {
      const target = capsule.targets.find(
        (entry) => entry.label === targetLabel,
      );
      const base = target?.env ?? capsule.envHints;
      const override =
        runtimeOverrides[capsule.id]?.targets[targetLabel]?.env ?? {};
      return {
        ...base,
        ...override,
      };
    },
    [runtimeOverrides],
  );

  const resolveBaseEnvKeys = useCallback(
    (capsule: Capsule, targetLabel: string): string[] => {
      const target = capsule.targets.find(
        (entry) => entry.label === targetLabel,
      );
      if (!target) {
        return [];
      }
      return Object.keys(target.env);
    },
    [],
  );

  const resolveRequiredEnvKeys = useCallback(
    (capsule: Capsule, targetLabel: string): string[] => {
      const target = capsule.targets.find(
        (entry) => entry.label === targetLabel,
      );
      if (!target) {
        return [];
      }
      return target.requiredEnv;
    },
    [],
  );

  const resolvePortValue = useCallback(
    (capsule: Capsule, targetLabel: string): string => {
      const target = capsule.targets.find(
        (entry) => entry.label === targetLabel,
      );
      const override = runtimeOverrides[capsule.id]?.targets[targetLabel];
      const port = override?.port ?? target?.port;
      return port ? String(port) : "";
    },
    [runtimeOverrides],
  );

  const resolvePermissionMode = useCallback(
    (capsule: Capsule, targetLabel: string): RunPermissionMode => {
      const override = runtimeOverrides[capsule.id]?.targets[targetLabel];
      return override?.permissionMode ?? "standard";
    },
    [runtimeOverrides],
  );

  const requestRun = (capsule: Capsule): void => {
    const target = resolveSelectedTarget(capsule);
    const targetMeta = capsule.targets.find((entry) => entry.label === target);
    const env = { ...resolveEnvValues(capsule, target) };
    resolveRequiredEnvKeys(capsule, target).forEach((key) => {
      if (!(key in env)) {
        env[key] = "";
      }
    });
    const portText = resolvePortValue(capsule, target);
    const port = parseNumberPort(portText) ?? undefined;
    setConfirmState({
      kind: "run",
      capsule,
      target,
      runtime: targetMeta?.runtime ?? "",
      driver: targetMeta?.driver ?? "",
      permissionMode: resolvePermissionMode(capsule, target),
      requiresPermissionGrant: requiresPermissionGrant(targetMeta),
      port,
      env,
    });
  };

  const requestStop = (capsule: Capsule): void => {
    const process = processes.find(
      (entry) => entry.capsuleId === capsule.id && entry.active,
    );
    if (!process) {
      return;
    }
    setConfirmState({ kind: "stop", process });
  };

  const requestDelete = (capsule: Capsule): void => {
    setConfirmState({ kind: "delete", capsule });
  };

  const requestRollbackRelease = (
    capsule: Capsule,
    release: CapsuleRelease,
  ): void => {
    setConfirmState({ kind: "rollback-release", capsule, release });
  };

  const requestYankRelease = (
    capsule: Capsule,
    release: CapsuleRelease,
  ): void => {
    setConfirmState({ kind: "yank-release", capsule, release });
  };

  const openLogs = (processId: string): void => {
    window.open(`/logs?processId=${encodeURIComponent(processId)}`, "_blank");
  };

  const resolveAppUrlForCapsule = useCallback(
    (capsule: Capsule, process?: Process): string | null => {
      const targets = capsule.targets ?? [];
      if (targets.length === 0) {
        return null;
      }

      const byLabel = new Map(targets.map((target) => [target.label, target]));
      const override = runtimeOverrides[capsule.id];
      const candidates = [
        process?.targetLabel,
        override?.selectedTarget,
        capsule.defaultTarget,
        targets[0]?.label,
      ].filter((value): value is string => Boolean(value && value.trim()));

      let selectedLabel = candidates.find((label) => byLabel.has(label));
      if (!selectedLabel) {
        selectedLabel = targets[0]?.label;
      }
      if (!selectedLabel) {
        return null;
      }

      const target = byLabel.get(selectedLabel);
      const overridePort = override?.targets[selectedLabel]?.port;
      const port = overridePort ?? target?.port ?? null;
      if (!port || !Number.isFinite(port) || port < 1 || port > 65535) {
        return null;
      }

      const protocol = window.location.protocol || "http:";
      const host = window.location.hostname || "127.0.0.1";
      return `${protocol}//${host}:${port}`;
    },
    [runtimeOverrides],
  );

  useEffect(() => {
    const runningProcesses = processes.filter((process) => process.active);
    if (runningProcesses.length === 0) {
      setOpenReadyByProcessId({});
      return;
    }

    let cancelled = false;

    const pollOpenReadiness = async (): Promise<void> => {
      const next: Record<string, boolean> = {};

      await Promise.all(
        runningProcesses.map(async (process) => {
          const capsule = catalogCapsules.find(
            (entry) => entry.id === process.capsuleId,
          );
          if (!capsule) {
            next[process.id] = false;
            return;
          }
          const url = resolveAppUrlForCapsule(capsule, process);
          if (!url) {
            next[process.id] = false;
            return;
          }
          try {
            const response = await fetch(
              `/v1/local/url-ready?url=${encodeURIComponent(url)}`,
            );
            if (!response.ok) {
              next[process.id] = false;
              return;
            }
            const payload = (await response.json()) as ApiUrlReadyResponse;
            next[process.id] = payload.ready === true;
          } catch {
            next[process.id] = false;
          }
        }),
      );

      if (cancelled) {
        return;
      }

      setOpenReadyByProcessId(next);
    };

    void pollOpenReadiness();
    const timer = window.setInterval(() => {
      void pollOpenReadiness();
    }, 1400);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [catalogCapsules, processes, resolveAppUrlForCapsule]);

  const isOpenReady = useCallback(
    (process?: Process): boolean => {
      if (!process || !process.active) {
        return false;
      }
      return openReadyByProcessId[process.id] === true;
    },
    [openReadyByProcessId],
  );

  const openRunningTarget = useCallback(
    (capsule: Capsule, process?: Process): void => {
      if (!isOpenReady(process)) {
        showErrorToast(
          "アプリURLの応答(HTTP 200)を確認中です。起動完了後にOpenできます。",
        );
        return;
      }
      const url = resolveAppUrlForCapsule(capsule, process);
      if (!url) {
        showErrorToast("Open先のアプリポートを解決できませんでした");
        return;
      }
      window.open(url, "_blank");
    },
    [isOpenReady, resolveAppUrlForCapsule, showErrorToast],
  );

  const clearProcessLogs = useCallback(
    async (processId: string): Promise<void> => {
      await fetch(`/v1/local/processes/${encodeURIComponent(processId)}/logs`, {
        method: "DELETE",
        headers: createWriteHeaders(),
      });
      await fetchProcessLogs(processId);
    },
    [createWriteHeaders, fetchProcessLogs],
  );

  const runConfirmed = useCallback(
    async (state: Extract<ConfirmState, { kind: "run" }>): Promise<void> => {
      const parsed = parseScopedId(state.capsule.scopedId);
      if (!parsed) {
        throw createActionFailure("invalid scoped id");
      }
      const response = await fetch(
        `/v1/local/capsules/by/${encodeURIComponent(parsed.publisher)}/${encodeURIComponent(parsed.slug)}/run`,
        {
          method: "POST",
          headers: createWriteHeaders("application/json"),
          body: JSON.stringify({
            confirmed: true,
            target: state.target || undefined,
            port: state.port,
            env: state.env,
            ...(state.permissionMode !== "standard"
              ? { permission_mode: state.permissionMode }
              : {}),
          }),
        },
      );
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `run failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
      const payload = (await response.json()) as ApiRunResponse;
      if (!payload.accepted) {
        throw createActionFailure("run was not accepted");
      }
    },
    [createWriteHeaders],
  );

  const stopConfirmed = useCallback(
    async (state: Extract<ConfirmState, { kind: "stop" }>): Promise<void> => {
      const response = await fetch(
        `/v1/local/processes/${encodeURIComponent(state.process.id)}/stop`,
        {
          method: "POST",
          headers: createWriteHeaders("application/json"),
          body: JSON.stringify({
            confirmed: true,
          }),
        },
      );
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `stop failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
    },
    [createWriteHeaders],
  );

  const deleteConfirmed = useCallback(
    async (state: Extract<ConfirmState, { kind: "delete" }>): Promise<void> => {
      const parsed = parseScopedId(state.capsule.scopedId);
      if (!parsed) {
        throw createActionFailure("invalid scoped id");
      }
      const response = await fetch(
        `/v1/local/capsules/by/${encodeURIComponent(parsed.publisher)}/${encodeURIComponent(parsed.slug)}?confirmed=true`,
        {
          method: "DELETE",
          headers: createWriteHeaders(),
        },
      );
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `delete failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
      const payload = (await response.json()) as ApiDeleteCapsuleResponse;
      if (!payload.deleted) {
        throw createActionFailure("delete was not accepted");
      }
      if (route.kind === "detail" && route.capsuleId === state.capsule.id) {
        navigate("/");
      }
      await loadCatalogCapsules();
      await loadProcesses();
    },
    [createWriteHeaders, loadCatalogCapsules, loadProcesses, navigate, route],
  );

  const rollbackReleaseConfirmed = useCallback(
    async (
      state: Extract<ConfirmState, { kind: "rollback-release" }>,
    ): Promise<void> => {
      if (!state.release.manifestHash) {
        throw createActionFailure("rollback target is missing a manifest hash");
      }
      const response = await fetch("/v1/manifest/rollback", {
        method: "POST",
        headers: createWriteHeaders("application/json"),
        body: JSON.stringify({
          scoped_id: state.capsule.scopedId,
          target_manifest_hash: state.release.manifestHash,
        }),
      });
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `rollback failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
      const payload = (await response.json()) as ApiRollbackResponse;
      if (
        !payload.target_manifest_hash &&
        !payload.manifest_hash &&
        !payload.pointer?.manifest_hash
      ) {
        throw createActionFailure("rollback was not accepted");
      }
      await loadCatalogCapsules();
      await loadProcesses();
    },
    [createWriteHeaders, loadCatalogCapsules, loadProcesses],
  );

  const yankReleaseConfirmed = useCallback(
    async (
      state: Extract<ConfirmState, { kind: "yank-release" }>,
    ): Promise<void> => {
      if (!state.release.manifestHash) {
        throw createActionFailure("yank target is missing a manifest hash");
      }
      const response = await fetch("/v1/manifest/yank", {
        method: "POST",
        headers: createWriteHeaders("application/json"),
        body: JSON.stringify({
          scoped_id: state.capsule.scopedId,
          target_manifest_hash: state.release.manifestHash,
        }),
      });
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `yank failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
      const payload = (await response.json()) as ApiYankResponse;
      if (payload.yanked !== true) {
        throw createActionFailure("yank was not accepted");
      }
      await loadCatalogCapsules();
      await loadProcesses();
    },
    [createWriteHeaders, loadCatalogCapsules, loadProcesses],
  );

  const persistRuntimeConfig = useCallback(
    async (
      capsuleId: string,
      runtimeConfig: CapsuleRuntimeOverride | undefined,
    ): Promise<void> => {
      const capsule = catalogCapsules.find((entry) => entry.id === capsuleId);
      if (!capsule) {
        return;
      }
      const parsed = parseScopedId(capsule.scopedId);
      if (!parsed) {
        return;
      }
      const compacted = runtimeConfig
        ? compactRuntimeOverride(runtimeConfig)
        : undefined;
      const response = await fetch(
        `/v1/local/capsules/by/${encodeURIComponent(parsed.publisher)}/${encodeURIComponent(parsed.slug)}/runtime-config`,
        {
          method: "PUT",
          headers: createWriteHeaders("application/json"),
          body: JSON.stringify({
            selected_target: compacted?.selectedTarget,
            targets: Object.fromEntries(
              Object.entries(compacted?.targets ?? {}).map(
                ([label, target]) => [
                  label,
                  {
                    ...(target.port ? { port: target.port } : {}),
                    env: target.env,
                    ...(target.permissionMode
                      ? { permission_mode: target.permissionMode }
                      : {}),
                  },
                ],
              ),
            ),
          }),
        },
      );
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const text = await response.text();
        const parsedError = parseActionErrorResponse(
          text,
          `runtime config update failed: ${response.status}`,
        );
        throw createActionFailure(parsedError.message, parsedError.copyText);
      }
      const payload = (await response.json()) as ApiRuntimeConfig;
      const saved = runtimeOverrideFromApi(payload);
      setRuntimeOverrides((prev) => {
        const next = { ...prev };
        if (saved) {
          next[capsuleId] = saved;
        } else {
          delete next[capsuleId];
        }
        return next;
      });
    },
    [catalogCapsules, createWriteHeaders],
  );

  const updateRuntimeConfig = useCallback(
    (
      capsuleId: string,
      updater: (draft: CapsuleRuntimeOverride) => void,
    ): void => {
      const draft = cloneRuntimeOverride(runtimeOverrides[capsuleId]);
      updater(draft);
      const compacted = compactRuntimeOverride(draft);
      setRuntimeOverrides((prev) => {
        const next = { ...prev };
        if (compacted) {
          next[capsuleId] = compacted;
        } else {
          delete next[capsuleId];
        }
        return next;
      });
      setDirtyRuntimeConfigCapsules((prev) => ({
        ...prev,
        [capsuleId]: true,
      }));
    },
    [runtimeOverrides],
  );

  const requestSaveRuntimeConfig = useCallback(
    (capsule: Capsule): void => {
      const runtimeConfig = compactRuntimeOverride(
        cloneRuntimeOverride(runtimeOverrides[capsule.id]),
      );
      if (!dirtyRuntimeConfigCapsules[capsule.id]) {
        showSuccessToast("保存する変更はありません");
        return;
      }
      setConfirmState({
        kind: "save-config",
        capsule,
        runtimeConfig,
      });
    },
    [dirtyRuntimeConfigCapsules, runtimeOverrides, showSuccessToast],
  );

  const saveRuntimeConfigConfirmed = useCallback(
    async (
      state: Extract<ConfirmState, { kind: "save-config" }>,
    ): Promise<void> => {
      await persistRuntimeConfig(state.capsule.id, state.runtimeConfig);
      setDirtyRuntimeConfigCapsules((prev) => {
        const next = { ...prev };
        delete next[state.capsule.id];
        return next;
      });
    },
    [persistRuntimeConfig],
  );

  const confirmAction = useCallback(async (): Promise<void> => {
    if (!confirmState || isSubmittingConfirm) {
      return;
    }
    const action = confirmState;
    setConfirmState(null);
    setIsSubmittingConfirm(true);
    try {
      if (action.kind === "run") {
        await runConfirmed(action);
        showSuccessToast("Runを受け付けました");
      } else if (action.kind === "stop") {
        await stopConfirmed(action);
        showSuccessToast("Stopを受け付けました");
      } else if (action.kind === "save-config") {
        await saveRuntimeConfigConfirmed(action);
        showSuccessToast("Configurationを保存しました");
      } else if (action.kind === "rollback-release") {
        await rollbackReleaseConfirmed(action);
        showSuccessToast(`Rollbackを受け付けました: ${action.release.version}`);
      } else if (action.kind === "yank-release") {
        await yankReleaseConfirmed(action);
        showSuccessToast(`Yankを受け付けました: ${action.release.version}`);
      } else {
        await deleteConfirmed(action);
        showSuccessToast("Capsuleを削除しました");
      }
      await loadProcesses();
    } catch (error) {
      const message = error instanceof Error ? error.message : "request failed";
      const copyText =
        typeof error === "object" &&
        error !== null &&
        "copyText" in error &&
        typeof (error as ActionFailureError).copyText === "string"
          ? ((error as ActionFailureError).copyText ?? message)
          : message;
      showErrorToast(`操作に失敗しました: ${message}`, copyText);
    } finally {
      setIsSubmittingConfirm(false);
    }
  }, [
    confirmState,
    deleteConfirmed,
    isSubmittingConfirm,
    loadProcesses,
    rollbackReleaseConfirmed,
    runConfirmed,
    saveRuntimeConfigConfirmed,
    showErrorToast,
    showSuccessToast,
    stopConfirmed,
    yankReleaseConfirmed,
  ]);

  const updateTargetSelection = (capsuleId: string, target: string): void => {
    const normalized = target.trim();
    if (!normalized) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      draft.selectedTarget = normalized;
      if (!draft.targets[normalized]) {
        draft.targets[normalized] = { env: {} };
      }
    });
  };

  const updatePort = (
    capsuleId: string,
    target: string,
    value: string,
  ): void => {
    const normalizedTarget = target.trim();
    if (!normalizedTarget) {
      return;
    }
    const trimmed = value.trim();
    if (trimmed.length > 0 && !/^\d+$/.test(trimmed)) {
      return;
    }
    const parsed = trimmed.length === 0 ? null : parseNumberPort(trimmed);
    if (trimmed.length > 0 && !parsed) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      if (!draft.targets[normalizedTarget]) {
        draft.targets[normalizedTarget] = { env: {} };
      }
      if (parsed) {
        draft.targets[normalizedTarget].port = parsed;
      } else {
        delete draft.targets[normalizedTarget].port;
      }
    });
  };

  const updatePermissionMode = (
    capsuleId: string,
    target: string,
    value: RunPermissionMode,
  ): void => {
    const normalizedTarget = target.trim();
    if (!normalizedTarget) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      if (!draft.targets[normalizedTarget]) {
        draft.targets[normalizedTarget] = { env: {} };
      }
      if (value === "standard") {
        delete draft.targets[normalizedTarget].permissionMode;
      } else {
        draft.targets[normalizedTarget].permissionMode = value;
      }
    });
  };
  const updateEnv = (
    capsuleId: string,
    target: string,
    key: string,
    value: string,
  ): void => {
    const normalizedTarget = target.trim();
    const normalizedKey = key.trim();
    if (!normalizedTarget || !normalizedKey) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      if (!draft.targets[normalizedTarget]) {
        draft.targets[normalizedTarget] = { env: {} };
      }
      draft.targets[normalizedTarget].env[normalizedKey] = value;
    });
  };

  const addEnv = (
    capsuleId: string,
    target: string,
    key: string,
    value: string,
  ): void => {
    const normalizedTarget = target.trim();
    const normalizedKey = key.trim();
    if (!normalizedTarget || !normalizedKey) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      if (!draft.targets[normalizedTarget]) {
        draft.targets[normalizedTarget] = { env: {} };
      }
      draft.targets[normalizedTarget].env[normalizedKey] = value;
    });
  };

  const removeEnv = (capsuleId: string, target: string, key: string): void => {
    const normalizedTarget = target.trim();
    const normalizedKey = key.trim();
    if (!normalizedTarget || !normalizedKey) {
      return;
    }
    updateRuntimeConfig(capsuleId, (draft) => {
      const targetOverride = draft.targets[normalizedTarget];
      if (!targetOverride) {
        return;
      }
      delete targetOverride.env[normalizedKey];
      if (
        Object.keys(targetOverride.env).length === 0 &&
        !targetOverride.port &&
        !targetOverride.permissionMode
      ) {
        delete draft.targets[normalizedTarget];
      }
    });
  };

  const enrichCapsuleDetail = useCallback(
    async (capsule: Capsule): Promise<void> => {
      const parts = capsule.scopedId.split("/");
      if (parts.length < 2) {
        return;
      }
      const [publisher, slug] = parts;
      try {
        const response = await fetch(
          `/v1/manifest/capsules/by/${encodeURIComponent(publisher)}/${encodeURIComponent(slug)}`,
        );
        if (!response.ok) {
          return;
        }
        const detail = (await response.json()) as ApiDetailResponse;
        const { defaultTarget, targets } = parseTargets(detail.manifest);
        const osArchSet = new Set<string>();
        collectOsArchStrings(detail.manifest, osArchSet);
        const inferredOsArch = inferOsArchFromTargets(targets);
        const osArch =
          osArchSet.size > 0
            ? Array.from(osArchSet)
            : inferredOsArch.length > 0
              ? inferredOsArch
              : capsule.osArch;
        const firstTargetEnv = targets[0]?.env ?? {};
        const readmeFromApi = detail.readmeMarkdown ?? detail.readme_markdown;
        const readmeSource = detail.readmeSource ?? detail.readme_source;
        const readme =
          readmeFromApi ??
          baseReadme(
            capsule.scopedId,
            detail.description ?? capsule.description,
          );
        const version =
          detail.latestVersion ?? detail.latest_version ?? capsule.version;
        const releases = Array.isArray(detail.releases)
          ? detail.releases.map(mapReleaseRow)
          : capsule.releases;
        const manifestToml = detail.manifestToml ?? detail.manifest_toml;
        const storeMetadata = mapStoreMetadata(
          detail.storeMetadata ?? detail.store_metadata,
        );
        const description =
          storeMetadata?.text ?? detail.description ?? capsule.description;
        const runtimeConfig = runtimeOverrideFromApi(
          detail.runtimeConfig ?? detail.runtime_config,
        );
        setCatalogCapsules((prev) =>
          prev.map((entry) =>
            entry.id === capsule.id
              ? {
                  ...entry,
                  description,
                  longDescription: description,
                  version,
                  osArch,
                  readme,
                  readmeSource,
                  rawToml: manifestToml,
                  manifest: detail.manifest,
                  envHints:
                    Object.keys(firstTargetEnv).length > 0
                      ? firstTargetEnv
                      : entry.envHints,
                  targets,
                  releases,
                  defaultTarget,
                  detailLoaded: true,
                  storeMetadata,
                }
              : entry,
          ),
        );
        setRuntimeOverrides((prev) => {
          const next = { ...prev };
          if (runtimeConfig) {
            next[capsule.id] = runtimeConfig;
          } else {
            delete next[capsule.id];
          }
          return next;
        });
        setDirtyRuntimeConfigCapsules((prev) => {
          const next = { ...prev };
          delete next[capsule.id];
          return next;
        });
      } catch {
        // keep search payload as fallback
      }
    },
    [],
  );

  const saveStoreMetadata = useCallback(
    async (capsule: Capsule, iconPath: string, text: string): Promise<void> => {
      const parsed = parseScopedId(capsule.scopedId);
      if (!parsed) {
        throw new Error("invalid scoped id");
      }
      const body: Record<string, unknown> = {
        confirmed: true,
      };
      const trimmedPath = iconPath.trim();
      const trimmedText = text.trim();
      if (trimmedPath.length > 0) {
        body.icon_path = trimmedPath;
      }
      if (trimmedText.length > 0) {
        body.text = trimmedText;
      }
      const response = await fetch(
        `/v1/local/capsules/by/${encodeURIComponent(parsed.publisher)}/${encodeURIComponent(parsed.slug)}/store-metadata`,
        {
          method: "PUT",
          headers: createWriteHeaders("application/json"),
          body: JSON.stringify(body),
        },
      );
      if (!response.ok) {
        if (response.status === 401) {
          setWriteAuthRequired(true);
        }
        const raw = await response.text();
        throw new Error(
          raw || `store metadata update failed: ${response.status}`,
        );
      }
      const payload = (await response.json()) as ApiStoreMetadataResponse;
      const metadata = mapStoreMetadata(
        payload.storeMetadata ?? payload.store_metadata,
      );
      const nextDescription = metadata?.text ?? capsule.description;
      setCatalogCapsules((prev) =>
        prev.map((entry) =>
          entry.id === capsule.id
            ? {
                ...entry,
                description: nextDescription,
                longDescription: nextDescription,
                storeMetadata: metadata,
              }
            : entry,
        ),
      );
      await enrichCapsuleDetail({
        ...capsule,
        detailLoaded: false,
        description: nextDescription,
        longDescription: nextDescription,
        storeMetadata: metadata,
      });
      showSuccessToast("Store metadataを更新しました");
    },
    [createWriteHeaders, enrichCapsuleDetail, showSuccessToast],
  );

  useEffect(() => {
    if (route.kind !== "detail") {
      return;
    }
    const capsule = catalogCapsules.find(
      (entry) => entry.id === route.capsuleId,
    );
    if (!capsule || capsule.detailLoaded) {
      return;
    }
    void enrichCapsuleDetail(capsule);
  }, [catalogCapsules, enrichCapsuleDetail, route]);

  useEffect(() => {
    if (isMobileViewport) {
      setMobileSidebarOpen(false);
    }
  }, [isMobileViewport, route]);

  useEffect(() => {
    if (!confirmState) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape" && !isSubmittingConfirm) {
        setConfirmState(null);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [confirmState, isSubmittingConfirm]);

  let content: JSX.Element;

  if (route.kind === "logs") {
    const process = processes.find((entry) => entry.id === route.processId);
    const logs: ProcessLogLine[] = logsByProcessId[route.processId] ?? [];
    content = (
      <LogsPage
        key={`${route.processId}-${logs.length}`}
        scopedId={process?.scopedId ?? route.processId}
        pid={process?.pid ?? 0}
        startedAt={process?.startedAt ?? new Date().toISOString()}
        logs={logs}
        onBack={() => window.history.back()}
        onClear={() => {
          if (!process) {
            return;
          }
          void clearProcessLogs(process.id);
        }}
      />
    );
  } else if (route.kind === "detail") {
    const capsule = catalogCapsules.find(
      (entry) => entry.id === route.capsuleId,
    );
    if (!capsule) {
      if (isLoadingCapsules) {
        content = <div className="row-meta">Loading capsule...</div>;
      } else {
        navigate("/");
        content = <div />;
      }
    } else {
      const process = latestProcessForCapsule(processes, capsule.id);
      const target = resolveSelectedTarget(capsule);
      const envValues = resolveEnvValues(capsule, target);
      const baseEnvKeys = resolveBaseEnvKeys(capsule, target);
      const requiredEnvKeys = resolveRequiredEnvKeys(capsule, target);
      const selectedPort = resolvePortValue(capsule, target);
      const selectedPermissionMode = resolvePermissionMode(capsule, target);
      content = (
        <DetailPage
          capsule={capsule}
          isMobile={isMobileViewport}
          process={process}
          selectedTarget={target}
          selectedPort={selectedPort}
          selectedPermissionMode={selectedPermissionMode}
          canRun={capsule.targets.length > 0 || !capsule.detailLoaded}
          hasRuntimeConfigChanges={Boolean(
            dirtyRuntimeConfigCapsules[capsule.id],
          )}
          isSavingRuntimeConfig={
            isSubmittingConfirm &&
            confirmState?.kind === "save-config" &&
            confirmState.capsule.id === capsule.id
          }
          envValues={envValues}
          baseEnvKeys={baseEnvKeys}
          requiredEnvKeys={requiredEnvKeys}
          storeMetadataIconPath={capsule.storeMetadata?.iconPath ?? ""}
          storeMetadataText={capsule.storeMetadata?.text ?? ""}
          logs={process ? (logsByProcessId[process.id] ?? []) : []}
          onBack={() => navigate("/")}
          onRun={requestRun}
          onStop={requestStop}
          onOpen={openRunningTarget}
          openReady={isOpenReady(process)}
          onDelete={requestDelete}
          onRollbackRelease={requestRollbackRelease}
          onYankRelease={requestYankRelease}
          onSaveStoreMetadata={saveStoreMetadata}
          onClearLogs={() => {
            if (!process) {
              return;
            }
            void clearProcessLogs(process.id);
          }}
          onEnvChange={updateEnv}
          onEnvAdd={addEnv}
          onEnvRemove={removeEnv}
          onTargetChange={updateTargetSelection}
          onPortChange={updatePort}
          onPermissionModeChange={updatePermissionMode}
          onSaveRuntimeConfig={requestSaveRuntimeConfig}
        />
      );
    }
  } else {
    content = (
      <CatalogPage
        capsules={visibleCapsules}
        processes={processes}
        platform={platform}
        isMobile={isMobileViewport}
        viewMode={viewMode}
        filter={filter}
        onFilterChange={setFilter}
        onViewModeChange={setViewMode}
        onRun={requestRun}
        onStop={requestStop}
        onOpen={openRunningTarget}
        isOpenReady={isOpenReady}
        onDelete={requestDelete}
        onInspect={(capsule) =>
          navigate(`/capsule/${encodeURIComponent(capsule.id)}`)
        }
        publishCommand={`ato publish --registry ${window.location.origin} --artifact ./dist/my-app.capsule`}
        onCopyCommand={() => {
          navigator.clipboard
            .writeText(
              `ato publish --registry ${window.location.origin} --artifact ./dist/my-app.capsule`,
            )
            .then(() =>
              showSuccessToast(
                "コマンドをコピーしました — ターミナルで実行してください",
              ),
            )
            .catch(() =>
              showErrorToast(
                "コピーに失敗しました",
                "Failed to copy publish command",
              ),
            );
        }}
      />
    );
  }

  const confirmTitle = !confirmState
    ? ""
    : confirmState.kind === "run"
      ? "Run confirmation"
      : confirmState.kind === "stop"
        ? "Stop confirmation"
        : confirmState.kind === "rollback-release"
          ? "Rollback confirmation"
          : confirmState.kind === "yank-release"
            ? "Yank confirmation"
            : confirmState.kind === "save-config"
              ? "Save configuration"
              : "Delete confirmation";

  const confirmLines = !confirmState
    ? []
    : confirmState.kind === "run"
      ? [
          `Capsule: ${confirmState.capsule.scopedId}`,
          `Target: ${confirmState.target || "(default)"} (${confirmState.runtime}${
            confirmState.driver ? `/${confirmState.driver}` : ""
          })`,
          `Port: ${confirmState.port ?? "-"}`,
          `Env keys: ${Object.keys(confirmState.env).length}`,
          `Permissions: ${confirmState.permissionMode}`,
          `Command: ato run ${confirmState.capsule.scopedId} --registry ${window.location.origin} --yes${
            confirmState.target ? ` --target ${confirmState.target}` : ""
          }${
            confirmState.permissionMode === "sandbox"
              ? " --sandbox"
              : confirmState.permissionMode === "dangerous"
                ? " --dangerously-skip-permissions"
                : ""
          }`,
        ]
      : confirmState.kind === "stop"
        ? [
            `Process: ${confirmState.process.id}`,
            `Capsule: ${confirmState.process.scopedId}`,
            `PID: ${confirmState.process.pid}`,
          ]
        : confirmState.kind === "rollback-release"
          ? [
              `Capsule: ${confirmState.capsule.scopedId}`,
              `Action: Roll back registry pointer`,
              `Version: ${confirmState.release.version}`,
              `Manifest hash: ${confirmState.release.manifestHash ?? "-"}`,
              `Content hash: ${confirmState.release.contentHash}`,
              `Signature: ${confirmState.release.signatureStatus}`,
            ]
          : confirmState.kind === "yank-release"
            ? [
                `Capsule: ${confirmState.capsule.scopedId}`,
                `Action: Yank release from resolution history`,
                `Version: ${confirmState.release.version}`,
                `Manifest hash: ${confirmState.release.manifestHash ?? "-"}`,
                `Content hash: ${confirmState.release.contentHash}`,
                `Signature: ${confirmState.release.signatureStatus}`,
              ]
            : confirmState.kind === "save-config"
              ? [
                  `Capsule: ${confirmState.capsule.scopedId}`,
                  `Selected target: ${confirmState.runtimeConfig?.selectedTarget ?? "-"}`,
                  `Configured targets: ${Object.keys(confirmState.runtimeConfig?.targets ?? {}).length}`,
                ]
              : [
                  `Capsule: ${confirmState.capsule.scopedId}`,
                  "Action: Delete from local registry",
                  "Scope: All published versions",
                ];

  const confirmExtraContent =
    confirmState?.kind === "run" && confirmState.requiresPermissionGrant ? (
      <div className="confirm-extra">
        <span className="confirm-auth-label">Execution permissions</span>
        <PermissionModeSelector
          name="confirm-run-permission-mode"
          value={confirmState.permissionMode}
          disabled={isSubmittingConfirm}
          onChange={(next) => {
            setConfirmState((prev) =>
              prev && prev.kind === "run"
                ? { ...prev, permissionMode: next }
                : prev,
            );
          }}
        />
        <p
          className={`confirm-note ${
            getPermissionModeMessage(confirmState.permissionMode).tone === "warn"
              ? "warn"
              : getPermissionModeMessage(confirmState.permissionMode).tone === "error"
                ? "error"
                : ""
          }`}
        >
          {getPermissionModeMessage(confirmState.permissionMode).text}
        </p>
      </div>
    ) : null;
  const confirmAuthRequired = Boolean(
    confirmState && (writeAuthRequired || confirmState.kind === "save-config"),
  );
  const confirmDisabled = Boolean(
    isSubmittingConfirm ||
    (confirmAuthRequired && !registryAuthToken.trim()) ||
    (confirmState?.kind === "run" &&
      confirmState.requiresPermissionGrant &&
      confirmState.permissionMode === "standard"),
  );

  return (
    <>
      <div className="app-shell">
        <Sidebar
          processes={processes}
          isMobile={isMobileViewport}
          mobileOpen={mobileSidebarOpen}
          onCloseMobile={() => setMobileSidebarOpen(false)}
          onOpenProcesses={() => setDrawerOpen(true)}
        />
        <main className="main-pane">
          {isMobileViewport ? (
            <div className="mobile-nav-bar">
              <button
                className="icon-btn mobile-menu-btn"
                type="button"
                aria-label="Open navigation menu"
                onClick={() => setMobileSidebarOpen(true)}
              >
                <Menu size={15} strokeWidth={1.5} />
              </button>
            </div>
          ) : null}
          <div className="content-pane">{content}</div>
        </main>

        <ProcessDrawer
          open={drawerOpen}
          processes={[...processes].sort((left, right) =>
            right.startedAt.localeCompare(left.startedAt),
          )}
          onClose={() => setDrawerOpen(false)}
          onOpenLogs={openLogs}
          onStop={(process) => setConfirmState({ kind: "stop", process })}
        />

        {toast ? <Toast toast={toast} onClose={() => setToast(null)} /> : null}
      </div>

      <ConfirmActionModal
        open={Boolean(confirmState)}
        title={confirmTitle}
        lines={confirmLines}
        authRequired={confirmAuthRequired}
        authToken={registryAuthToken}
        isSubmitting={isSubmittingConfirm}
        confirmLabel={
          !confirmState
            ? "Confirm"
            : confirmState.kind === "delete"
              ? "Delete"
              : confirmState.kind === "rollback-release"
                ? "Rollback"
                : confirmState.kind === "yank-release"
                  ? "Yank"
                  : "Confirm"
        }
        onAuthTokenChange={updateRegistryAuthToken}
        extraContent={confirmExtraContent}
        onClose={() => setConfirmState(null)}
        onConfirm={() => {
          void confirmAction();
        }}
        confirmDisabled={confirmDisabled}
      />
    </>
  );
}
