// The wire protocol with the Formation worker: one request on stdin, one
// result on stdout. Mirrors `ato_formation::browser` — the Rust side validates
// every result against the Contract, so the shapes and bounds here must match.

import { z } from "zod";

export const PROTOCOL = "ato.browser-verifier/0";
export const VERIFIER_NAME = "formation-browser-verifier/0";

// Bounds (bytes / counts), identical to the Rust validator.
export const MAX_ACTIONS = 200;
export const MAX_EVIDENCE = 64;
export const MAX_FACTS_PER_EVIDENCE = 64;
export const MAX_TEXT_BYTES = 4096;
export const MAX_REASON_BYTES = 2048;
/// Page text kept per observation; well under MAX_TEXT_BYTES.
export const PAGE_EXCERPT_BYTES = 2048;

const Criterion = z.strictObject({
  id: z.string().min(1),
  kind: z.literal("browser.task"),
  instruction: z.string().min(1),
  required: z.boolean(),
});

export const RequestSchema = z.strictObject({
  protocol: z.literal(PROTOCOL),
  contract: z.strictObject({
    schema: z.literal("ato.browser-contract/0"),
    original_prompt: z.string().min(1),
    criteria: z.array(Criterion).min(1),
    normalization: z.string(),
  }),
  url: z.string().url(),
  budget: z.strictObject({
    max_browser_steps: z.number().int().positive(),
    max_jev_rounds: z.number().int().positive(),
    wall_clock_ms: z.number().int().positive(),
  }),
  scratch_dir: z.string().min(1),
});

export type VerificationRequest = z.infer<typeof RequestSchema>;
export type BrowserCriterion = z.infer<typeof Criterion>;

export type Verdict = "pass" | "fail" | "inconclusive";
export type JudgeChoice = "complete" | "verify_more" | "incomplete";

export interface JudgeDecision {
  choice: JudgeChoice;
  confidence: number;
  probabilities: Record<string, number>;
  model: string;
}

export interface CriterionResult {
  id: string;
  verdict: Verdict;
  decision: JudgeDecision | null;
  rounds: number;
  evidence_refs: string[];
  reason: string | null;
}

/// Where a piece of evidence came from. Never mixed.
///  - browser_snapshot: read from the browser through the page API
///  - model_extracted_facts: a model's reading of the page
///  - agent_report: what the browser agent said it did
export type EvidenceKind = "browser_snapshot" | "model_extracted_facts" | "agent_report";

export interface Evidence {
  id: string;
  kind: EvidenceKind;
  sequence: number;
  url: string | null;
  title: string | null;
  facts: string[];
  text_excerpt: string | null;
}

/// What the browser itself reported: navigations, loads, and every request
/// the origin boundary refused.
export type EventKind = "navigation" | "load" | "blocked_request" | "origin_violation";

export interface BrowserEvent {
  sequence: number;
  kind: EventKind;
  url: string | null;
}

export const MAX_EVENTS = 200;

export interface Action {
  kind: string;
  description: string;
  url: string | null;
}

export interface VerifierIdentity {
  verifier: string;
  stagehand_version: string | null;
  browser_version: string | null;
  agent_model: string | null;
  judge_model: string | null;
}

export interface VerificationResult {
  protocol: typeof PROTOCOL;
  verdict: Verdict;
  criteria: CriterionResult[];
  evidence: Evidence[];
  observed_events: BrowserEvent[];
  action_trace: Action[];
  verifier: VerifierIdentity;
  reason: string | null;
}

/// Cut `text` to at most `bytes` UTF-8 bytes, on a character boundary.
export function bound(text: string, bytes: number): string {
  const encoded = new TextEncoder().encode(text);
  if (encoded.byteLength <= bytes) return text;
  let cut = bytes - 3;
  // Back off continuation bytes so the cut lands between characters.
  while (cut > 0 && (encoded[cut]! & 0xc0) === 0x80) cut--;
  return new TextDecoder().decode(encoded.slice(0, cut)) + "...";
}

export function boundOrNull(text: string | null | undefined, bytes: number): string | null {
  return text === null || text === undefined ? null : bound(text, bytes);
}

/// The overall verdict, computed exactly as the Rust side recomputes it.
export function overallVerdict(
  criteria: readonly BrowserCriterion[],
  results: readonly CriterionResult[],
): Verdict {
  let allPassed = true;
  for (const criterion of criteria.filter((c) => c.required)) {
    const result = results.find((r) => r.id === criterion.id);
    if (result?.verdict === "fail") return "fail";
    if (result?.verdict !== "pass") allPassed = false;
  }
  return allPassed ? "pass" : "inconclusive";
}
