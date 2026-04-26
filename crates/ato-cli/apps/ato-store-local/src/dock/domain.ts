export type DockCapsuleType =
  | "app"
  | "webapp"
  | "cli"
  | "agent"
  | "webpage"
  | "service";

export type DockPublisherSummary = {
  handle: string;
  displayName?: string | null;
  verified?: boolean;
  avatarUrl?: string | null;
};

export type DockCatalogItem = {
  id: string;
  slug: string;
  scopedId: string;
  href?: string;
  title: string;
  description: string;
  publisher: DockPublisherSummary;
  version: string;
  downloads: number;
  vouchesCount: number;
  type: DockCapsuleType;
  category: string;
  tags: string[];
  trustBadge?: string | null;
  visibility?: string | null;
  coverImage?: string;
  iconImage?: string;
  updatedAt?: string;
  sizeLabel?: string;
  aclCount?: number;
  repository?: string | null;
  playgroundAvailable?: boolean;
};

export type DockRelease = {
  version: string;
  contentHash: string;
  signatureStatus: string;
  status?: string;
  createdAt?: string;
  sizeBytes?: number;
  releaseNotes?: string;
  sourceCommit?: string | null;
  builderIdentity?: string | null;
  manifestHash?: string;
  isCurrent?: boolean;
  yankedAt?: string;
};

export type DockCapsuleDetail = {
  id: string;
  slug: string;
  scopedId: string;
  title: string;
  description: string;
  publisher: DockPublisherSummary;
  version: string;
  downloads: number;
  vouchesCount: number;
  type: DockCapsuleType;
  category: string;
  tags: string[];
  trustBadge?: string | null;
  visibility?: string | null;
  coverImage?: string;
  iconImage?: string;
  updatedAt?: string;
  createdAt?: string;
  readmeMarkdown: string;
  readmeSource?: string | null;
  releases: DockRelease[];
  screenshots: string[];
  repository?: string | null;
  homepage?: string | null;
  licenseSpdx?: string | null;
  aclCount?: number;
  playgroundAvailable?: boolean;
};

export type LocalCatalogSourceItem = {
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
  publisher: {
    handle: string;
    verified?: boolean;
  };
  store_metadata?: {
    icon_path?: string;
    iconPath?: string;
    text?: string;
    icon_url?: string;
    iconUrl?: string;
  };
  storeMetadata?: {
    icon_path?: string;
    iconPath?: string;
    text?: string;
    icon_url?: string;
    iconUrl?: string;
  };
};

export type LocalDetailSource = {
  id?: string;
  description?: string;
  latestVersion?: string;
  latest_version?: string;
  releases?: Array<{
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
  }>;
  readmeMarkdown?: string;
  readme_markdown?: string;
  readmeSource?: string | null;
  readme_source?: string | null;
  store_metadata?: {
    icon_path?: string;
    iconPath?: string;
    text?: string;
    icon_url?: string;
    iconUrl?: string;
  };
  storeMetadata?: {
    icon_path?: string;
    iconPath?: string;
    text?: string;
    icon_url?: string;
    iconUrl?: string;
  };
  repository?: string | null;
  created_at?: string;
  updated_at?: string;
};

export const CATEGORY_TAGS: Record<string, string[]> = {
  ai: ["ai", "agent", "llm"],
  "dev-tool": ["dev", "tooling", "code"],
  productivity: ["workflow", "tasks", "automation"],
  media: ["audio", "media", "image"],
  other: ["general"],
};

function normalizeString(value: unknown, fallback = ""): string {
  return typeof value === "string" && value.trim().length > 0
    ? value.trim()
    : fallback;
}

