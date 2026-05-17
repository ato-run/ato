import { useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  CheckCircle2,
  CircleAlert,
  Copy,
  ChevronLeft,
  ChevronRight,
  Download,
  ExternalLink,
  FileText,
  Github,
  LayoutGrid,
  Lock,
  MoreHorizontal,
  Package,
  PencilLine,
  Plus,
  Rocket,
  Search,
  Trash2,
  Upload,
  UserRound,
  X,
} from "lucide-react";

import { postDockCommand, postImportOpen, looksLikeGitHubRepoUrl } from "./src/bridge.js";

const CURRENT_IDENTITY = typeof window !== "undefined" ? window.__ATO_IDENTITY ?? null : null;
const CURRENT_BOOTSTRAP = typeof window !== "undefined" ? window.__ATO_DOCK_BOOTSTRAP ?? {} : {};
const CURRENT_USER_ID = CURRENT_IDENTITY?.user_id ?? "user-001";
const CURRENT_USER_NAME = CURRENT_IDENTITY?.name ?? "Koh0920";
const CURRENT_USER_EMAIL = CURRENT_IDENTITY?.email ?? "koh0920@example.com";
const CURRENT_USER_HANDLE = CURRENT_IDENTITY?.github ?? "Koh0920";

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
  return (first ? first.toUpperCase() : "A").slice(0, 1);
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
    latestRelease: { version: "0.1.0", releasedAt: "", notes: [] },
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
  if (!capsule.verification.verifiedAt) missing.push("Run & verify");
  if (!capsule.publishing.listingInfoComplete) missing.push("Listing info");
  if (!capsule.publishing.distributionConfigured) missing.push("Distribution");
  if (capsule.storeStatus === "rejected") missing.push("Resolve review feedback");
  return missing;
}

function createRequestId(prefix) {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `${prefix}-${crypto.randomUUID().slice(0, 8)}`;
  }
  return `${prefix}-${Date.now()}`;
}

function initialSelectedId() {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("capsule");
}

const ICON_GRADIENTS = [
  "from-blue-600 to-indigo-600",
  "from-emerald-500 to-teal-600",
  "from-amber-500 to-orange-500",
  "from-rose-500 to-pink-500",
  "from-violet-500 to-purple-600",
];

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
      notes: ["Polished the onboarding copy.", "Tightened preview spacing and install command presentation."],
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
    description: "Preview how source capsules read, feel, and publish with a stable store-detail layout.",
    category: "Developer tools",
    screenshots: ["terminal-preheat", "listing-overview"],
    publicUrl: "https://desktop.ato.run/capsules/atlas-dock",
    verification: { verifiedAt: "2026-05-15T18:45:00Z", lastResult: "passed" },
    latestRelease: {
      version: "0.2.0",
      releasedAt: "2026-05-15T18:45:00Z",
      notes: ["Added store-detail hero preview and actions panel.", "Improved badge summaries for draft and published states."],
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
    latestRelease: { version: "0.1.0", releasedAt: "", notes: [] },
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
    description: "Used to verify local source handling, store review state, and the update listing flow.",
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
    latestRelease: { version: "1.0.0", releasedAt: "2026-05-10T11:30:00Z", notes: ["Public listing for another account."] },
  }),
];

function EditorField({ label, children, hint }) {
  return (
    <label className="block">
      <span className="mb-1.5 block text-xs font-semibold tracking-wide text-gray-500 uppercase">{label}</span>
      {children}
      {hint ? <span className="mt-1.5 block text-xs leading-5 text-gray-400">{hint}</span> : null}
    </label>
  );
}

