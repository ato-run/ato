import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { resolve, join } from "node:path";
import { once } from "node:events";
import { createFaultProxy, pauseOwnedRuntime } from "./unknown-result-transport.mjs";

function fixture() {
  const parent = resolve(".tmp"); mkdirSync(parent, { recursive: true });
  return mkdtempSync(join(parent, "unknown-transport-test-"));
}
async function listen(server) { server.listen(0, "127.0.0.1"); await once(server, "listening"); return server.address().port; }
async function request(port, path) {
  return new Promise((resolve) => {
    const req = http.request({ hostname: "127.0.0.1", port, path, method: "POST" }, (res) => {
      res.resume(); res.on("end", () => resolve(res.statusCode));
    });
    req.on("error", (error) => resolve(error.code)); req.end("{}");
  });
}
for (const confirmed of [false, true]) {
  test(`first result cannot escape when workload confirmation is ${confirmed}`, async () => {
    const root = fixture(); let delivered = 0; let pauses = 0;
    const upstream = http.createServer((req, res) => { delivered++; req.resume(); res.end("{}"); });
    let proxy;
    try {
      const port = await listen(upstream);
      if (confirmed) writeFileSync(join(root, "workload-started.json"), "{}");
      proxy = createFaultProxy({ root, upstreamPort: port, pause: () => { pauses++; return 123; } });
      const proxyPort = await listen(proxy);
      assert.equal(await request(proxyPort, "/v1/runtime-network/attempts/fixture/result"), "ECONNRESET");
      assert.equal(delivered, 0); assert.equal(pauses, 1);
      const proof = JSON.parse(readFileSync(join(root, "result-transport-cut.json")));
      assert.equal(proof.cut_after_confirmed_start, confirmed); assert.equal(proof.delivered, false);
      writeFileSync(join(root, "release-result-transport"), "owner release");
      assert.equal(await request(proxyPort, "/v1/runtime-network/attempts/fixture/result"), 200);
      assert.equal(delivered, 1); assert.equal(pauses, 1);
    } finally { proxy?.close(); upstream.close(); rmSync(root, { recursive: true }); }
  });
}
test("missing or changed Runtime identity fails closed", async () => {
  const root = fixture(); let delivered = 0;
  const upstream = http.createServer((req, res) => { delivered++; res.end(); });
  let proxy;
  try {
    const port = await listen(upstream);
    proxy = createFaultProxy({ root, upstreamPort: port, pause: () => { throw new Error("identity changed"); } });
    const proxyPort = await listen(proxy);
    assert.equal(await request(proxyPort, "/v1/runtime-network/attempts/fixture/result"), 503);
    assert.equal(delivered, 0);
  } finally { proxy?.close(); upstream.close(); rmSync(root, { recursive: true }); }
});
test("Linux pause checks identity and actually holds an owner fixture process", { skip: process.platform !== "linux" }, async () => {
  const root = fixture(); const argv = [process.execPath, "--eval", "setInterval(()=>{},1000)"];
  const child = spawn(argv[0], argv.slice(1), { stdio: "ignore" });
  try {
    await once(child, "spawn");
    const stat = readFileSync(`/proc/${child.pid}/stat`).toString();
    const ticks = stat.slice(stat.lastIndexOf(") ") + 2).split(" ")[19];
    const control = { pid: child.pid, start_ticks: ticks };
    writeFileSync(join(root, "runtime-control.json"), JSON.stringify(control));
    writeFileSync(join(root, "infrastructure-start.json"), JSON.stringify({ Runtime_argv: argv,
      runtime_binary_sha256: createHash("sha256").update(readFileSync(argv[0])).digest("hex") }));
    assert.equal(pauseOwnedRuntime(root), child.pid);
    const end = Date.now() + 1000;
    while (readFileSync(`/proc/${child.pid}/stat`).toString().split(") ")[1][0] !== "T" && Date.now() < end)
      await new Promise((done) => setTimeout(done, 10));
    assert.equal(readFileSync(`/proc/${child.pid}/stat`).toString().split(") ")[1][0], "T");
    child.kill("SIGCONT");
    writeFileSync(join(root, "runtime-control.json"), JSON.stringify({ ...control, start_ticks: "0" }));
    assert.throws(() => pauseOwnedRuntime(root), /runtime_identity_changed/);
  } finally { child.kill("SIGCONT"); child.kill("SIGTERM"); await once(child, "exit"); rmSync(root, { recursive: true }); }
});