function formatSizeLabel(sizeBytes: unknown): string | undefined {
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
    return undefined;
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

function buildScopedId(publisher: string, slug: string): string {
  return `${publisher}/${slug}`;
}

function normalizeCapsuleType(
  rawType: string | null | undefined,
  category: string | null | undefined,
): DockCapsuleType {
  const type = (rawType || "").toLowerCase();
  const normalizedCategory = (category || "").toLowerCase();

  if (type.includes("service")) return "service";
  if (type.includes("webapp")) return "webapp";
  if (type.includes("webpage") || type.includes("website")) return "webpage";
  if (type.includes("cli") || type.includes("command")) return "cli";
  if (type.includes("agent") || type.includes("skill")) return "agent";
  if (type.includes("app")) return "app";
  if (normalizedCategory.includes("agent") || normalizedCategory === "ai") {
    return "agent";
  }
  if (normalizedCategory.includes("cli")) return "cli";
  if (normalizedCategory.includes("web")) return "webapp";

  return "app";
}

export function normalizeLocalCatalogItem(
  item: LocalCatalogSourceItem,
  options: {
    href?: string;
    trustBadge?: string | null;
    visibility?: string | null;
  } = {},
): DockCatalogItem {
  const metadata = item.storeMetadata ?? item.store_metadata;
  const scopedId =
    normalizeString(item.scopedId) ||
    normalizeString(item.scoped_id) ||
    buildScopedId(item.publisher.handle, item.slug);
  const description =
    normalizeString(metadata?.text) ||
    normalizeString(item.description) ||
    "No description";

  return {
    id: item.id,
    slug: item.slug,
    scopedId,
    href: options.href,
    title: normalizeString(item.name, item.slug),
    description,
    publisher: {
      handle: item.publisher.handle,
      displayName: item.publisher.handle,
      verified: Boolean(item.publisher.verified),
      avatarUrl:
        normalizeString(metadata?.icon_url) ||
        normalizeString(metadata?.iconUrl) ||
        undefined,
    },
    version:
      normalizeString(item.latestVersion) ||
      normalizeString(item.latest_version) ||
      "0.0.0",
    downloads: 0,
    vouchesCount: 0,
    type: normalizeCapsuleType(item.type, item.category),
    category: normalizeString(item.category, "other"),
    tags: CATEGORY_TAGS[normalizeString(item.category, "other")] ?? [],
    trustBadge:
      options.trustBadge ??
      (item.publisher.verified ? "verified" : "unverified"),
    visibility: options.visibility ?? "local",
    iconImage:
      normalizeString(metadata?.icon_url) ||
      normalizeString(metadata?.iconUrl) ||
      undefined,
    sizeLabel: formatSizeLabel(
      item.latestSizeBytes ??
        item.latest_size_bytes ??
        item.sizeBytes ??
        item.size_bytes ??
        item.size,
    ),
  };
}

export function normalizeLocalDetail(
  detail: LocalDetailSource,
  options: {
    publisher: string;
    slug: string;
    scopedId?: string;
    title?: string;
    verified?: boolean;
    trustBadge?: string | null;
    visibility?: string | null;
    type?: string;
    category?: string;
    version?: string;
    description?: string;
    iconImage?: string | null;
  },
): DockCapsuleDetail {
  const metadata = detail.storeMetadata ?? detail.store_metadata;
  const scopedId =
    normalizeString(options.scopedId) ||
    buildScopedId(options.publisher, options.slug);
  const normalizedCategory = normalizeString(options.category, "other");
  const description =
    normalizeString(metadata?.text) ||
    normalizeString(options.description) ||
    normalizeString(detail.description) ||
    "No description";
  const readmeMarkdown =
    normalizeString(detail.readmeMarkdown) ||
    normalizeString(detail.readme_markdown) ||
    `# ${scopedId}\n\n${description}\n`;

  return {
    id: detail.id || scopedId,
    slug: options.slug,
    scopedId,
    title: normalizeString(options.title, options.slug),
    description,
    publisher: {
      handle: options.publisher,
      displayName: options.publisher,
      verified: options.verified,
      avatarUrl:
        normalizeString(metadata?.icon_url) ||
        normalizeString(metadata?.iconUrl) ||
        undefined,
    },
    version:
      normalizeString(options.version) ||
      normalizeString(detail.latestVersion) ||
      normalizeString(detail.latest_version) ||
      "0.0.0",
    downloads: 0,
    vouchesCount: 0,
    type: normalizeCapsuleType(options.type, normalizedCategory),
    category: normalizedCategory,
    tags: CATEGORY_TAGS[normalizedCategory] ?? CATEGORY_TAGS.other,
    trustBadge:
      options.trustBadge ?? (options.verified ? "verified" : "unverified"),
    visibility: options.visibility ?? "local",
    iconImage:
      normalizeString(options.iconImage) ||
      normalizeString(metadata?.icon_url) ||
      normalizeString(metadata?.iconUrl) ||
      undefined,
    updatedAt: detail.updated_at || detail.created_at,
    createdAt: detail.created_at,
    readmeMarkdown,
    readmeSource: detail.readmeSource ?? detail.readme_source ?? null,
    releases: Array.isArray(detail.releases)
      ? detail.releases.map((release) => ({
          version: release.version,
          contentHash:
            normalizeString(release.contentHash) ||
            normalizeString(release.content_hash) ||
            "-",
          signatureStatus:
            normalizeString(release.signatureStatus) ||
            normalizeString(release.signature_status) ||
            "unknown",
          manifestHash:
            normalizeString(release.manifestHash) ||
            normalizeString(release.manifest_hash) ||
            undefined,
          isCurrent: release.isCurrent ?? release.is_current ?? false,
          yankedAt: release.yankedAt ?? release.yanked_at,
        }))
      : [],
    screenshots: [],
    repository: detail.repository ?? null,
  };
}