function CapsuleEditorModal({ mode, draft, onChange, onClose, onSave }) {
  const title = mode === "import" ? "Import capsule" : mode === "new" ? "New capsule" : "Edit listing";
  const primary = mode === "edit" ? "Save listing" : mode === "import" ? "Import capsule" : "Create capsule";
  const sourceHint = mode === "edit"
    ? "Update the source repo or local path that this capsule is built from."
    : "Use a GitHub repo or a local path. The install command updates automatically.";

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-gray-950/40 px-4 py-8 backdrop-blur-[2px]">
      <div className="dock-modal-enter w-full max-w-4xl overflow-hidden rounded-xl border border-gray-200 bg-white shadow-[0_24px_80px_rgba(0,0,0,0.18)]">
        <div className="flex items-start justify-between border-b border-gray-200 px-6 py-5">
          <div>
            <h3 className="text-lg font-bold tracking-tight text-gray-900">{title}</h3>
            <p className="mt-1 text-sm text-gray-500">Keep the store detail page honest. Save a draft now and refine later.</p>
          </div>
          <button type="button" onClick={onClose} className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600" aria-label="Close">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="grid gap-0 md:grid-cols-[minmax(0,1.2fr)_320px]">
          <div className="space-y-4 border-b border-gray-200 bg-gray-50/80 px-6 py-5 md:border-b-0 md:border-r">
            <div className="grid gap-4 sm:grid-cols-2">
              <EditorField label="Name">
                <input className={INPUT_CLASS} value={draft.name} onChange={(e) => onChange({ ...draft, name: e.target.value, iconText: iconTextFrom(e.target.value) })} placeholder="hello-capsule" />
              </EditorField>
              <EditorField label="Version">
                <input className={INPUT_CLASS} value={draft.version} onChange={(e) => onChange({ ...draft, version: e.target.value })} placeholder="0.1.0" />
              </EditorField>
            </div>
            <EditorField label="Source repo / local path" hint={sourceHint}>
              <input className={INPUT_CLASS} value={draft.sourceUrl} onChange={(e) => onChange({ ...draft, sourceUrl: e.target.value })} placeholder="github.com/owner/repo" />
            </EditorField>
            <div className="grid gap-4 sm:grid-cols-2">
              <EditorField label="Category">
                <input className={INPUT_CLASS} value={draft.category} onChange={(e) => onChange({ ...draft, category: e.target.value })} placeholder="Developer tools" />
              </EditorField>
              <EditorField label="Public URL">
                <input className={INPUT_CLASS} value={draft.publicUrl} onChange={(e) => onChange({ ...draft, publicUrl: e.target.value })} placeholder="https://desktop.ato.run/capsules/hello-capsule" />
              </EditorField>
            </div>
            <EditorField label="Tagline">
              <input className={INPUT_CLASS} value={draft.tagline} onChange={(e) => onChange({ ...draft, tagline: e.target.value })} placeholder="A tiny example capsule to say hello." />
            </EditorField>
            <EditorField label="Overview">
              <textarea className={TEXTAREA_CLASS} value={draft.description} onChange={(e) => onChange({ ...draft, description: e.target.value })} placeholder="Write a short overview that will appear on the store detail page." />
            </EditorField>
          </div>
          <div className="space-y-4 bg-white px-6 py-5">
            <EditorField label="Public link">
              <select className={INPUT_CLASS} value={draft.publicLinkStatus} onChange={(e) => onChange({ ...draft, publicLinkStatus: e.target.value })}>
                <option value="active">Enabled</option>
                <option value="disabled">Disabled</option>
              </select>
            </EditorField>
            <EditorField label="Store status">
              <select className={INPUT_CLASS} value={draft.storeStatus} onChange={(e) => onChange({ ...draft, storeStatus: e.target.value })}>
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
                {mode === "edit" ? "This form updates the store-facing copy and keeps the preview panel in sync." : "Create a working draft first. You can refine listing info and distribution state later."}
              </p>
              <div className="mt-3 rounded-lg border border-gray-200 bg-white p-3">
                <div className="text-[11px] font-semibold tracking-wide text-gray-400 uppercase">Install command</div>
                <div className="mt-1.5 font-mono text-[13px] text-gray-900">{`ato install ${draft.sourceUrl || "github.com/owner/new-capsule"}`}</div>
              </div>
            </div>
            <div className="flex flex-wrap justify-end gap-2 pt-2">
              <button type="button" onClick={onClose} className="h-10 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">Cancel</button>
              <button type="button" onClick={onSave} className="inline-flex h-10 items-center gap-2 rounded-lg bg-gray-900 px-4 text-sm font-semibold text-white transition-colors hover:bg-gray-700">
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
            <h3 className="text-lg font-bold tracking-tight text-gray-900">{capsule.name}</h3>
            <p className="mt-1 text-sm text-gray-500">Review the missing items before you submit the capsule for Store review.</p>
          </div>
          <button type="button" onClick={onClose} className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600" aria-label="Close">
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
                <div key={item.label} className="flex items-center justify-between rounded-lg border border-gray-200 bg-white px-4 py-2.5 text-sm">
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
              <div className="flex items-center gap-2 font-semibold"><CircleAlert className="h-4 w-4" />Missing items</div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {missingItems.map((item) => (<span key={item} className="inline-flex items-center rounded-full border border-amber-200 bg-white px-2.5 py-0.5 text-xs font-medium text-amber-700">{item}</span>))}
              </div>
            </div>
          ) : (
            <div className="rounded-lg border border-emerald-200 bg-emerald-50 p-4 text-sm text-emerald-900">
              <div className="flex items-center gap-2 font-semibold"><CheckCircle2 className="h-4 w-4" />Everything is ready</div>
              <p className="mt-1 text-emerald-700">The capsule is ready to enter review.</p>
            </div>
          )}
          <div className="flex justify-end gap-2">
            <button type="button" onClick={onClose} className="h-10 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">Cancel</button>
            <button type="button" disabled={!canSubmit} onClick={onSubmit} className={cn("inline-flex h-10 items-center gap-2 rounded-lg px-4 text-sm font-semibold transition-colors", canSubmit ? "bg-gray-900 text-white hover:bg-gray-700" : "cursor-not-allowed bg-gray-100 text-gray-400")}>
              <Upload className="h-4 w-4" />{label}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function Toast({ toast }) {
  if (!toast) return null;
  const toneMap = {
    info: "border-blue-300 bg-blue-50 text-blue-800",
    success: "border-emerald-300 bg-emerald-50 text-emerald-800",
    warning: "border-amber-300 bg-amber-50 text-amber-800",
    danger: "border-red-300 bg-red-50 text-red-800",
  };
  return (
    <div className="dock-toast-enter fixed bottom-5 left-5 z-50 max-w-sm">
      <div className={cn("rounded-lg border px-4 py-2.5 text-sm font-medium shadow-[0_8px_30px_rgba(0,0,0,0.12)]", toneMap[toast.type] ?? toneMap.info)}>
        {toast.message}
      </div>
    </div>
  );
}

