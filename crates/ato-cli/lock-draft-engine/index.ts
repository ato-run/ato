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

// Wrapper function for Cloudflare Workers (synchronous WASM execution)
import { evaluateLockDraftJson } from "./pkg/lock_draft_engine.js";

export function evaluateLockDraft(input: LockDraftInput): LockDraft {
  return JSON.parse(evaluateLockDraftJson(JSON.stringify(input))) as LockDraft;
}
