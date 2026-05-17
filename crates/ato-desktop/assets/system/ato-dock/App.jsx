import { useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  Copy,
  Download,
  ExternalLink,
  FileText,
  Github,
  Lock,
  MoreHorizontal,
  Package,
  PencilLine,
  Plus,
  Search,
  SlidersHorizontal,
  Trash2,
  Upload,
  UserRound,
  X,
} from "lucide-react";

import { postDockCommand } from "./src/bridge.js";

const CURRENT_IDENTITY = typeof window !== "undefined" ? window.__ATO_IDENTITY ?? null : null;
const CURRENT_BOOTSTRAP = typeof window !== "undefined" ? window.__ATO_DOCK_BOOTSTRAP ?? {} : {};
const CURRENT_USER_ID = CURRENT_IDENTITY?.user_id ?? "user-001";
const CURRENT_USER_NAME = CURRENT_IDENTITY?.name ?? "Koh0920";
const CURRENT_USER_EMAIL = CURRENT_IDENTITY?.email ?? "koh0920@example.com";
const CURRENT_USER_HANDLE = CURRENT_IDENTITY?.github ?? "Koh0920";

const FILTERS = [
  { id: "all", label: "All" },
  { id: "needs-action", label: "Needs action" },
  { id: "published", label: "Published" },
  { id: "drafts", label: "Drafts" },
];

const INPUT_CLASS =
  "h-10 w-full rounded-lg border border-gray-200 bg-white px-3 text-sm text-gray-900 outline-none transition-all duration-150 placeholder:text-gray-400 focus:border-blue-500 focus:ring-2 focus:ring-blue-500/10";

const TEXTAREA_CLASS =
  "min-h-[120px] w-full rounded-lg border border-gray-200 bg-white px-3 py-2.5 text-sm text-gray-900 outline-none transition-all duration-150 placeholder:text-gray-400 focus:border-blue-500 focus:ring-2 focus:ring-blue-500/10";

function cn(...parts) {
  return parts.filter(Boolean).join(" ");
}

function slugify(value) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40) || "new-capsule";
}

function iconTextFrom(value) {
  const first = value?.trim()?.[0];
  return (first ? first.toUpperCase() : "A").slice(0, 2);
}

function createCapsule(seed) {
  return {
    id: seed.id,
    name: seed.name,
    owner: seed.owner,
    ownerId: seed.ownerId,
    sourceUrl: seed.sourceUrl,
    iconText: seed.iconText ?? iconTextFrom(seed.name),
    version: seed.version ?? "0.1.0",
    updatedAt: seed.updatedAt ?? "Updated just now",
    lifecycleStatus: seed.lifecycleStatus ?? "draft",
    publicLinkStatus: seed.publicLinkStatus ?? "disabled",
    storeStatus: seed.storeStatus ?? "hidden",
    tagline: seed.tagline ?? "",
    description: seed.description ?? "",
    category: seed.category ?? "",
    screenshots: seed.screenshots ?? [],
    publicUrl: seed.publicUrl ?? "",
    verification: seed.verification ?? { verifiedAt: null, lastResult: "unknown" },
    latestRelease: seed.latestRelease ?? null,
  };
}

function normalizeCapsule(capsule) {
  const listingInfoComplete = Boolean(
    capsule.tagline.trim() && capsule.description.trim() && capsule.category.trim(),
  );
  const distributionConfigured =
    capsule.publicLinkStatus === "active" || capsule.storeStatus !== "hidden";
  const submitted = ["in_review", "listed"].includes(capsule.storeStatus);
  const installType = capsule.publicLinkStatus === "active"
    ? capsule.storeStatus === "listed"
      ? "public_install"
      : "limited_link"
    : "private";

  return {
    ...capsule,
    installCommand: `ato install ${capsule.sourceUrl}`,
    installType,
    publishing: {
      listingInfoComplete,
      distributionConfigured,
      submitted,
    },
    needsAction:
      capsule.lifecycleStatus === "draft" ||
      capsule.storeStatus === "ready" ||
      capsule.storeStatus === "rejected" ||
      capsule.verification.lastResult !== "passed" ||
      !listingInfoComplete ||
      !distributionConfigured,
  };
}

function createDraftCapsule(mode) {
  const name = mode === "import" ? "imported-capsule" : "new-capsule";
  const sourceUrl = mode === "import"
    ? "github.com/owner/imported-capsule"
    : `github.com/${CURRENT_USER_HANDLE}/${slugify(name)}`;

  return createCapsule({
    id: `draft-${Date.now()}`,
    name,
    owner: CURRENT_USER_NAME,
    ownerId: CURRENT_USER_ID,
    sourceUrl,
    iconText: iconTextFrom(name),
    version: "0.1.0",
    updatedAt: "Updated just now",
    lifecycleStatus: "draft",
    publicLinkStatus: "disabled",
    storeStatus: "hidden",
    tagline: "",
    description: "",
    category: "",
    screenshots: [],
    publicUrl: "",
    verification: { verifiedAt: null, lastResult: "unknown" },
    latestRelease: {
      version: "0.1.0",
      releasedAt: "",
      notes: [],
    },
  });
}

function toCoreCapsule(capsule) {
  return {
    id: capsule.id,
    name: capsule.name,
    owner: capsule.owner,
    ownerId: capsule.ownerId,
    sourceUrl: capsule.sourceUrl,
    iconText: capsule.iconText,
    version: capsule.version,
    updatedAt: capsule.updatedAt,
    lifecycleStatus: capsule.lifecycleStatus,
    publicLinkStatus: capsule.publicLinkStatus,
    storeStatus: capsule.storeStatus,
    tagline: capsule.tagline,
    description: capsule.description,
    category: capsule.category,
    screenshots: [...capsule.screenshots],
    publicUrl: capsule.publicUrl,
    verification: { ...capsule.verification },
    latestRelease: capsule.latestRelease
      ? { ...capsule.latestRelease, notes: [...capsule.latestRelease.notes] }
      : null,
  };
}

function getLifecycleBadge(capsule) {
  if (capsule.lifecycleStatus === "published") {
    return { label: "Published", color: "text-emerald-600", dot: "bg-emerald-500" };
  }
  if (capsule.lifecycleStatus === "failed") {
    return { label: "Failed", color: "text-red-500", dot: "bg-red-400" };
  }
  return { label: "Draft", color: "text-amber-600", dot: "bg-amber-400" };
}