function StepperStep({ title, subtitle, done, active, last }) {
  return (
    <div className={cn("relative flex", !last && "pb-7")}>
      <div className="relative flex flex-col items-center">
        <span className={cn("flex h-6 w-6 shrink-0 items-center justify-center rounded-full ring-[3px] ring-white z-10 transition-colors duration-300", done ? "bg-emerald-500" : active ? "bg-blue-600" : "border-2 border-gray-300 bg-white")}>
          {done ? (
            <svg className="h-3 w-3 text-white" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" /></svg>
          ) : active ? (
            <span className="h-1.5 w-1.5 rounded-full bg-white" />
          ) : (
            <span className="h-1.5 w-1.5 rounded-full bg-gray-400" />
          )}
        </span>
        {!last && <div className={cn("absolute left-[11px] top-6 bottom-0 w-0.5 transition-colors duration-300", done ? "bg-emerald-500" : "bg-gray-200")} />}
      </div>
      <div className="ml-3">
        <div className={cn("text-sm", done || active ? "font-semibold text-gray-900" : "font-medium text-gray-400")}>{title}</div>
        {subtitle && <div className="mt-0.5 text-xs text-gray-500">{subtitle}</div>}
      </div>
    </div>
  );
}

function CapsuleGridCard({ capsule, index, onSelect }) {
  const badge = getLifecycleBadge(capsule);
  const gradient = ICON_GRADIENTS[index % ICON_GRADIENTS.length];

  return (
    <button
      type="button"
      onClick={onSelect}
      className="dock-grid-enter group p-5 rounded-2xl border border-slate-200 bg-white shadow-sm flex gap-4 text-left hover:shadow-md hover:border-slate-300 transition-all duration-200"
      style={{ animationDelay: `${index * 40}ms` }}
    >
      <div className={cn("w-[72px] h-[72px] rounded-2xl bg-gradient-to-br flex items-center justify-center shrink-0 shadow-inner text-3xl font-bold text-white", gradient)}>
        {capsule.iconText}
      </div>
      <div className="flex flex-col flex-1 min-w-0 py-0.5">
        <div className="flex items-center gap-2 mb-1">
          <h4 className="font-bold text-[#0F172A] text-[14px] truncate">{capsule.name}</h4>
          <span className={cn("text-[9px] font-bold px-1.5 py-0.5 rounded-md shrink-0", badge.dot === "bg-emerald-500" ? "text-emerald-600 bg-emerald-50" : badge.dot === "bg-amber-400" ? "text-amber-600 bg-amber-50" : "text-red-500 bg-red-50")}>
            {badge.label}
          </span>
        </div>
        <p className="text-[11px] text-slate-500 leading-[1.4] line-clamp-2 mb-auto">
          {capsule.tagline || "No overview has been written yet."}
        </p>
        <div className="flex items-center justify-between mt-2">
          <div className="flex items-center gap-1.5 text-[10px] text-slate-500 font-medium truncate">
            <Github className="h-3 w-3 shrink-0" />
            <span className="truncate">{capsule.sourceUrl}</span>
            <span className="opacity-40 px-0.5">&#8226;</span>
            <span className="text-slate-700 shrink-0">v{capsule.version}</span>
          </div>
          <button
            type="button"
            onClick={(e) => { e.stopPropagation(); onSelect(); }}
            className="flex items-center gap-1 bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white px-2.5 py-1.5 rounded-lg text-[10px] font-bold shadow-sm hover:opacity-90 transition-opacity shrink-0"
          >
            <svg width="8" height="8" viewBox="0 0 24 24" fill="currentColor">
              <polygon points="5 3 19 12 5 21 5 3" />
            </svg>
            Open
          </button>
        </div>
      </div>
    </button>
  );
}

function CompactCard({ capsule, index, selected, onSelect }) {
  const badge = getLifecycleBadge(capsule);
  const gradient = ICON_GRADIENTS[index % ICON_GRADIENTS.length];

  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "group relative w-full flex items-center gap-3 px-3 py-2.5 text-left transition-all duration-150 border-l-[3px]",
        selected
          ? "border-l-blue-600 bg-blue-50"
          : "border-l-transparent hover:bg-gray-50",
      )}
    >
      <div className={cn("grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-gradient-to-br text-sm font-bold text-white", gradient)}>
        {capsule.iconText}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[13px] font-semibold text-gray-900 truncate">{capsule.name}</span>
          <span className={cn("h-[6px] w-[6px] shrink-0 rounded-full", badge.dot)} />
        </div>
        <div className="mt-0.5 text-[11px] text-gray-500 truncate">{capsule.sourceUrl}</div>
      </div>
    </button>
  );
}

