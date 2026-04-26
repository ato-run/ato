export function detectPlatform(): string {
  const platform = navigator.platform.toLowerCase();
  const ua = navigator.userAgent.toLowerCase();
  const arch = ua.includes("arm") || ua.includes("aarch64") ? "arm64" : "x64";
  if (platform.includes("mac")) {
    return `darwin/${arch}`;
  }
  if (platform.includes("win")) {
    return `windows/${arch}`;
  }
  return `linux/${arch}`;
}

export function toOsFilterLabel(osArch: string): "macos" | "linux" | "windows" | "other" {
  if (osArch.startsWith("darwin/")) {
    return "macos";
  }
  if (osArch.startsWith("windows/")) {
    return "windows";
  }
  if (osArch.startsWith("linux/")) {
    return "linux";
  }
  return "other";
}

export type PlatformBadgeFamily = "apple" | "linux" | "windows" | "other";

export interface PlatformBadgeMeta {
  family: PlatformBadgeFamily;
  label: string;
  arch: string;
}

function formatArchLabel(arch: string): string {
  const trimmed = arch.trim();
  const normalized = trimmed.toLowerCase();
  if (normalized === "arm64") {
    return "ARM64";
  }
  if (normalized === "x64") {
    return "x64";
  }
  return trimmed.toUpperCase();
}

export function getPlatformBadgeMeta(osArch: string): PlatformBadgeMeta {
  const [os, arch = ""] = osArch.split("/", 2);
  if (os === "darwin") {
    return { family: "apple", label: "Apple", arch: formatArchLabel(arch) };
  }
  if (os === "windows") {
    return { family: "windows", label: "Windows", arch: formatArchLabel(arch) };
  }
  if (os === "linux") {
    return { family: "linux", label: "Linux", arch: formatArchLabel(arch) };
  }
  return {
    family: "other",
    label: os.trim() || osArch,
    arch: arch ? formatArchLabel(arch) : "",
  };
}
