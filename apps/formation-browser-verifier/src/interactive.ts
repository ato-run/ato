// A single operator mailbox owned by Ato in the helper's private scratch.
// The page cannot write it. Commands are constrained to an observed document.
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { type Snapshot, type TaskOutcome } from "./browser.ts";
import type { Action } from "./protocol.ts";

const control = z.number().int().positive().max(100);
const ActionSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("click"), element: control }),
  z.strictObject({ kind: z.literal("type"), element: control, text: z.string().max(512) }),
  z.strictObject({ kind: z.literal("navigate"), path: z.string().max(512) }),
  z.strictObject({ kind: z.literal("reload") }),
  z.strictObject({ kind: z.literal("finish") }),
]);

const CommandSchema = z.strictObject({
  request_id: z.string().regex(/^[0-9a-f]{32}$/),
  observation_id: z.uuid(),
  session_id: z.string(),
  action: ActionSchema,
});

export type InteractiveAction = z.infer<typeof ActionSchema>;
export interface InteractiveElement { id: number; label: string; kind: string; can_type: boolean }
export type ActionStatus = "applied" | "stale" | "refused";
export interface InteractiveDriver {
  interactiveObserve(): Promise<{ snapshot: Snapshot; elements: InteractiveElement[] }>;
  interactiveAct(action: InteractiveAction): Promise<ActionStatus>;
}

export async function operate(
  scratch: string,
  session: string,
  driver: InteractiveDriver,
  maxSteps: number,
  signal: AbortSignal,
  onObserve: (snapshot: Snapshot) => void,
): Promise<TaskOutcome> {
  const commandPath = join(scratch, "operator", "command.json");
  const viewPath = join(scratch, "operator", "observation.json");
  const actions: Action[] = [];
  let lastAction: {request_id: string; status: ActionStatus} | null = null;
  try {
    for (let step = 0; step < maxSteps; step++) {
      signal.throwIfAborted();
      const observed = await driver.interactiveObserve();
      onObserve(observed.snapshot);
      const id = randomUUID();
      const url = observed.snapshot.url ? new URL(observed.snapshot.url) : null;
      // Remove URL query and fragment from the client projection and never
      // expose DOM input values. These page observations are untrusted.
      const snapshot = { ...observed.snapshot, url: url ? url.origin + url.pathname : null };
      writeFileSync(viewPath + ".next", JSON.stringify({
        session_id: session, observation_id: id, snapshot,
        elements: observed.elements, last_action: lastAction,
      }), { mode: 0o600 });
      renameSync(viewPath + ".next", viewPath);
      // Leave the previous command in place until its replacement observation
      // has been published, so another writer cannot target the old view.
      if (lastAction !== null) rmSync(commandPath, { force: true });
      while (!existsSync(commandPath)) await delay(40, undefined, { signal });
      const parsed = CommandSchema.safeParse(JSON.parse(readFileSync(commandPath, "utf8")));
      if (!parsed.success || parsed.data.session_id !== session) {
        throw new Error("invalid_operator_command");
      }
      const command = parsed.data;
      if (command.observation_id !== id) {
        lastAction = { request_id: command.request_id, status: "stale" };
      } else if (command.action.kind === "finish") {
        actions.push({
          kind: "operator_finish", description: command.request_id, url: null,
        });
        return { claimedActions: actions, completed: true, message: null };
      } else {
        lastAction = { request_id: command.request_id, status: await driver.interactiveAct(command.action) };
      }
      actions.push({
        kind: "operator_" + command.action.kind + "_" + lastAction.status,
        description: command.request_id,
        url: null,
      });
    }
    throw new Error("operator_step_budget_exhausted");
  } finally {
    rmSync(viewPath, { force: true });
    rmSync(commandPath, { force: true });
  }
}