function CompactSidebar({ capsules, selectedId, onSelect, onBack, onCollapse }) {
  return (
    <div className="dock-slide-in flex w-60 shrink-0 flex-col border-r border-gray-200 bg-white">
      <div className="flex items-center justify-between px-3 py-3">
        <button
          type="button"
          onClick={onBack}
          className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700"
          title="Back to grid"
        >
          <LayoutGrid className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={onCollapse}
          className="grid h-8 w-8 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700"
          title="Collapse sidebar"
        >
          <ChevronLeft className="h-4 w-4" />
        </button>
      </div>
      <div className="h-px bg-gray-200 mx-3" />
      <nav className="dock-scrollbar min-h-0 flex-1 overflow-y-auto py-1">
        {capsules.map((capsule, i) => (
          <CompactCard
            key={capsule.id}
            capsule={capsule}
            index={i}
            selected={capsule.id === selectedId}
            onSelect={() => onSelect(capsule.id)}
          />
        ))}
      </nav>
    </div>
  );
}

function IconBar({ capsules, selectedId, onSelect, onBack, onToggleExpand }) {
  return (
    <div className="dock-icon-bar-enter flex w-[52px] shrink-0 flex-col items-center border-r border-gray-200 bg-white py-3 gap-1">
      <button
        type="button"
        onClick={onBack}
        className="grid h-9 w-9 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700"
        title="Back to grid"
      >
        <LayoutGrid className="h-4 w-4" />
      </button>
      <button
        type="button"
        onClick={onToggleExpand}
        className="grid h-9 w-9 place-items-center rounded-lg text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700"
        title="Expand sidebar"
      >
        <ChevronRight className="h-4 w-4" />
      </button>
      <div className="h-px w-6 bg-gray-200" />
      {capsules.map((capsule, i) => {
        const gradient = ICON_GRADIENTS[i % ICON_GRADIENTS.length];
        const active = capsule.id === selectedId;
        return (
          <button
            key={capsule.id}
            type="button"
            onClick={() => onSelect(capsule.id)}
            title={capsule.name}
            className={cn(
              "grid h-9 w-9 shrink-0 place-items-center rounded-lg text-sm font-bold transition-all duration-150",
              active
                ? cn("bg-gradient-to-br text-white shadow-sm", gradient)
                : "bg-gray-100 text-gray-500 hover:bg-gray-200 hover:text-gray-700",
            )}
          >
            {capsule.iconText}
          </button>
        );
      })}
    </div>
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
          {capsule.screenshots.length === 0 && <div className="mt-2 text-sm text-gray-400">No screenshots yet</div>}
        </div>
      </div>
    </div>
  );
}

