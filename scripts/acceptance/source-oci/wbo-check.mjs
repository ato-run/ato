#!/usr/bin/env node
// Functional check of a running WBO through two real browser clients.
// Drives headless Chrome over the DevTools protocol (Node's built-in
// WebSocket; no npm packages). Uses WBO's own client API, as its
// scripts/generateload.mjs does, to draw one pencil line with a fixed id.
//
//   node wbo-check.mjs draw   --chrome PATH --url BASE --board NAME --id ID
//     client A draws; client B must render the same id (live reflection)
//   node wbo-check.mjs verify --chrome PATH --url BASE --board NAME --id ID
//     a fresh client must render the id (persisted board after restart)
//
// Prints one JSON line. Exit 0 only when the expectation holds.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [mode, ...rest] = process.argv.slice(2);
const opt = {};
for (let i = 0; i < rest.length; i += 2) opt[rest[i].replace(/^--/, "")] = rest[i + 1];
const PENCIL = 1; // client-data/tools/manifest.js TOOL_CODE_BY_ID.pencil
const CREATE = 1; // client-data/js/mutation_type.js
const APPEND = 4;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const profile = mkdtempSync(join(opt.scratch ?? tmpdir(), "wbo-check-"));
const chrome = spawn(
  opt.chrome,
  [
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    `--user-data-dir=${profile}`,
    "--remote-debugging-port=0",
    "about:blank",
  ],
  { stdio: ["ignore", "ignore", "pipe"] },
);
let chromeLog = "";
chrome.stderr.on("data", (d) => (chromeLog = (chromeLog + d).slice(-2000)));

function finish(result, ok) {
  console.log(JSON.stringify({ mode, ...result, ok }));
  try {
    chrome.kill("SIGKILL");
  } catch {}
  setTimeout(() => {
    rmSync(profile, { recursive: true, force: true });
    process.exit(ok ? 0 : 1);
  }, 300);
}

async function devtools() {
  const file = join(profile, "DevToolsActivePort");
  for (let i = 0; i < 100 && !existsSync(file); i++) await sleep(100);
  const [port, path] = readFileSync(file, "utf8").trim().split("\n");
  const ws = new WebSocket(`ws://127.0.0.1:${port}${path}`);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
  let next = 1;
  const pending = new Map();
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    }
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return { send };
}

async function openClient(cdp, url) {
  const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  await cdp.send("Page.enable", {}, sessionId);
  await cdp.send("Page.navigate", { url }, sessionId);
  const evaluate = async (expression) => {
    const r = await cdp.send(
      "Runtime.evaluate",
      { expression, awaitPromise: true, returnByValue: true },
      sessionId,
    );
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 500));
    return r.result.value;
  };
  return { evaluate };
}

const READY = `(() => { const t = window.WBOApp; return !!(t &&
  document.documentElement.dataset.boardPhase === "ready" &&
  t.connection.socket?.connected && t.replay.awaitingSnapshot === false &&
  !t.writes.isWritePaused()); })()`;

async function until(client, expression, ms) {
  const started = Date.now();
  while (Date.now() - started < ms) {
    try {
      if (await client.evaluate(expression)) return Date.now() - started;
    } catch {}
    await sleep(200);
  }
  return null;
}

async function main() {
  const cdp = await devtools();
  const boardUrl = `${opt.url.replace(/\/$/, "")}/boards/${encodeURIComponent(opt.board)}`;
  const present = `!!document.getElementById(${JSON.stringify(opt.id)})`;
  if (mode === "draw") {
    const a = await openClient(cdp, boardUrl);
    const b = await openClient(cdp, boardUrl);
    const readyA = await until(a, READY, 30000);
    const readyB = await until(b, READY, 30000);
    if (readyA === null || readyB === null)
      return finish({ stage: "ready", readyA, readyB, chromeLog }, false);
    const alreadyThere = await b.evaluate(present);
    await a.evaluate(`(async () => {
      const t = window.WBOApp;
      await t.toolRegistry.bootTool("pencil");
      t.writes.drawAndSend({ tool: ${PENCIL}, type: ${CREATE}, id: ${JSON.stringify(opt.id)},
        color: "#123456", size: 4, opacity: 1 });
      for (const [x, y] of [[100,100],[140,120],[180,160],[220,150],[260,200]]) {
        t.writes.drawAndSend({ tool: ${PENCIL}, type: ${APPEND}, parent: ${JSON.stringify(opt.id)}, x, y });
      }
      return true; })()`);
    const drawnOnA = await until(a, present, 10000);
    const reflectedOnB = await until(b, present, 10000);
    // Allow the server's save interval (WBO_SAVE_INTERVAL default 2000 ms).
    await sleep(5000);
    return finish(
      { stage: "reflect", board: opt.board, id: opt.id, alreadyThere, drawnOnA, reflectedOnB },
      !alreadyThere && drawnOnA !== null && reflectedOnB !== null,
    );
  }
  if (mode === "verify") {
    const c = await openClient(cdp, boardUrl);
    const ready = await until(c, READY, 30000);
    if (ready === null) return finish({ stage: "ready", ready, chromeLog }, false);
    const found = await until(c, present, 10000);
    return finish({ stage: "persist", board: opt.board, id: opt.id, found }, found !== null);
  }
  finish({ error: `unknown mode ${mode}` }, false);
}

const timer = setTimeout(() => finish({ stage: "timeout", chromeLog }, false), 120000);
main()
  .catch((e) => finish({ stage: "error", error: String(e), chromeLog }, false))
  .finally(() => clearTimeout(timer));
