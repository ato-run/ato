export type TrustLevel = "verified" | "unverified" | "signed";

export type ProcessStatus =
  | "starting"
  | "ready"
  | "running"
  | "exited"
  | "failed"
  | "stopped"
  | "unknown";

export interface ProcessStatusMeta {
  label: string;
  tone: ProcessStatus;
  active: boolean;
}

export interface CapsuleRelease {
  version: string;
  manifestHash?: string;
  contentHash: string;
  signatureStatus: string;
  isCurrent: boolean;
  yankedAt?: string;
}

export function getProcessStatusMeta(status: ProcessStatus): ProcessStatusMeta {
  switch (status) {
    case "starting":
      return { label: "Starting", tone: "starting", active: true };
    case "ready":
      return { label: "Ready", tone: "ready", active: true };
    case "running":
      return { label: "Running", tone: "running", active: true };
    case "exited":
      return { label: "Exited", tone: "exited", active: false };
    case "failed":
      return { label: "Failed", tone: "failed", active: false };
    case "stopped":
      return { label: "Stopped", tone: "stopped", active: false };
    default:
      return { label: "Unknown", tone: "unknown", active: false };
  }
}

export interface StoreMetadata {
  iconPath?: string;
  text?: string;
  iconUrl?: string;
}

export type RunPermissionMode = "standard" | "sandbox" | "dangerous";

export interface CapsuleTarget {
  label: string;
  runtime: string;
  driver: string;
  port: number | null;
  env: Record<string, string>;
  requiredEnv: string[];
}

export interface Capsule {
  id: string;
  scopedId: string;
  name: string;
  publisher: string;
  iconKey: "globe" | "package" | "zap" | "box";
  description: string;
  longDescription?: string;
  type: "webapp" | "cli" | "service";
  version: string;
  size: string;
  osArch: string[];
  envHints: Record<string, string>;
  readme: string;
  readmeSource?: "artifact" | "none";
  rawToml?: string;
  manifest?: unknown;
  targets: CapsuleTarget[];
  releases: CapsuleRelease[];
  defaultTarget?: string;
  detailLoaded?: boolean;
  localPath: string;
  appUrl: string | null;
  trustLevel: TrustLevel;
  storeMetadata?: StoreMetadata;
}

export interface Process {
  id: string;
  name: string;
  pid: number;
  capsuleId: string;
  scopedId: string;
  active: boolean;
  status: ProcessStatus;
  startedAt: string;
  lastSeenAt: string;
  runtime: string;
  targetLabel?: string;
}

export type LogLevel = "INFO" | "WARN" | "ERROR" | "SIGTERM";

export interface ProcessLogLine {
  index: number;
  timestamp: string;
  level: LogLevel | string;
  message: string;
}

export type OsFilter = "all" | "macos" | "linux" | "windows";
export type CatalogViewMode = "list" | "grid";
