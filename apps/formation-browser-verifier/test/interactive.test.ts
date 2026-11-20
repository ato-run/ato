import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";

import { operate, type InteractiveAction, type InteractiveDriver } from "../src/interactive.ts";

test("old observations cannot act and a subsequent current action is recorded", async () => {
  const scratch = mkdtempSync(join(tmpdir(), "ato-browser-operator-"));
  const mailbox = join(scratch, "operator");
  mkdirSync(mailbox);
  const command = join(mailbox, "command.json");
  const observation = join(mailbox, "observation.json");
  const session = "publication-test-1";
  const actions: InteractiveAction[] = [];
  const seen: string[] = [];
  const driver: InteractiveDriver = {
    async interactiveObserve() {
      return {
        snapshot: { url: "http://127.0.0.1:8090/?canary=omit", title: "Example", text: "Page data", navigationType: "navigate" },
        elements: [{ id: 1, kind: "button", label: "Add", can_type: false }],
      };
    },
    async interactiveAct(action) {
      actions.push(action);
      return "applied";
    },
  };
  const signal = AbortSignal.timeout(3000);
  const job = operate(scratch, session, driver, 4, signal, (snapshot) => { seen.push(snapshot.text ?? ""); });
  async function readAfter(previous?: string) {
    for (;;) {
      signal.throwIfAborted();
      try {
        const value = JSON.parse(readFileSync(observation, "utf8"));
        if (value.observation_id !== previous) return value;
      } catch {}
      await delay(10);
    }
  }
  function send(observation_id: string, action: InteractiveAction, number: string) {
    writeFileSync(command, JSON.stringify({ session_id: session, request_id: number.repeat(32), observation_id, action }));
  }

  try {
    const first = await readAfter();
    assert.equal(first.snapshot.url, "http://127.0.0.1:8090/");
    send("11111111-1111-4111-a111-111111111111", { kind: "click", element: 1 }, "1");
    const second = await readAfter(first.observation_id);
    assert.equal(second.last_action.status, "stale");
    assert.equal(actions.length, 0);
    send(second.observation_id, { kind: "click", element: 1 }, "2");
    const third = await readAfter(second.observation_id);
    assert.equal(third.last_action.status, "applied");
    send(third.observation_id, { kind: "finish" }, "3");
    const result = await job;
    assert.deepEqual(actions, [{ kind: "click", element: 1 }]);
    assert.deepEqual(result.claimedActions.map((item) => item.kind),
      ["operator_click_stale", "operator_click_applied", "operator_finish"]);
    assert.equal(result.completed, true);
    assert.equal(seen.length, 3);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});
