import assert from "node:assert/strict";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";
import test from "node:test";

import type { JudgeState } from "../src/judge.ts";
import { OllamaJudge } from "../src/local-judge.ts";

const digest = "c".repeat(64);
const tag = "qwen3:8b";
const state: JudgeState = {
  objective: "The heading is visible",
  observed_browser_events: [],
  observed_page_states: [{
    sequence: 1, url: null, title: null, navigation_type: null, visible_text: "untrusted fixture text",
  }],
  model_derived_facts: [],
  agent_claims: [],
  known_gaps: [],
};

test("the local judge validates the tag pin before and after a typed response", async () => {
  let advertised = digest;
  let calls = 0;
  let retagAfterReply = false;
  const server = createServer((request: IncomingMessage, response: ServerResponse) => {
    response.setHeader("content-type", "application/json");
    if (request.url === "/api/tags") {
      response.end(JSON.stringify({ models: [{ name: tag, digest: advertised }] }));
      return;
    }
    calls++;
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      const payload = JSON.parse(body);
      assert.equal(request.url, "/api/chat");
      assert.equal(payload.model, tag);
      assert.equal(payload.stream, false);
      assert.equal(payload.messages[0].role, "system");
      assert.doesNotMatch(payload.messages[0].content, /untrusted fixture text/);
      assert.equal(payload.messages[1].role, "user");
      assert.match(payload.messages[1].content, /untrusted fixture text/);
      if (retagAfterReply) advertised = "b".repeat(64);
      response.end(JSON.stringify({
        model: tag, done: true, message: { role: "assistant", content: JSON.stringify({
          choice: "complete", confidence: 0.9, probabilities: { complete: 0.9, incomplete: 0.05, verify_more: 0.05 },
        }) },
      }));
    });
  }).listen(0, "127.0.0.1");
  await once(server, "listening");
  try {
    const address = server.address();
    assert.ok(address && typeof address !== "string");
    const judge = new OllamaJudge(`http://127.0.0.1:${address.port}`, tag, digest);
    const first = await judge.decide(state, AbortSignal.timeout(3000));
    assert.equal(first.model, "ollama/sha256-" + digest);
    assert.equal(first.choice, "complete");
    retagAfterReply = true;
    await assert.rejects(judge.decide(state, AbortSignal.timeout(3000)), /judge_invalid_response/);
    await assert.rejects(judge.decide(state, AbortSignal.timeout(3000)), /judge_invalid_response/);
    assert.equal(calls, 2);
  } finally {
    server.close();
  }
});

test("the local judge refuses non-loopback addresses and remote model tags", () => {
  assert.throws(() => new OllamaJudge("http://192.0.2.1:11434", tag, digest), /judge_not_configured/);
  assert.throws(() => new OllamaJudge("http://127.0.0.1:11434", "qwen3:8b-cloud", digest), /judge_not_configured/);
});
