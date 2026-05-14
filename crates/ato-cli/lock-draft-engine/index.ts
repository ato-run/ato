/**
 * TypeScript types for the lock-draft-wasm package.
 * These mirror the Rust types in lock-draft-engine/src/lib.rs (serialized via serde).
 * Source: apps/ato-cli/lock-draft-engine/src/lib.rs
 * Build: wasm-pack build --target bundler --features wasm
 */

export type LockDraftReadiness =
  | "draft"
  | "ready_to_finalize"
  | "finalized_locally"
  | "preview_verified"
  | "publishable";

export type LockDraftConfidence = "low" | "medium" | "high";

export interface SelectedTarget {
  label?: string | null;
  runtime?: string | null;
  driver?: string | null;
  entrypoint?: string | null;
  run_command?: string | null;
  cmd?: string[];
  runtime_version?: string | null;
  runtime_tools?: Record<string, string>;
  dependencies_path?: string | null;
}

export type RepoFileKind = "file" | "dir";

export interface RepoFileEntry {
  path: string;
  kind: RepoFileKind;
}

export interface ManifestSource {
  text: string;
  selected_target_label?: string | null;
}

export interface ExistingAtoLockSummary {
  manifest_hash?: string | null;
  runtime_keys?: string[];
  tool_keys?: string[];
  target_keys?: string[];
}

export interface LockDraftExternalDependency {
  name: string;
  source: string;
  source_type: string;
  injection_bindings?: Record<string, string>;
}

export interface LockDraftRuntimePlatform {
  os: string;
  arch: string;
  target_triple: string;
}

export interface LockDraftInput {
  selected_target?: SelectedTarget | null;
  repo_file_index?: RepoFileEntry[];
  file_text_map?: Record<string, string>;
  manifest_source?: ManifestSource | null;
  existing_ato_lock_summary?: ExistingAtoLockSummary | null;
  external_dependency_hints?: LockDraftExternalDependency[];
}

export interface LockDraft {
  runtime?: string | null;
  driver?: string | null;
  required_runtime_version?: string | null;
  runtime_tools?: Record<string, string>;
  runtime_platforms?: LockDraftRuntimePlatform[];
  required_native_lockfiles?: string[];
  missing_native_lockfiles?: string[];
  external_capsule_dependencies?: LockDraftExternalDependency[];
  blocking_issues?: string[];
  warnings?: string[];
  suggested_commands?: string[];
  readiness: LockDraftReadiness;
  confidence: LockDraftConfidence;
  draft_hash: string;
}

// Type aliases expected by ato-api
export type LockDraftRepoFileEntry = RepoFileEntry;
export type LockDraftSelectedTarget = SelectedTarget;

// ── LEIP v1 types ─────────────────────────────────────────────────────────────
// Mirror of leip.rs types, serialized via serde.

export type EvidenceKind =
  | "runtime_marker_file"
  | "package_manager_lockfile"
  | "package_script_command"
  | "entrypoint_file"
  | "direct_shell_command"
  | "readme_raw_shell_command"
  | "framework_marker"
  | "manifest_hint";

export type EvidenceSource = "repo_file_index" | "file_text_map" | "manifest_hint";

export interface Evidence {
  id: string;
  kind: EvidenceKind;
  path: string;
  key: string;
  normalized_value: string;
  source: EvidenceSource;
}

export type LeipNodeKind = "app_target" | "worker_target" | "provider_capsule";

export interface LaunchEnvelopeDraft {
  driver?: string | null;
  runtime_version?: string | null;
  cmd: string[];
  entrypoint?: string | null;
  env?: Record<string, string>;
  port?: number | null;
}

export interface LaunchGraphNodeDraft {
  id: string;
  kind: LeipNodeKind;
  label: string;
  envelope?: LaunchEnvelopeDraft | null;
  service?: string | null;
  provider_capsule?: string | null;
}

export interface LaunchGraphEdgeDraft {
  source: string;
  target: string;
  kind: string;
  evidence_refs: string[];
}

export interface LaunchGraphDraft {
  primary_node_id: string;
  nodes: LaunchGraphNodeDraft[];
  edges: LaunchGraphEdgeDraft[];
  evidence_refs: string[];
  unsupported_features: string[];
}

export type LeipDecision =
  | { kind: "auto_accept"; candidate_id: string }
  | { kind: "needs_selection"; reason: string }
  | { kind: "unresolved"; reason: string }
  | { kind: "rejected"; reason: string };