function CapsuleDetail({ capsule, contentKey, onOpenActions }) {
  return (
    <div key={contentKey} className="dock-content-enter mx-auto max-w-3xl px-10 py-8">
      <div className="mb-8 flex items-start justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight text-gray-900">{capsule.name}</h2>
          <p className="mt-2 text-sm leading-relaxed text-gray-500">{capsule.tagline || "No overview has been written yet."}</p>
          <div className="mt-3 flex items-center gap-2">
            <Github className="h-3.5 w-3.5 text-gray-400" />
            <span className="text-xs font-medium text-blue-600">{capsule.sourceUrl}</span>
            <span className="rounded-full border border-gray-200 bg-gray-50 px-2 py-0.5 text-[11px] font-semibold text-gray-500">v{capsule.version}</span>
          </div>
        </div>
        <button
          type="button"
          onClick={onOpenActions}
          className="inline-flex h-9 items-center gap-2 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50"
        >
          <Rocket className="h-4 w-4" />
          Actions
        </button>
      </div>

      <div className="mb-8">
        <PreviewWindow capsule={capsule} />
      </div>

      <div className="mb-8">
        <h3 className="text-base font-semibold text-gray-900 mb-2">Overview</h3>
        <p className="text-sm leading-7 text-gray-600">{capsule.description || "No overview has been written yet."}</p>
      </div>

      <div className="mb-8">
        <h3 className="text-base font-semibold text-gray-900 mb-2">Install</h3>
        <div className="flex items-center justify-between rounded-lg bg-[#1F2937] px-4 py-3.5 shadow-[inset_0_1px_0_rgba(255,255,255,0.06)]">
          <span className="font-mono text-[13px] text-gray-100">{capsule.installCommand}</span>
          <Copy className="h-4 w-4 text-gray-500" />
        </div>
      </div>

      {capsule.latestRelease?.notes?.length > 0 && (
        <div className="mb-8">
          <div className="flex items-center justify-between mb-2">
            <h3 className="text-base font-semibold text-gray-900">What&apos;s new in v{capsule.latestRelease.version}</h3>
            <span className="rounded-full border border-gray-200 bg-gray-50 px-2 py-0.5 text-[11px] font-semibold text-gray-500">Latest</span>
          </div>
          <div className="space-y-1 text-sm leading-7 text-gray-600">
            {capsule.latestRelease.notes.map((note) => <p key={note}>&#8226; {note}</p>)}
          </div>
        </div>
      )}
    </div>
  );
}

function ActionsPanel({ capsule, copied, onCopy, onEdit, onSubmit, onSubmitLabel, submitDisabled, onUnpublish, onDelete, onClose }) {
  const missingItems = capsule ? getMissingSubmissionItems(capsule) : [];

  return (
    <div className="dock-panel-enter flex w-[320px] shrink-0 flex-col border-l border-gray-200 bg-white">
      <div className="dock-scrollbar flex-1 overflow-y-auto px-6 py-6">
        <div className="flex items-center justify-between mb-6">
          <h3 className="text-sm font-bold text-gray-900">Actions</h3>
          <button type="button" onClick={onClose} className="grid h-7 w-7 place-items-center rounded-md text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600" aria-label="Close panel">
            <X className="h-4 w-4" />
          </button>
        </div>

        <button
          type="button"
          onClick={onCopy}
          className={cn(
            "flex h-10 w-full items-center justify-center gap-2 rounded-lg text-sm font-semibold transition-all duration-200",
            copied
              ? "bg-emerald-600 text-white shadow-sm shadow-emerald-600/20"
              : "bg-gray-900 text-white shadow-sm shadow-gray-900/10 hover:bg-gray-700",
          )}
        >
          {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
          {copied ? "Copied!" : "Copy install command"}
        </button>

        <div className="mt-3 space-y-2">
          {capsule?.publicUrl ? (
            <a href={capsule.publicUrl} target="_blank" rel="noreferrer" className="flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">
              <ExternalLink className="h-4 w-4" />View public page
            </a>
          ) : (
            <button type="button" disabled className="flex h-10 w-full cursor-not-allowed items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium text-gray-300">
              <ExternalLink className="h-4 w-4" />View public page
            </button>
          )}
          <button type="button" onClick={onEdit} className="flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">
            <PencilLine className="h-4 w-4" />Edit listing
          </button>
          <button type="button" disabled={submitDisabled} onClick={onSubmit} className={cn("flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium transition-colors", !submitDisabled ? "text-gray-700 hover:bg-gray-50" : "cursor-not-allowed text-gray-300")}>
            <Upload className="h-4 w-4" />{onSubmitLabel}
          </button>
        </div>

        {missingItems.length > 0 && (
          <div className="mt-4 rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900">
            <div className="flex items-center gap-2 font-semibold"><CircleAlert className="h-4 w-4" />Missing items</div>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {missingItems.map((item) => <span key={item} className="inline-flex items-center rounded-full border border-amber-200 bg-white px-2 py-0.5 text-xs font-medium text-amber-700">{item}</span>)}
            </div>
          </div>
        )}

        {capsule && (
          <>
            <div className="mt-6">
              <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-4">Publish Status</h4>
              <div className="space-y-0">
                <StepperStep title="Run & verify" subtitle={capsule.verification.verifiedAt ? "Verification passed" : "Verification not run yet"} done={Boolean(capsule.verification.verifiedAt)} active={!capsule.verification.verifiedAt} last={false} />
                <StepperStep title="Listing info" subtitle={capsule.publishing.listingInfoComplete ? "Listing copy complete" : "Fill in tagline, overview, and category"} done={capsule.publishing.listingInfoComplete} active={!capsule.publishing.listingInfoComplete && Boolean(capsule.verification.verifiedAt)} last={false} />
                <StepperStep title="Distribution" subtitle={capsule.publishing.distributionConfigured ? "Public link or store listing configured" : "Distribution is still hidden"} done={capsule.publishing.distributionConfigured} active={!capsule.publishing.distributionConfigured && capsule.publishing.listingInfoComplete} last={false} />
                <StepperStep title="Submit" subtitle={capsule.storeStatus === "in_review" ? "In review" : capsule.storeStatus === "listed" ? "Listed on the Store" : capsule.storeStatus === "ready" ? "Ready to submit" : "Not submitted yet"} done={["in_review", "listed"].includes(capsule.storeStatus)} active={capsule.storeStatus === "ready"} last />
              </div>
            </div>

            <div className="mt-6 border-t border-gray-100 pt-5">
              <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-3">Details</h4>
              <div className="space-y-2.5 text-sm">
                <div className="flex justify-between"><span className="text-gray-500">Version</span><span className="font-medium text-gray-900">{capsule.version}</span></div>
                <div className="flex justify-between"><span className="text-gray-500">Category</span><span className="font-medium text-gray-900">{capsule.category || "\u2014"}</span></div>
                <div className="flex justify-between"><span className="text-gray-500">Visibility</span><span className="font-medium text-gray-900">{capsule.publicLinkStatus === "active" && capsule.storeStatus === "listed" ? "Public" : capsule.publicLinkStatus === "active" ? "Limited" : "Private"}</span></div>
                <div className="flex justify-between"><span className="text-gray-500">Last updated</span><span className="font-medium text-gray-900">{capsule.updatedAt}</span></div>
              </div>
            </div>

            <div className="mt-6 border-t border-gray-100 pt-5">
              <h4 className="text-[11px] font-bold uppercase tracking-[0.08em] text-gray-500 mb-3">Advanced</h4>
              <div className="space-y-2">
                <button type="button" disabled={capsule.publicLinkStatus !== "active"} onClick={onUnpublish} className={cn("flex h-9 w-full items-center justify-center gap-2 rounded-lg border border-gray-200 bg-white text-sm font-medium transition-colors", capsule.publicLinkStatus === "active" ? "text-gray-700 hover:bg-gray-50" : "cursor-not-allowed text-gray-300")}>
                  <Lock className="h-3.5 w-3.5" />Unpublish link
                </button>
                <button type="button" onClick={onDelete} className="flex h-9 w-full items-center justify-center gap-2 rounded-lg border border-red-200 text-sm font-medium text-red-600 transition-colors hover:bg-red-50">
                  <Trash2 className="h-3.5 w-3.5" />Delete capsule
                </button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function DockHeader({ search, onSearchChange, onImport, onNew, profileOpen, onToggleProfile, onLogin }) {
  return (
    <header className="relative flex h-14 shrink-0 items-center justify-between border-b border-gray-200 bg-white px-5">
      <div className="flex items-center gap-3">
        <h1 className="text-base font-extrabold tracking-tight text-gray-900">
          <span className="text-blue-600">Ato</span> Dock
        </h1>
      </div>

      <div className="mx-4 w-full max-w-sm">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-gray-400" />
          <input
            className="h-9 w-full rounded-lg border border-gray-200 bg-gray-50 pl-9 pr-3 text-sm outline-none transition-all placeholder:text-gray-400 focus:border-blue-500 focus:bg-white focus:ring-2 focus:ring-blue-500/10"
            value={search}
            onChange={(e) => onSearchChange(e.target.value)}
            placeholder="Search capsules..."
          />
        </div>
      </div>

      <div className="flex items-center gap-2">
        <button type="button" onClick={onImport} className="inline-flex h-9 items-center gap-1.5 rounded-lg border border-gray-200 bg-white px-3 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">
          <Download className="h-4 w-4" />Import
        </button>
        <button type="button" onClick={onNew} className="inline-flex h-9 items-center gap-1.5 rounded-lg bg-gray-900 px-3 text-sm font-semibold text-white transition-colors hover:bg-gray-700">
          <Plus className="h-4 w-4" />New Capsule
        </button>
        <div className="relative" data-profile-menu>
          <button type="button" onClick={onToggleProfile} className="flex h-9 w-9 items-center justify-center rounded-full bg-gray-100 text-xs font-bold text-gray-600 transition-colors hover:bg-gray-200">
            {iconTextFrom(CURRENT_USER_NAME)}
          </button>
          {profileOpen && (
            <div data-profile-menu className="dock-fade-in absolute right-0 top-11 z-30 w-56 rounded-lg border border-gray-200 bg-white py-1 shadow-[0_12px_40px_rgba(0,0,0,0.12)]">
              <div className="border-b border-gray-100 px-3 py-2.5">
                <div className="text-sm font-semibold text-gray-900">{CURRENT_USER_NAME}</div>
                <div className="mt-0.5 text-xs text-gray-500">{CURRENT_USER_EMAIL}</div>
                <div className="mt-0.5 text-xs text-gray-500">@{CURRENT_USER_HANDLE}</div>
              </div>
              <button type="button" onClick={onLogin} className="flex w-full items-center gap-2 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50">
                <UserRound className="h-4 w-4 text-gray-400" />Sign in with Ato
              </button>
            </div>
          )}
        </div>
      </div>
    </header>
  );
}

function MyCapsulesPage() {
  const [capsules, setCapsules] = useState(() => INITIAL_CAPSULES);
  const [search, setSearch] = useState("");
  const [selectedCapsuleId, setSelectedCapsuleId] = useState(() => initialSelectedId());
  const [profileOpen, setProfileOpen] = useState(false);
  const [toast, setToast] = useState(null);
  const [modal, setModal] = useState(null);
  const [copied, setCopied] = useState(false);
  const [view, setView] = useState("grid");
  const [sidebarExpanded, setSidebarExpanded] = useState(false);
  const prevCapsuleRef = useRef(null);
  const [contentKey, setContentKey] = useState(0);

  const normalized = useMemo(() => capsules.map(normalizeCapsule), [capsules]);
  const ownedCapsules = useMemo(() => normalized.filter((c) => c.ownerId === CURRENT_USER_ID), [normalized]);

  const filteredCapsules = useMemo(() => {
    const query = search.trim().toLowerCase();
    return ownedCapsules.filter((capsule) => {
      if (!query) return true;
      return [capsule.name, capsule.sourceUrl, capsule.tagline, capsule.description, capsule.category]
        .filter(Boolean)
        .some((v) => v.toLowerCase().includes(query));
    });
  }, [ownedCapsules, search]);

  const selectedCapsule = useMemo(
    () => filteredCapsules.find((c) => c.id === selectedCapsuleId) ?? filteredCapsules[0] ?? null,
    [filteredCapsules, selectedCapsuleId],
  );

  useEffect(() => {
    if (selectedCapsule?.id !== prevCapsuleRef.current) {
      prevCapsuleRef.current = selectedCapsule?.id ?? null;
      setContentKey((k) => k + 1);
    }
  }, [selectedCapsule?.id]);

  useEffect(() => {
    if (typeof window === "undefined") return;
    const url = new URL(window.location.href);
    if (selectedCapsuleId) url.searchParams.set("capsule", selectedCapsuleId);
    else url.searchParams.delete("capsule");
    window.history.replaceState({}, "", `${url.pathname}${url.search}${url.hash}`);
  }, [selectedCapsuleId]);

  useEffect(() => {
    const onPointerDown = (event) => {
      if (!(event.target instanceof Element)) return;
      if (!event.target.closest("[data-profile-menu]")) setProfileOpen(false);
    };
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        setProfileOpen(false);
        setModal(null);
        if (view === "actions") setView("detail");
        else if (view === "detail") { setView("grid"); setSidebarExpanded(false); }
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [view]);

  useEffect(() => {
    const handler = (event) => {
      if (!event || typeof event !== "object") return;
      const message = typeof event.message === "string" ? event.message : typeof event.kind === "string" ? event.kind : null;
      if (!message) return;
      setToast({ type: typeof event.kind === "string" && event.kind.includes("failed") ? "warning" : "info", message });
    };
    window.__ATO_DOCK_EVENT__ = handler;
    return () => { if (window.__ATO_DOCK_EVENT__ === handler) delete window.__ATO_DOCK_EVENT__; };
  }, []);

  useEffect(() => {
    if (!toast) return undefined;
    const timeout = window.setTimeout(() => setToast(null), 2800);
    return () => window.clearTimeout(timeout);
  }, [toast]);

  const setEditorDraft = (nextDraft) => setModal((current) => (current ? { ...current, draft: nextDraft } : current));

  const openEditor = (mode, capsule) => {
    setModal({ kind: "editor", mode, draft: capsule ? toCoreCapsule(capsule) : createDraftCapsule(mode) });
  };

  const openSubmit = (capsule) => {
    setModal({ kind: "submit", capsule: toCoreCapsule(capsule) });
  };

  const persistCapsule = (nextCapsule, isNew = false) => {
    setCapsules((current) => isNew ? [nextCapsule, ...current] : current.map((c) => (c.id === nextCapsule.id ? nextCapsule : c)));
    setSelectedCapsuleId(nextCapsule.id);
    setToast({ type: "success", message: isNew ? "Capsule created" : "Listing saved" });
  };

  const copyInstallCommand = async (capsule = selectedCapsule) => {
    if (!capsule) return;
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
    if (!window.confirm(`Delete capsule "${capsule.name}"? This cannot be undone.`)) return;
    setCapsules((current) => current.filter((item) => item.id !== capsule.id));
    setToast({ type: "danger", message: "Capsule deleted" });
    setView("grid");
  };

  const runUnpublish = (capsule) => {
    setCapsules((current) => current.map((item) => item.id === capsule.id ? { ...item, publicLinkStatus: "disabled", updatedAt: "Updated just now" } : item));
    setToast({ type: "info", message: "Public link unpublished" });
  };

  const runSubmit = (capsule) => {
    const missing = getMissingSubmissionItems(capsule);
    if (missing.length > 0) {
      setToast({ type: "warning", message: "Complete listing info before submitting" });
      return;
    }
    setCapsules((current) => current.map((item) =>
      item.id === capsule.id
        ? {
            ...item,
            storeStatus: item.storeStatus === "listed" ? "listed" : "in_review",
            lifecycleStatus: "published",
            updatedAt: "Updated just now",
            latestRelease: item.latestRelease
              ? { ...item.latestRelease, notes: [...item.latestRelease.notes, "Submitted to Store from the dock."] }
              : { version: item.version, releasedAt: new Date().toISOString(), notes: ["Submitted to Store from the dock."] },
          }
        : item,
    ));
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
    if (!modal || modal.kind !== "editor") return;
    const rawUrl = modal.draft.sourceUrl.trim();

    // Import mode + GitHub URL: hand off to the GitHub Import review
    // surface (ato-import) instead of registering the entry in the
    // local developer dock. This lets the user verify the launch
    // recipe before deciding what to do with the repo.
    if (modal.mode === "import" && looksLikeGitHubRepoUrl(rawUrl)) {
      const sent = postImportOpen(rawUrl);
      if (!sent) {
        setToast({ type: "warning", message: "Import bridge is unavailable" });
        return;
      }
      setToast({ type: "info", message: "Opening GitHub Import…" });
      setModal(null);
      return;
    }

    const next = {
      ...modal.draft,
      name: modal.draft.name.trim() || slugify(modal.draft.sourceUrl.split("/").pop() || "new-capsule"),
      owner: CURRENT_USER_NAME,
      ownerId: CURRENT_USER_ID,
      sourceUrl: rawUrl || `github.com/${CURRENT_USER_HANDLE}/${slugify(modal.draft.name || "new-capsule")}`,
      iconText: iconTextFrom(modal.draft.name || modal.draft.sourceUrl),
      version: modal.draft.version.trim() || "0.1.0",
      updatedAt: "Updated just now",
      publicUrl: modal.draft.publicUrl.trim(),
      verification: modal.draft.verification ?? { verifiedAt: null, lastResult: "unknown" },
      latestRelease: modal.draft.latestRelease ?? { version: modal.draft.version.trim() || "0.1.0", releasedAt: "", notes: [] },
    };
    persistCapsule(next, modal.mode !== "edit");
    setModal(null);
  };

  const handleCardSelect = (id) => {
    setSelectedCapsuleId(id);
    setView("detail");
  };

  const handleIconBarSelect = (id) => {
    setSelectedCapsuleId(id);
  };

  const submitLabel = selectedCapsule?.storeStatus === "listed"
    ? "Update Store listing"
    : selectedCapsule?.storeStatus === "in_review"
      ? "In review"
      : "Submit to Store";
  const submitDisabled = selectedCapsule?.storeStatus === "in_review";

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[#F9FAFB] text-gray-900">
      <DockHeader
        search={search}
        onSearchChange={setSearch}
        onImport={() => openEditor("import")}
        onNew={() => openEditor("new")}
        profileOpen={profileOpen}
        onToggleProfile={() => setProfileOpen((v) => !v)}
        onLogin={handleLogin}
      />

      <div className="flex min-h-0 flex-1">
        {view === "grid" ? (
          <main className="dock-scrollbar min-h-0 flex-1 overflow-y-auto">
            <div className="mx-auto max-w-5xl px-6 py-8">
              {filteredCapsules.length === 0 ? (
                <div className="dock-fade-in flex flex-col items-center py-20 text-center">
                  <div className="mx-auto grid h-14 w-14 place-items-center rounded-xl bg-gray-100">
                    <Package className="h-6 w-6 text-gray-400" />
                  </div>
                  <p className="mt-4 text-base font-semibold text-gray-600">No capsules found</p>
                  <p className="mt-1 text-sm text-gray-400">Import a repo or create a new capsule to get started.</p>
                  <div className="mt-5 flex gap-3">
                    <button type="button" onClick={() => openEditor("import")} className="inline-flex h-10 items-center gap-2 rounded-lg border border-gray-200 bg-white px-4 text-sm font-medium text-gray-700 transition-colors hover:bg-gray-50">
                      <Download className="h-4 w-4" />Import
                    </button>
                    <button type="button" onClick={() => openEditor("new")} className="inline-flex h-10 items-center gap-2 rounded-lg bg-gray-900 px-4 text-sm font-semibold text-white transition-colors hover:bg-gray-700">
                      <Plus className="h-4 w-4" />New Capsule
                    </button>
                  </div>
                </div>
              ) : (
                <div className="grid gap-4 sm:grid-cols-2">
                  {filteredCapsules.map((capsule, i) => (
                    <CapsuleGridCard key={capsule.id} capsule={capsule} index={i} onSelect={() => handleCardSelect(capsule.id)} />
                  ))}
                </div>
              )}
            </div>
          </main>
        ) : (
          <>
            {sidebarExpanded ? (
              <CompactSidebar
                capsules={filteredCapsules}
                selectedId={selectedCapsule?.id}
                onSelect={handleIconBarSelect}
                onBack={() => { setView("grid"); setSidebarExpanded(false); }}
                onCollapse={() => setSidebarExpanded(false)}
              />
            ) : (
              <IconBar
                capsules={filteredCapsules}
                selectedId={selectedCapsule?.id}
                onSelect={handleIconBarSelect}
                onBack={() => { setView("grid"); setSidebarExpanded(false); }}
                onToggleExpand={() => setSidebarExpanded(true)}
              />
            )}
            <main className="dock-scrollbar min-h-0 flex-1 overflow-y-auto">
              {selectedCapsule ? (
                <CapsuleDetail
                  capsule={selectedCapsule}
                  contentKey={contentKey}
                  onOpenActions={() => setView("actions")}
                />
              ) : (
                <div className="grid min-h-full place-items-center p-10">
                  <div className="dock-fade-in text-center">
                    <Package className="mx-auto h-8 w-8 text-gray-300" />
                    <p className="mt-3 text-sm text-gray-500">Select a capsule from the sidebar.</p>
                  </div>
                </div>
              )}
            </main>
            {view === "actions" && selectedCapsule && (
              <ActionsPanel
                capsule={selectedCapsule}
                copied={copied}
                onCopy={() => copyInstallCommand(selectedCapsule)}
                onEdit={() => openEditor("edit", selectedCapsule)}
                onSubmit={() => openSubmit(selectedCapsule)}
                onSubmitLabel={submitLabel}
                submitDisabled={submitDisabled}
                onUnpublish={() => runUnpublish(selectedCapsule)}
                onDelete={() => runDelete(selectedCapsule)}
                onClose={() => setView("detail")}
              />
            )}
          </>
        )}
      </div>

      <Toast toast={toast} />

      {modal?.kind === "editor" && (
        <CapsuleEditorModal
          mode={modal.mode}
          draft={modal.draft}
          onChange={setEditorDraft}
          onClose={() => setModal(null)}
          onSave={createOrSave}
        />
      )}

      {modal?.kind === "submit" && (
        <SubmitModal
          capsule={modal.capsule}
          missingItems={getMissingSubmissionItems(modal.capsule)}
          onClose={() => setModal(null)}
          onSubmit={() => runSubmit(modal.capsule)}
        />
      )}
    </div>
  );
}

export default function App() {
  return <MyCapsulesPage />;
}