function sourceOwner(capsule) {
  if (capsule.sourceUrl.includes("github.com")) {
    const parts = capsule.sourceUrl.replace(/^https?:\/\//, "").split("/");
    return parts[1] ?? capsule.owner;
  }
  return "Local";
}

function getMissingSubmissionItems(capsule) {
  const missing = [];
  if (!capsule.verification.verifiedAt) {
    missing.push("Run & verify");
  }
  if (!capsule.publishing.listingInfoComplete) {
    missing.push("Listing info");
  }
  if (!capsule.publishing.distributionConfigured) {
    missing.push("Distribution");
  }
  if (capsule.storeStatus === "rejected") {
    missing.push("Resolve review feedback");
  }
  return missing;
}

function createRequestId(prefix) {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `${prefix}-${crypto.randomUUID().slice(0, 8)}`;
  }
  return `${prefix}-${Date.now()}`;
}

function initialSelectedId() {
  if (typeof window === "undefined") {
    return null;
  }
  return new URLSearchParams(window.location.search).get("capsule");
}

const INITIAL_CAPSULES = [
  createCapsule({
    id: "hello-capsule",
    name: "hello-capsule",
    owner: CURRENT_USER_NAME,
    ownerId: CURRENT_USER_ID,
    sourceUrl: "github.com/Koh0920/hello-capsule",
    iconText: "H",
    version: "0.4.2",
    updatedAt: "Updated just now",
    lifecycleStatus: "published",
    publicLinkStatus: "active",
    storeStatus: "listed",
    tagline: "A tiny example capsule to say hello.",
    description:
      "This is a tiny example capsule designed to help you learn how capsules work. It simply outputs a greeting message to the console or the preview window. Use it as a starting point for building your own custom capsules.",
    category: "Utilities",
    screenshots: [],
    publicUrl: "https://desktop.ato.run/capsules/hello-capsule",
    verification: { verifiedAt: "2026-05-16T09:20:00Z", lastResult: "passed" },
    latestRelease: {
      version: "0.4.2",
      releasedAt: "2026-05-16T09:20:00Z",
      notes: [
        "Polished the onboarding copy.",
        "Tightened preview spacing and install command presentation.",
      ],
    },
  }),
  createCapsule({
    id: "atlas-dock",
    name: "atlas-dock",
    owner: CURRENT_USER_NAME,
    ownerId: CURRENT_USER_ID,
    sourceUrl: "github.com/Koh0920/atlas-dock",
    iconText: "A",
    version: "0.2.0",
    updatedAt: "Updated 2h ago",
    lifecycleStatus: "published",
    publicLinkStatus: "active",
    storeStatus: "ready",
    tagline: "A command-dense capsule for source previews.",
    description:
      "Preview how source capsules read, feel, and publish with a stable store-detail layout.",
    category: "Developer tools",
    screenshots: ["terminal-preheat", "listing-overview"],
    publicUrl: "https://desktop.ato.run/capsules/atlas-dock",
    verification: { verifiedAt: "2026-05-15T18:45:00Z", lastResult: "passed" },
    latestRelease: {
      version: "0.2.0",
      releasedAt: "2026-05-15T18:45:00Z",
      notes: [
        "Added store-detail hero preview and actions panel.",
        "Improved badge summaries for draft and published states.",
      ],
    },
  }),
  createCapsule({
    id: "stack-notes",
    name: "stack-notes",
    owner: CURRENT_USER_NAME,
    ownerId: CURRENT_USER_ID,
    sourceUrl: "github.com/Koh0920/stack-notes",
    iconText: "S",
    version: "0.1.0",
    updatedAt: "Updated yesterday",
    lifecycleStatus: "draft",
    publicLinkStatus: "disabled",
    storeStatus: "hidden",
    tagline: "",
    description: "An unfinished note capsule with a lot left to polish.",
    category: "Productivity",
    screenshots: [],
    publicUrl: "",
    verification: { verifiedAt: null, lastResult: "unknown" },
    latestRelease: {
      version: "0.1.0",
      releasedAt: "",
      notes: [],
    },
  }),
  createCapsule({
    id: "pulse-kit",
    name: "pulse-kit",
    owner: CURRENT_USER_NAME,
    ownerId: CURRENT_USER_ID,
    sourceUrl: "/Users/koh0920/dev/pulse-kit",
    iconText: "P",
    version: "0.3.1",
    updatedAt: "Updated 3 days ago",
    lifecycleStatus: "published",
    publicLinkStatus: "disabled",
    storeStatus: "in_review",
    tagline: "A local-only capsule that still publishes cleanly.",
    description:
      "Used to verify local source handling, store review state, and the update listing flow.",
    category: "Developer tools",
    screenshots: ["local-shell"],
    publicUrl: "",
    verification: { verifiedAt: "2026-05-12T09:10:00Z", lastResult: "passed" },
    latestRelease: {
      version: "0.3.1",
      releasedAt: "2026-05-12T09:10:00Z",
      notes: ["Fixed local source handling and refreshed the preview card."],
    },
  }),
  createCapsule({
    id: "outsider-prototype",
    name: "outsider-prototype",
    owner: "Other Publisher",
    ownerId: "user-999",
    sourceUrl: "github.com/OtherPublisher/outsider-prototype",
    iconText: "O",
    version: "1.0.0",
    updatedAt: "Updated last week",
    lifecycleStatus: "published",
    publicLinkStatus: "active",
    storeStatus: "listed",
    tagline: "This one should never appear in the My Capsules list.",
    description: "An outsider capsule used to prove owner-scoped filtering.",
    category: "Design",
    screenshots: [],
    publicUrl: "https://desktop.ato.run/capsules/outsider-prototype",
    verification: { verifiedAt: "2026-05-10T11:30:00Z", lastResult: "passed" },
    latestRelease: {
      version: "1.0.0",
      releasedAt: "2026-05-10T11:30:00Z",
      notes: ["Public listing for another account."],
    },
  }),
];

function EditorField({ label, children, hint }) {
  return (
    <label className="block">
      <span className="mb-1.5 block text-xs font-semibold tracking-wide text-gray-500 uppercase">
        {label}
      </span>
      {children}
      {hint ? <span className="mt-1.5 block text-xs leading-5 text-gray-400">{hint}</span> : null}
    </label>
  );
}