export interface LaunchGraphCandidate {
  id: string;
  graph: LaunchGraphDraft;
  score: number;
  relative_confidence: number;
  margin: number;
  decision: LeipDecision;
  evidence_refs: string[];
  risks: string[];
  hard_constraint_failures: string[];
  runtime_score: number;
  launch_score: number;
  lockfile_score: number;
  risk_penalty: number;
}

export interface LeipInput {
  repo_file_index?: RepoFileEntry[];
  file_text_map?: Record<string, string>;
  target_hint?: SelectedTarget | null;
  existing_ato_lock_summary?: ExistingAtoLockSummary | null;
}

export interface LeipResult {
  candidates: LaunchGraphCandidate[];
  rejected_candidates: LaunchGraphCandidate[];
  decision: LeipDecision;
  engine_version: string;
  required_evidence_coverage: number;
  diagnostics: string[];
}

export interface VerificationObservation {
  candidate_id: string;
  stage: string;
  status: string;
  failure_class?: string | null;
  redacted_log_excerpt?: string | null;
  elapsed_ms: number;
}

// Wrapper function for Cloudflare Workers (synchronous WASM execution).
// wasm-pack's generated wrapper calls __wbindgen_start() unconditionally, but
// Cloudflare's module validator may expose the wasm exports under `default`.
// Initialize the generated bg helpers directly so both Node/Vitest and Workers
// see the same initialized wasm object.
import * as wasmModule from "./pkg/lock_draft_engine_bg.wasm";
import {
  __wbg_set_wasm,
  __wbindgen_init_externref_table,
  evaluateLockDraftJson,
  // NOTE: evaluateLaunchGraphsJson / evaluateLaunchEnvelopesJson are only available
  // after `wasm-pack build --target bundler --features wasm` is re-run.
  // The stubs below cast via `unknown` to avoid build failures until the pkg/ is rebuilt.
} from "./pkg/lock_draft_engine_bg.js";

type LockDraftWasmExports = {
  __wbindgen_externrefs?: { grow: (delta: number) => number };
  __wbindgen_free?: unknown;
  __wbindgen_malloc?: unknown;
  __wbindgen_start?: () => void;
};

let initialized = false;

function resolveWasmExports(): LockDraftWasmExports {
  return "default" in wasmModule
    ? ((wasmModule as { default: LockDraftWasmExports }).default ??
        (wasmModule as LockDraftWasmExports))
    : (wasmModule as LockDraftWasmExports);
}

function ensureWasmInitialized() {
  if (initialized) {
    return;
  }

  const wasmExports = resolveWasmExports();
  if (
    typeof wasmExports.__wbindgen_malloc !== "function" ||
    typeof wasmExports.__wbindgen_free !== "function"
  ) {
    throw new Error("lock-draft WASM exports are unavailable");
  }

  __wbg_set_wasm(wasmExports);
  if (typeof wasmExports.__wbindgen_start === "function") {
    wasmExports.__wbindgen_start();
  } else if (wasmExports.__wbindgen_externrefs) {
    __wbindgen_init_externref_table();
  } else {
    throw new Error("lock-draft WASM externref table is unavailable");
  }
  initialized = true;
}

export function evaluateLockDraft(input: LockDraftInput): LockDraft {
  ensureWasmInitialized();
  return JSON.parse(evaluateLockDraftJson(JSON.stringify(input))) as LockDraft;
}

/**
 * Primary LEIP v1 inference API.
 * NOTE: Requires wasm-pack rebuild to be functional — stubs until then.
 */
export function evaluateLaunchGraphs(input: LeipInput): LeipResult {
  ensureWasmInitialized();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const fn = (globalThis as any).evaluateLaunchGraphsJson as
    | ((s: string) => string)
    | undefined;
  if (typeof fn !== "function") {
    throw new Error(
      "evaluateLaunchGraphsJson WASM export unavailable — run wasm-pack build to regenerate pkg/"
    );
  }
  return JSON.parse(fn(JSON.stringify(input))) as LeipResult;
}

/**
 * LEIP v1 compatibility wrapper.
 * Accepts `LockDraftInput` (maps `selected_target` → `target_hint`)
 * and returns a `LeipResult`.
 * NOTE: Requires wasm-pack rebuild to be functional — stubs until then.
 */
export function evaluateLaunchEnvelopes(input: LockDraftInput): LeipResult {
  ensureWasmInitialized();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const fn = (globalThis as any).evaluateLaunchEnvelopesJson as
    | ((s: string) => string)
    | undefined;
  if (typeof fn !== "function") {
    throw new Error(
      "evaluateLaunchEnvelopesJson WASM export unavailable — run wasm-pack build to regenerate pkg/"
    );
  }
  return JSON.parse(fn(JSON.stringify(input))) as LeipResult;
}