function CapsuleEditorModal({ mode, draft, onChange, onClose, onSave }) {
  const title =
    mode === "import"
      ? "Import capsule"
      : mode === "new"
        ? "New capsule"
        : "Edit listing";
  const primary = mode === "edit" ? "Save listing" : mode === "import" ? "Import capsule" : "Create capsule";
  const sourceHint =
    mode === "edit"
      ? "Update the source repo or local path that this capsule is built from."
      : "Use a GitHub repo or a local path. The install command updates automatically.";

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-gray-950/40 px-4 py-8 backdrop-blur-[2px]">
      <div className="dock-modal-enter w-full max-w-4xl overflow-hidden rounded-xl border border-gray-200 bg-white shadow-[0_24px_80px_rgba(0,0,0,0.18)]">
        <div className="flex items-start justify-between border-b border-gray-200 px-6 py-5">
          <div>
            <h3 className="text-lg font-bold tracking-tight text-gray-900">{title}</h3>
            <p className="mt-1 text-sm text-gray-500">
              Keep the store detail page honest. Save a draft now and refine the listing later.
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600"
            aria-label="Close"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="grid gap-0 md:grid-cols-[minmax(0,1.2fr)_320px]">
          <div className="space-y-4 border-b border-gray-200 bg-gray-50/80 px-6 py-5 md:border-b-0 md:border-r md:border-gray-200">
            <div className="grid gap-4 sm:grid-cols-2">
              <EditorField label="Name">
                <input
                  className={INPUT_CLASS}
                  value={draft.name}
                  onChange={(event) => onChange({ ...draft, name: event.target.value, iconText: iconTextFrom(event.target.value) })}
                  placeholder="hello-capsule"
                />
              </EditorField>
              <EditorField label="Version">
                <input
                  className={INPUT_CLASS}
                  value={draft.version}
                  onChange={(event) => onChange({ ...draft, version: event.target.value })}
                  placeholder="0.1.0"
                />
              </EditorField>
            </div>

            <EditorField label="Source repo / local path" hint={sourceHint}>
              <input
                className={INPUT_CLASS}
                value={draft.sourceUrl}
                onChange={(event) => onChange({ ...draft, sourceUrl: event.target.value })}
                placeholder="github.com/owner/repo"
              />
            </EditorField>

            <div className="grid gap-4 sm:grid-cols-2">
              <EditorField label="Category">
                <input
                  className={INPUT_CLASS}
                  value={draft.category}
                  onChange={(event) => onChange({ ...draft, category: event.target.value })}
                  placeholder="Developer tools"
                />
              </EditorField>
              <EditorField label="Public URL">
                <input
                  className={INPUT_CLASS}
                  value={draft.publicUrl}
                  onChange={(event) => onChange({ ...draft, publicUrl: event.target.value })}
                  placeholder="https://desktop.ato.run/capsules/hello-capsule"
                />
              </EditorField>
            </div>

            <EditorField label="Tagline">
              <input
                className={INPUT_CLASS}
                value={draft.tagline}
                onChange={(event) => onChange({ ...draft, tagline: event.target.value })}
                placeholder="A tiny example capsule to say hello."
              />
            </EditorField>

            <EditorField label="Overview">
              <textarea
                className={TEXTAREA_CLASS}
                value={draft.description}
                onChange={(event) => onChange({ ...draft, description: event.target.value })}
                placeholder="Write a short overview that will appear on the store detail page."
              />
            </EditorField>
          </div>

          <div className="space-y-4 bg-white px-6 py-5">
            <EditorField label="Public link">
              <select
                className={INPUT_CLASS}
                value={draft.publicLinkStatus}
                onChange={(event) => onChange({ ...draft, publicLinkStatus: event.target.value })}
              >
                <option value="active">Enabled</option>
                <option value="disabled">Disabled</option>
              </select>
            </EditorField>

            <EditorField label="Store status">
              <select
                className={INPUT_CLASS}
                value={draft.storeStatus}
                onChange={(event) => onChange({ ...draft, storeStatus: event.target.value })}
              >
                <option value="hidden">Hidden</option>
                <option value="ready">Ready</option>
                <option value="in_review">In review</option>
                <option value="listed">Listed</option>
                <option value="rejected">Rejected</option>
              </select>
            </EditorField>

            <div className="rounded-lg border border-gray-200 bg-gray-50 p-4">
              <div className="flex items-center gap-2 text-sm font-medium text-gray-700">
                <FileText className="h-4 w-4 text-gray-400" />
                Listing note
              </div>
              <p className="mt-2 text-sm leading-6 text-gray-500">
                {mode === "edit"
                  ? "This form updates the store-facing copy and keeps the preview panel in sync."
                  : "Create a working draft first. You can refine listing info and distribution state later."}
              </p>
              <div className="mt-3 rounded-lg border border-gray-200 bg-white p-3">
                <div className="text-[11px] font-semibold tracking-wide text-gray-400 uppercase">
                  Install command
                </div>
                <div className="mt-1.5 font-mono text-[13px] text-gray-900">
                  {`ato install ${draft.sourceUrl || "github.com/owner/new-capsule"}`}
                </div>
              </div>
            </div>

            <div className="flex flex-wrap justify-end gap-2 pt-2">
              <button
                type="button"
                onClick={onClose}
                className="h-10 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={onSave}
                className="inline-flex h-10 items-center gap-2 rounded-lg bg-gray-900 px-4 text-sm font-semibold text-white transition-colors hover:bg-gray-700"
              >
                {mode === "edit" ? <PencilLine className="h-4 w-4" /> : mode === "import" ? <Download className="h-4 w-4" /> : <Plus className="h-4 w-4" />}
                {primary}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function SubmitModal({ capsule, missingItems, onClose, onSubmit }) {
  const canSubmit = missingItems.length === 0 && capsule.storeStatus !== "in_review";
  const label = capsule.storeStatus === "listed" ? "Update Store listing" : "Submit to Store";

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-gray-950/40 px-4 py-8 backdrop-blur-[2px]">
      <div className="dock-modal-enter w-full max-w-xl overflow-hidden rounded-xl border border-gray-200 bg-white shadow-[0_24px_80px_rgba(0,0,0,0.18)]">
        <div className="flex items-start justify-between border-b border-gray-200 px-6 py-5">
          <div>
            <h3 className="text-lg font-bold tracking-tight text-gray-900">
              {capsule.name}
            </h3>
            <p className="mt-1 text-sm text-gray-500">
              Review the missing items before you submit the capsule for Store review.
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600"
            aria-label="Close"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-4 px-6 py-5">
          <div className="rounded-lg border border-gray-200 bg-gray-50 p-4">
            <div className="text-sm font-semibold text-gray-900">Publishing checklist</div>
            <div className="mt-3 space-y-2">
              {[
                { label: "Run & verify", ok: Boolean(capsule.verification.verifiedAt) },
                { label: "Listing info", ok: capsule.publishing.listingInfoComplete },
                { label: "Distribution", ok: capsule.publishing.distributionConfigured },
                { label: "Submit", ok: capsule.storeStatus === "listed" || capsule.storeStatus === "in_review" },
              ].map((item) => (
                <div
                  key={item.label}
                  className="flex items-center justify-between rounded-lg border border-gray-200 bg-white px-4 py-2.5 text-sm"
                >
                  <div className="font-medium text-gray-700">{item.label}</div>
                  <div className={cn("flex items-center gap-1.5 text-xs font-semibold", item.ok ? "text-emerald-600" : "text-amber-600")}>
                    {item.ok ? <CheckCircle2 className="h-3.5 w-3.5" /> : <CircleAlert className="h-3.5 w-3.5" />}
                    {item.ok ? "Ready" : "Missing"}
                  </div>
                </div>
              ))}
            </div>
          </div>

          {missingItems.length > 0 ? (
            <div className="rounded-lg border border-amber-200 bg-amber-50 p-4 text-sm text-amber-900">
              <div className="flex items-center gap-2 font-semibold">
                <CircleAlert className="h-4 w-4" />
                Missing items
              </div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {missingItems.map((item) => (
                  <span key={item} className="inline-flex items-center rounded-full border border-amber-200 bg-white px-2.5 py-0.5 text-xs font-medium text-amber-700">
                    {item}
                  </span>
                ))}
              </div>
            </div>
          ) : (
            <div className="rounded-lg border border-emerald-200 bg-emerald-50 p-4 text-sm text-emerald-900">
              <div className="flex items-center gap-2 font-semibold">
                <CheckCircle2 className="h-4 w-4" />
                Everything is ready
              </div>
              <p className="mt-1 text-emerald-700">
                The capsule is ready to enter review. Submit when you are happy with the current copy.
              </p>
            </div>
          )}

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="h-10 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50"
            >
              Cancel
            </button>
            <button
              type="button"
              disabled={!canSubmit}
              onClick={onSubmit}
              className={cn(
                "inline-flex h-10 items-center gap-2 rounded-lg px-4 text-sm font-semibold transition-colors",
                canSubmit
                  ? "bg-gray-900 text-white hover:bg-gray-700"
                  : "cursor-not-allowed bg-gray-100 text-gray-400",
              )}
            >
              <Upload className="h-4 w-4" />
              {label}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function Toast({ toast }) {
  if (!toast) {
    return null;
  }

  const toneMap = {
    info: "border-blue-300 bg-blue-50 text-blue-800",
    success: "border-emerald-300 bg-emerald-50 text-emerald-800",
    warning: "border-amber-300 bg-amber-50 text-amber-800",
    danger: "border-red-300 bg-red-50 text-red-800",
  };

  return (
    <div className="dock-toast-enter fixed bottom-5 left-5 z-50 max-w-sm">
      <div className={cn(
        "rounded-lg border px-4 py-2.5 text-sm font-medium shadow-[0_8px_30px_rgba(0,0,0,0.12)]",
        toneMap[toast.type] ?? toneMap.info,
      )}>
        {toast.message}
      </div>
    </div>
  );
}

function StepperStep({ title, subtitle, done, active, last }) {
  return (
    <div className={cn("relative flex", !last && "pb-7")}>
      <div className="relative flex flex-col items-center">
        <span
          className={cn(
            "flex h-6 w-6 shrink-0 items-center justify-center rounded-full ring-[3px] ring-white z-10 transition-colors duration-300",
            done
              ? "bg-emerald-500"
              : active
                ? "bg-blue-600"
                : "border-2 border-gray-300 bg-white",
          )}
        >
          {done ? (
            <svg className="h-3 w-3 text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" />
            </svg>
          ) : active ? (
            <span className="h-1.5 w-1.5 rounded-full bg-white" />
          ) : (
            <span className="h-1.5 w-1.5 rounded-full bg-gray-400" />
          )}
        </span>
        {!last && (
          <div className={cn(
            "absolute left-[11px] top-6 bottom-0 w-0.5 transition-colors duration-300",
            done ? "bg-emerald-500" : "bg-gray-200",
          )} />
        )}
      </div>
      <div className="ml-3">
        <div className={cn(
          "text-sm transition-colors duration-200",
          done ? "font-semibold text-gray-900" : active ? "font-semibold text-gray-900" : "font-medium text-gray-400",
        )}>
          {title}
        </div>
        {subtitle && (
          <div className="mt-0.5 text-xs text-gray-500">{subtitle}</div>
        )}
      </div>
    </div>
  );
}

function SidebarListItem({ capsule, selected, onSelect, index }) {
  const badge = getLifecycleBadge(capsule);

  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "group relative w-full border-l-[3px] px-4 py-3 text-left transition-all duration-150",
        selected
          ? "border-l-blue-600 bg-blue-50/80"
          : "border-l-transparent hover:bg-gray-50",
      )}
      style={{ animationDelay: `${index * 30}ms` }}
    >
      <div className="text-[13px] font-semibold text-gray-900 transition-colors group-hover:text-gray-950">{capsule.name}</div>
      <div className={cn("mt-1 flex items-center text-xs", badge.color)}>
        <span className={cn("mr-1.5 h-[6px] w-[6px] rounded-full", badge.dot)} />
        {badge.label}
      </div>
      <div className="mt-1 flex items-center text-xs text-gray-500">
        <Github className="mr-1 h-3 w-3 opacity-50" />
        <span className="truncate">{sourceOwner(capsule)}</span>
      </div>
    </button>
  );
}

function PreviewWindow({ capsule }) {
  return (
    <div className="overflow-hidden rounded-lg border border-gray-200 bg-white shadow-[0_1px_3px_rgba(0,0,0,0.08),0_4px_12px_rgba(0,0,0,0.04)]">
      <div className="flex h-8 items-center bg-gray-100/80 px-4 border-b border-gray-200">
        <span className="mr-1.5 h-[10px] w-[10px] rounded-full bg-[#FF5F57]" />
        <span className="mr-1.5 h-[10px] w-[10px] rounded-full bg-[#FFBD2E]" />
        <span className="h-[10px] w-[10px] rounded-full bg-[#28C840]" />
      </div>
      <div className="flex min-h-[200px] items-center justify-center bg-white p-6">
        <div className="text-center">
          <div className="text-base font-medium text-gray-800">
            {capsule.screenshots.length > 0 ? capsule.screenshots[0] : `Hello from ${capsule.name}!`}
          </div>
          {capsule.screenshots.length === 0 && (
            <div className="mt-2 text-sm text-gray-400">
              No screenshots yet
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function MyCapsulesPage() {
  const [capsules, setCapsules] = useState(() => INITIAL_CAPSULES);
  const [search, setSearch] = useState("");
  const [listSearch, setListSearch] = useState("");
  const [filter, setFilter] = useState("all");
  const [selectedCapsuleId, setSelectedCapsuleId] = useState(() => initialSelectedId());
  const [profileOpen, setProfileOpen] = useState(false);
  const [detailMenuOpen, setDetailMenuOpen] = useState(false);
  const [toast, setToast] = useState(null);
  const [modal, setModal] = useState(null);
  const [copied, setCopied] = useState(false);
  const prevCapsuleRef = useRef(null);
  const [contentKey, setContentKey] = useState(0);

  const normalized = useMemo(() => capsules.map(normalizeCapsule), [capsules]);
  const ownedCapsules = useMemo(
    () => normalized.filter((capsule) => capsule.ownerId === CURRENT_USER_ID),
    [normalized],
  );

  const filteredCapsules = useMemo(() => {
    const query = search.trim().toLowerCase();
    const listQuery = listSearch.trim().toLowerCase();
    return ownedCapsules.filter((capsule) => {
      const matchesQuery =
        !query ||
        [capsule.name, capsule.sourceUrl, capsule.tagline, capsule.description, capsule.category]
          .filter(Boolean)
          .some((value) => value.toLowerCase().includes(query));
      const matchesListQuery =
        !listQuery ||
        [capsule.name, capsule.sourceUrl]
          .filter(Boolean)
          .some((value) => value.toLowerCase().includes(listQuery));

      const matchesFilter =
        filter === "all"
          ? true
          : filter === "needs-action"
            ? capsule.needsAction
            : filter === "published"
              ? capsule.lifecycleStatus === "published"
              : capsule.lifecycleStatus === "draft";

      return matchesQuery && matchesListQuery && matchesFilter;
    });
  }, [ownedCapsules, search, listSearch, filter]);

  const selectedCapsule =
    filteredCapsules.find((capsule) => capsule.id === selectedCapsuleId) ?? filteredCapsules[0] ?? null;

  useEffect(() => {
    if (filteredCapsules.length === 0) {
      if (selectedCapsuleId !== null) {
        setSelectedCapsuleId(null);
      }
      return;
    }

    const selectedExists = filteredCapsules.some((capsule) => capsule.id === selectedCapsuleId);
    if (!selectedExists) {
      setSelectedCapsuleId(filteredCapsules[0].id);
    }
  }, [filteredCapsules, selectedCapsuleId]);

  useEffect(() => {
    if (selectedCapsule?.id !== prevCapsuleRef.current) {
      prevCapsuleRef.current = selectedCapsule?.id ?? null;
      setContentKey((k) => k + 1);
    }
  }, [selectedCapsule?.id]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }

    const url = new URL(window.location.href);
    if (selectedCapsuleId) {
      url.searchParams.set("capsule", selectedCapsuleId);
    } else {
      url.searchParams.delete("capsule");
    }
    window.history.replaceState({}, "", `${url.pathname}${url.search}${url.hash}`);
  }, [selectedCapsuleId]);

  useEffect(() => {
    const onPointerDown = (event) => {
      if (!(event.target instanceof Element)) {
        return;
      }
      if (!event.target.closest("[data-profile-menu]")) {
        setProfileOpen(false);
      }
      if (!event.target.closest("[data-detail-menu]")) {
        setDetailMenuOpen(false);
      }
    };

    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        setProfileOpen(false);
        setDetailMenuOpen(false);
        setModal(null);
      }
    };

    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, []);

  useEffect(() => {
    const handler = (event) => {
      if (!event || typeof event !== "object") {
        return;
      }
      const message =
        typeof event.message === "string"
          ? event.message
          : typeof event.kind === "string"
            ? event.kind
            : null;
      if (!message) {
        return;
      }
      setToast({
        type:
          typeof event.kind === "string" && event.kind.includes("failed")
            ? "warning"
            : "info",
        message,
      });
    };

    window.__ATO_DOCK_EVENT__ = handler;
    return () => {
      if (window.__ATO_DOCK_EVENT__ === handler) {
        delete window.__ATO_DOCK_EVENT__;
      }
    };
  }, []);

  useEffect(() => {
    if (!toast) {
      return undefined;
    }

    const timeout = window.setTimeout(() => setToast(null), 2800);
    return () => window.clearTimeout(timeout);
  }, [toast]);

  const selectedView = selectedCapsule;
  const missingSubmissionItems = selectedView ? getMissingSubmissionItems(selectedView) : [];

  const setEditorDraft = (nextDraft) => {
    setModal((current) => (current ? { ...current, draft: nextDraft } : current));
  };

  const openEditor = (mode, capsule) => {
    setModal({
      kind: "editor",
      mode,
      draft: capsule ? toCoreCapsule(capsule) : createDraftCapsule(mode),
    });
  };

  const openSubmit = (capsule) => {
    setModal({
      kind: "submit",
      capsule: toCoreCapsule(capsule),
    });
  };

  const closeModal = () => setModal(null);

  const persistCapsule = (nextCapsule, isNew = false) => {
    setCapsules((current) => {
      const next = isNew
        ? [nextCapsule, ...current]
        : current.map((capsule) => (capsule.id === nextCapsule.id ? nextCapsule : capsule));
      return next;
    });
    setSelectedCapsuleId(nextCapsule.id);
    setToast({
      type: "success",
      message: isNew ? "Capsule created" : "Listing saved",
    });
  };

  const copyInstallCommand = async (capsule = selectedView) => {
    if (!capsule) {
      return;
    }

    try {
      await navigator.clipboard.writeText(capsule.installCommand);
      setCopied(true);
      setToast({ type: "success", message: "Install command copied" });
      setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      setToast({ type: "warning", message: "Clipboard copy failed" });
      console.warn(error);
    }
  };

  const runDelete = (capsule) => {
    const confirmed = window.confirm(`Delete capsule "${capsule.name}"? This cannot be undone.`);
    if (!confirmed) {
      return;
    }

    setCapsules((current) => current.filter((item) => item.id !== capsule.id));
    setToast({ type: "danger", message: "Capsule deleted" });
  };

  const runUnpublish = (capsule) => {
    setCapsules((current) =>
      current.map((item) =>
        item.id === capsule.id
          ? {
              ...item,
              publicLinkStatus: "disabled",
              updatedAt: "Updated just now",
            }
          : item,
      ),
    );
    setToast({ type: "info", message: "Public link unpublished" });
  };

  const runSubmit = (capsule) => {
    const missing = getMissingSubmissionItems(capsule);
    if (missing.length > 0) {
      setToast({ type: "warning", message: "Complete listing info before submitting" });
      return;
    }

    setCapsules((current) =>
      current.map((item) =>
        item.id === capsule.id
          ? {
              ...item,
              storeStatus: item.storeStatus === "listed" ? "listed" : "in_review",
              lifecycleStatus: "published",
              updatedAt: "Updated just now",
              latestRelease: item.latestRelease
                ? {
                    ...item.latestRelease,
                    notes: [...item.latestRelease.notes, "Submitted to Store from the dock."],
                  }
                : {
                    version: item.version,
                    releasedAt: new Date().toISOString(),
                    notes: ["Submitted to Store from the dock."],
                  },
            }
          : item,
      ),
    );
    setToast({ type: "success", message: "Store submission queued" });
    setModal(null);
  };

  const handleLogin = () => {
    if (!postDockCommand({ kind: "login", request_id: createRequestId("login") })) {
      setToast({ type: "warning", message: "Sign-in bridge is unavailable" });
    } else {
      setToast({ type: "info", message: "Opening Ato sign-in" });
    }
  };

  const createOrSave = () => {
    if (!modal || modal.kind !== "editor") {
      return;
    }

    const next = {
      ...modal.draft,
      name: modal.draft.name.trim() || slugify(modal.draft.sourceUrl.split("/").pop() || "new-capsule"),
      owner: CURRENT_USER_NAME,
      ownerId: CURRENT_USER_ID,
      sourceUrl:
        modal.draft.sourceUrl.trim() ||
        `github.com/${CURRENT_USER_HANDLE}/${slugify(modal.draft.name || "new-capsule")}`,
      iconText: iconTextFrom(modal.draft.name || modal.draft.sourceUrl),
      version: modal.draft.version.trim() || "0.1.0",
      updatedAt: "Updated just now",
      publicUrl: modal.draft.publicUrl.trim(),
      verification: modal.draft.verification ?? { verifiedAt: null, lastResult: "unknown" },
      latestRelease: modal.draft.latestRelease ?? {
        version: modal.draft.version.trim() || "0.1.0",
        releasedAt: "",
        notes: [],
      },
    };

    persistCapsule(next, modal.mode !== "edit");
    setModal(null);
  };

  const submitLabel =
    selectedView?.storeStatus === "listed"
      ? "Update Store listing"
      : selectedView?.storeStatus === "in_review"
        ? "In review"
        : "Submit to Store";
  const submitDisabled = selectedView?.storeStatus === "in_review";

  return (
    <div className="flex h-screen overflow-hidden bg-[#F9FAFB] text-gray-900">
      {/* Left Sidebar */}
      <div className="flex w-[280px] shrink-0 flex-col border-r border-gray-200 bg-white">
        <div className="flex items-center justify-between px-4 py-4">
          <h1 className="text-[18px] font-extrabold tracking-tight text-gray-900">
            <span className="text-blue-600">{CURRENT_USER_NAME.slice(0, CURRENT_USER_NAME.indexOf("0") > 0 ? CURRENT_USER_NAME.indexOf("0") : 3)}</span>&apos;s Capsules
          </h1>
          <div className="relative" data-profile-menu>
            <button
              type="button"
              onClick={() => setProfileOpen((v) => !v)}
              className="flex h-8 w-8 items-center justify-center rounded-full bg-gray-100 text-xs font-bold text-gray-600 transition-colors hover:bg-gray-200"
            >
              {iconTextFrom(CURRENT_USER_NAME)}
            </button>
            {profileOpen && (
              <div
                data-profile-menu
                className="dock-fade-in absolute right-0 top-10 z-30 w-56 rounded-lg border border-gray-200 bg-white py-1 shadow-[0_12px_40px_rgba(0,0,0,0.12)]"
              >
                <div className="border-b border-gray-100 px-3 py-2.5">
                  <div className="text-sm font-semibold text-gray-900">{CURRENT_USER_NAME}</div>
                  <div className="mt-0.5 text-xs text-gray-500">{CURRENT_USER_EMAIL}</div>
                  <div className="mt-0.5 text-xs text-gray-500">@{CURRENT_USER_HANDLE}</div>
                </div>
                <button
                  type="button"
                  onClick={handleLogin}
                  className="flex w-full items-center gap-2 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50"
                >
                  <UserRound className="h-4 w-4 text-gray-400" />
                  Sign in with Ato
                </button>
              </div>
            )}
          </div>
        </div>

        <div className="border-b border-gray-100 px-4 py-3">
          <div className="flex items-center gap-2">
            <div className="relative flex-1">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-gray-400" />
              <input
                className="h-8 w-full rounded-lg border border-gray-200 bg-white pl-8 pr-3 text-xs outline-none transition-all placeholder:text-gray-400 focus:border-blue-500 focus:ring-2 focus:ring-blue-500/10"
                value={listSearch}
                onChange={(event) => setListSearch(event.target.value)}
                placeholder="Filter..."
              />
            </div>
            <button
              type="button"
              onClick={() => setCapsules((current) => current.map((capsule) => ({ ...capsule })))}
              className="grid h-8 w-8 shrink-0 place-items-center rounded-lg border border-gray-200 bg-white text-gray-400 transition-colors hover:bg-gray-50 hover:text-gray-600"
            >
              <SlidersHorizontal className="h-3.5 w-3.5" />
            </button>
          </div>
          <div className="mt-2.5 flex flex-wrap gap-1.5">
            {FILTERS.map((chip) => (
              <button
                type="button"
                key={chip.id}
                onClick={() => setFilter(chip.id)}
                className={cn(
                  "rounded-full px-2.5 py-1 text-xs font-medium transition-all duration-150",
                  filter === chip.id
                    ? "bg-gray-900 text-white shadow-sm"
                    : "bg-gray-100 text-gray-600 hover:bg-gray-200",
                )}
              >
                {chip.label}
              </button>
            ))}
          </div>
        </div>

        <nav className="dock-scrollbar min-h-0 flex-1 overflow-y-auto">
          {filteredCapsules.length === 0 ? (
            <div className="dock-fade-in px-4 py-12 text-center">
              <div className="mx-auto grid h-12 w-12 place-items-center rounded-xl bg-gray-100">
                <Package className="h-5 w-5 text-gray-400" />
              </div>
              <p className="mt-3 text-sm font-semibold text-gray-600">No capsules yet.</p>
              <p className="mt-1 text-xs text-gray-400">Import a repo or create a new capsule.</p>
              <div className="mt-4 flex justify-center gap-2">
                <button
                  type="button"
                  onClick={() => openEditor("import")}
                  className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-gray-200 bg-white px-3 text-xs font-medium text-gray-700 transition-colors hover:bg-gray-50"
                >
                  <Download className="h-3.5 w-3.5" />
                  Import
                </button>
                <button
                  type="button"
                  onClick={() => openEditor("new")}
                  className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-gray-900 px-3 text-xs font-semibold text-white transition-colors hover:bg-gray-700"
                >
                  <Plus className="h-3.5 w-3.5" />
                  New
                </button>
              </div>
            </div>
          ) : (
            filteredCapsules.map((capsule, i) => (
              <SidebarListItem
                key={capsule.id}
                capsule={capsule}
                selected={capsule.id === selectedCapsule?.id}
                onSelect={() => setSelectedCapsuleId(capsule.id)}
                index={i}
              />
            ))
          )}
        </nav>

        <div className="flex items-center justify-between border-t border-gray-200 px-4 py-2.5">
          <span className="text-[11px] font-medium text-gray-400">{filteredCapsules.length} capsules</span>
          <div className="flex gap-1">
            <button
              type="button"
              onClick={() => openEditor("import")}
              className="inline-flex h-7 items-center gap-1 rounded-md border border-gray-200 bg-white px-2 text-[11px] font-medium text-gray-600 transition-colors hover:bg-gray-50"
            >
              <Download className="h-3 w-3" />
              Import
            </button>
            <button
              type="button"
              onClick={() => openEditor("new")}
              className="inline-flex h-7 items-center gap-1 rounded-md bg-gray-900 px-2 text-[11px] font-semibold text-white transition-colors hover:bg-gray-700"
            >
              <Plus className="h-3 w-3" />
              New
            </button>
          </div>
        </div>
      </div>

      {/* Center Content */}
      <main className="dock-scrollbar min-h-0 flex-1 overflow-y-auto">
        {selectedView ? (
          <div key={contentKey} className="dock-content-enter mx-auto max-w-3xl px-10 py-8">
            <div className="mb-8">
              <div className="flex items-start justify-between">
                <div>
                  <h2 className="text-2xl font-bold tracking-tight text-gray-900">{selectedView.name}</h2>
                  <p className="mt-2 text-sm leading-relaxed text-gray-500">
                    {selectedView.tagline || "No overview has been written yet."}
                  </p>
                  <div className="mt-3 flex items-center gap-2">
                    <Github className="h-3.5 w-3.5 text-gray-400" />
                    <span className="text-xs font-medium text-blue-600">{selectedView.sourceUrl}</span>
                    <span className="rounded-full border border-gray-200 bg-gray-50 px-2 py-0.5 text-[11px] font-semibold text-gray-500">
                      v{selectedView.version}
                    </span>
                  </div>
                </div>
                <div className="relative" data-detail-menu>
                  <button
                    type="button"
                    onClick={() => setDetailMenuOpen((v) => !v)}
                    className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600"
                  >
                    <MoreHorizontal className="h-4 w-4" />
                  </button>
                  {detailMenuOpen && (
                    <div className="dock-fade-in absolute right-0 top-10 z-20 w-52 rounded-lg border border-gray-200 bg-white py-1 shadow-[0_12px_40px_rgba(0,0,0,0.12)]">
                      <button
                        type="button"
                        onClick={() => { copyInstallCommand(selectedView); setDetailMenuOpen(false); }}
                        className="flex w-full items-center gap-2.5 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50"
                      >
                        <Copy className="h-4 w-4 text-gray-400" />
                        Copy install command
                      </button>
                      <a
                        href={selectedView.publicUrl || undefined}
                        target="_blank"
                        rel="noreferrer"
                        className={cn(
                          "flex items-center gap-2.5 px-3 py-2 text-sm transition-colors",
                          selectedView.publicUrl ? "text-gray-700 hover:bg-gray-50" : "pointer-events-none text-gray-300",
                        )}
                        onClick={() => setDetailMenuOpen(false)}
                      >
                        <ExternalLink className="h-4 w-4 text-gray-400" />
                        View public page
                      </a>
                      <button
                        type="button"
                        onClick={() => { openEditor("edit", selectedView); setDetailMenuOpen(false); }}
                        className="flex w-full items-center gap-2.5 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50"
                      >
                        <PencilLine className="h-4 w-4 text-gray-400" />
                        Edit listing
                      </button>
                      <button
                        type="button"
                        onClick={() => { openSubmit(selectedView); setDetailMenuOpen(false); }}
                        className="flex w-full items-center gap-2.5 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50"
                      >
                        <Upload className="h-4 w-4 text-gray-400" />
                        Submit to Store
                      </button>
                    </div>
                  )}
                </div>
              </div>
            </div>

            <div className="mb-8">
              <PreviewWindow capsule={selectedView} />
            </div>

            <div className="mb-8">
              <h3 className="text-base font-semibold text-gray-900 mb-2">Overview</h3>
              <p className="text-sm leading-7 text-gray-600">
                {selectedView.description || "No overview has been written yet."}
              </p>
            </div>

            <div className="mb-8">
              <h3 className="text-base font-semibold text-gray-900 mb-2">Install</h3>
              <div className="flex items-center justify-between rounded-lg bg-[#1F2937] px-4 py-3.5 shadow-[inset_0_1px_0_rgba(255,255,255,0.06)]">
                <span className="font-mono text-[13px] text-gray-100">{selectedView.installCommand}</span>
                <button
                  type="button"
                  onClick={() => copyInstallCommand(selectedView)}
                  className="ml-4 text-gray-500 transition-colors hover:text-gray-200"
                  aria-label="Copy install command"
                >
                  <Copy className="h-4 w-4" />
                </button>
              </div>
            </div>

            {selectedView.latestRelease?.notes?.length > 0 && (
              <div className="mb-8">
                <div className="flex items-center justify-between mb-2">
                  <h3 className="text-base font-semibold text-gray-900">
                    What&apos;s new in v{selectedView.latestRelease.version}
                  </h3>
                  <span className="rounded-full border border-gray-200 bg-gray-50 px-2 py-0.5 text-[11px] font-semibold text-gray-500">
                    Latest
                  </span>
                </div>
                <div className="space-y-1 text-sm leading-7 text-gray-600">
                  {selectedView.latestRelease.notes.map((note) => (
                    <p key={note}>&#8226; {note}</p>
                  ))}
                </div>
              </div>
            )}
          </div>
        ) : (
          <div className="grid min-h-full place-items-center p-10">
            <div className="dock-fade-in max-w-sm text-center">
              <div className="mx-auto grid h-14 w-14 place-items-center rounded-xl bg-gray-100">
                <Package className="h-6 w-6 text-gray-400" />
              </div>
              <h2 className="mt-5 text-lg font-bold text-gray-900">No capsule selected</h2>
              <p className="mt-2 text-sm text-gray-500">
                Select one of your capsules to inspect its store detail page and actions.
              </p>
            </div>
          </div>
        )}
      </main>

      {/* Right Sidebar */}
      <div className="flex w-[320px] shrink-0 flex-col border-l border-gray-200 bg-white">
        <div className="dock-scrollbar flex-1 overflow-y-auto px-6 py-8">
          <button
            type="button"
            disabled={!selectedView}
            onClick={() => copyInstallCommand(selectedView)}
            className={cn(
              "flex h-10 w-full items-center justify-center gap-2 rounded-lg text-sm font-semibold transition-all duration-200",
              copied
                ? "bg-emerald-600 text-white shadow-sm shadow-emerald-600/20"
                : selectedView
                  ? "bg-gray-900 text-white shadow-sm shadow-gray-900/10 hover:bg-gray-700"
                  : "cursor-not-allowed bg-gray-100 text-gray-400",
            )}
          >
            {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
            {copied ? "Copied!" : "Copy install command"}
          </button>

          <div className="mt-4 space-y-2">
            {selectedView?.publicUrl ? (
              <a
                href={selectedView.publicUrl}
                target="_blank"
                rel="noreferrer"
                className="flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50"
              >
                <ExternalLink className="h-4 w-4" />
                View public page
              </a>
            ) : (
              <button
                type="button"
                disabled
                className="flex h-10 w-full cursor-not-allowed items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium text-gray-300"
              >
                <ExternalLink className="h-4 w-4" />
                View public page
              </button>
            )}

            <button
              type="button"
              disabled={!selectedView}
              onClick={() => selectedView && openEditor("edit", selectedView)}
              className={cn(
                "flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium transition-colors",
                selectedView ? "text-gray-700 hover:bg-gray-50" : "cursor-not-allowed text-gray-300",
              )}
            >
              <PencilLine className="h-4 w-4" />
              Edit listing
            </button>

            <button
              type="button"
              disabled={!selectedView || submitDisabled}
              onClick={() => selectedView && openSubmit(selectedView)}
              className={cn(
                "flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium transition-colors",
                selectedView && !submitDisabled ? "text-gray-700 hover:bg-gray-50" : "cursor-not-allowed text-gray-300",
              )}
            >
              <Upload className="h-4 w-4" />
              {submitLabel}
            </button>
          </div>

          {selectedView && missingSubmissionItems.length > 0 && (
            <div className="mt-4 rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900">
              <div className="flex items-center gap-2 font-semibold">
                <CircleAlert className="h-4 w-4" />
                Missing items
              </div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {missingSubmissionItems.map((item) => (
                  <span key={item} className="inline-flex items-center rounded-full border border-amber-200 bg-white px-2 py-0.5 text-xs font-medium text-amber-700">
                    {item}
                  </span>
                ))}
              </div>
            </div>
          )}

          <div className="mt-8">
            <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-4">
              Publish Status
            </h4>
            {selectedView ? (
              <div className="space-y-0">
                <StepperStep
                  title="Run & verify"
                  subtitle={selectedView.verification.verifiedAt ? "Verification passed" : "Verification not run yet"}
                  done={Boolean(selectedView.verification.verifiedAt)}
                  active={!selectedView.verification.verifiedAt}
                  last={false}
                />
                <StepperStep
                  title="Listing info"
                  subtitle={selectedView.publishing.listingInfoComplete ? "Listing copy complete" : "Fill in tagline, overview, and category"}
                  done={selectedView.publishing.listingInfoComplete}
                  active={!selectedView.publishing.listingInfoComplete && Boolean(selectedView.verification.verifiedAt)}
                  last={false}
                />
                <StepperStep
                  title="Distribution"
                  subtitle={selectedView.publishing.distributionConfigured ? "Public link or store listing configured" : "Distribution is still hidden"}
                  done={selectedView.publishing.distributionConfigured}
                  active={!selectedView.publishing.distributionConfigured && selectedView.publishing.listingInfoComplete}
                  last={false}
                />
                <StepperStep
                  title="Submit"
                  subtitle={
                    selectedView.storeStatus === "in_review"
                      ? "In review"
                      : selectedView.storeStatus === "listed"
                        ? "Listed on the Store"
                        : selectedView.storeStatus === "ready"
                          ? "Ready to submit"
                          : "Not submitted yet"
                  }
                  done={["in_review", "listed"].includes(selectedView.storeStatus)}
                  active={selectedView.storeStatus === "ready"}
                  last
                />
              </div>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-gray-50 px-4 py-4 text-sm text-gray-400">
                Select a capsule to view publishing progress.
              </div>
            )}
          </div>

          <div className="mt-8 border-t border-gray-100 pt-6">
            <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-3">
              Details
            </h4>
            <div className="space-y-2.5 text-sm">
              <div className="flex justify-between">
                <span className="text-gray-500">Version</span>
                <span className="font-medium text-gray-900">{selectedView?.version ?? "\u2014"}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-gray-500">Category</span>
                <span className="font-medium text-gray-900">{selectedView?.category || "\u2014"}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-gray-500">Visibility</span>
                <span className="font-medium text-gray-900">
                  {selectedView
                    ? selectedView.publicLinkStatus === "active" && selectedView.storeStatus === "listed"
                      ? "Public"
                      : selectedView.publicLinkStatus === "active"
                        ? "Limited"
                        : "Private"
                    : "\u2014"}
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-gray-500">Last updated</span>
                <span className="font-medium text-gray-900">{selectedView?.updatedAt ?? "\u2014"}</span>
              </div>
            </div>
          </div>

          <div className="mt-8 border-t border-gray-100 pt-6">
            <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-3">
              Advanced
            </h4>
            <div className="space-y-2">
              <button
                type="button"
                disabled={!selectedView || selectedView.publicLinkStatus !== "active"}
                onClick={() => selectedView && runUnpublish(selectedView)}
                className={cn(
                  "flex h-9 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium transition-colors",
                  selectedView && selectedView.publicLinkStatus === "active"
                    ? "text-gray-700 hover:bg-gray-50"
                    : "cursor-not-allowed text-gray-300",
                )}
              >
                <Lock className="h-3.5 w-3.5" />
                Unpublish link
              </button>
              <button
                type="button"
                disabled={!selectedView}
                onClick={() => selectedView && runDelete(selectedView)}
                className={cn(
                  "flex h-9 w-full items-center justify-center gap-2 rounded-lg border text-sm font-medium transition-colors",
                  selectedView
                    ? "border-red-200 text-red-600 hover:bg-red-50"
                    : "cursor-not-allowed border-gray-200 text-gray-300",
                )}
              >
                <Trash2 className="h-3.5 w-3.5" />
                Delete capsule
              </button>
            </div>
          </div>
        </div>
      </div>

      <Toast toast={toast} />

      {modal?.kind === "editor" ? (
        <CapsuleEditorModal
          mode={modal.mode}
          draft={modal.draft}
          onChange={setEditorDraft}
          onClose={closeModal}
          onSave={createOrSave}
        />
      ) : null}

      {modal?.kind === "submit" ? (
        <SubmitModal
          capsule={modal.capsule}
          missingItems={getMissingSubmissionItems(modal.capsule)}
          onClose={closeModal}
          onSubmit={() => runSubmit(modal.capsule)}
        />
      ) : null}
    </div>
  );
}

export default function App() {
  return <MyCapsulesPage />;
}
