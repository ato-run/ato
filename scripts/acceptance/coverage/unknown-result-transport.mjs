// Acceptance-only fault injection. Never forwards a result before owner release,
// including when workload observation is missing. No Search or attempt is created.
import http from "node:http";
import { existsSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

export function pauseOwnedRuntime(root) {
  const control = JSON.parse(readFileSync(join(root, "runtime-control.json")));
  const expected = JSON.parse(readFileSync(join(root, "infrastructure-start.json")));
  if (!Number.isSafeInteger(control.pid) || control.pid <= 1 || !control.start_ticks)
    throw new Error("runtime_identity_missing");
  const argv = readFileSync(`/proc/${control.pid}/cmdline`).toString().split("\0").filter(Boolean);
  const stat = readFileSync(`/proc/${control.pid}/stat`).toString();
  const ticks = stat.slice(stat.lastIndexOf(") ") + 2).split(" ")[19];
  const digest = createHash("sha256").update(readFileSync(expected.Runtime_argv[0])).digest("hex");
  if (JSON.stringify(argv) !== JSON.stringify(expected.Runtime_argv) ||
      ticks !== control.start_ticks || digest !== expected.runtime_binary_sha256)
    throw new Error("runtime_identity_changed");
  const stopped = spawnSync("sudo", ["kill", "-STOP", String(control.pid)], { timeout: 5000 });
  if (stopped.status !== 0) throw new Error("runtime_pause_failed");
  return control.pid;
}

export function createFaultProxy({ root, upstreamPort, pause = pauseOwnedRuntime }) {
  return http.createServer((req, res) => {
    const isResult = req.method === "POST" && /^\/v1\/runtime-network\/attempts\/[^/]+\/result$/.test(req.url || "");
    if (isResult && !existsSync(join(root, "release-result-transport"))) {
      try {
        const pid = pause(root);
        const confirmed = existsSync(join(root, "workload-started.json"));
        const proof = { at_ms: Date.now(), method: req.method, path: req.url,
          delivered: false, runtime_pid: pid, paused_before_retry: true,
          cut_after_confirmed_start: confirmed, product_timeout_changed: false };
        appendFileSync(join(root, "blocked-transport.jsonl"), JSON.stringify(proof) + "\n", { mode: 0o600 });
        writeFileSync(join(root, "result-transport-cut.json"), JSON.stringify(proof), { mode: 0o600 });
        req.socket.destroy();
      } catch {
        // A broken observer must not turn this acceptance into an ordinary PASS.
        res.writeHead(503); res.end("acceptance_transport_not_released");
      }
      return;
    }
    const upstream = http.request({ hostname: "127.0.0.1", port: upstreamPort,
      path: req.url, method: req.method, headers: req.headers }, (response) => {
      res.writeHead(response.statusCode, response.headers); response.pipe(res);
    });
    upstream.on("error", () => { res.writeHead(503); res.end(); });
    req.pipe(upstream);
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const root = process.cwd();
  const [port, upstreamPort] = process.argv.slice(2).map(Number);
  if (![port, upstreamPort].every((v) => Number.isInteger(v) && v > 0 && v < 65536))
    throw new Error("explicit_ports_required");
  const server = createFaultProxy({ root, upstreamPort });
  server.listen(port, "127.0.0.1");
  process.on("SIGTERM", () => server.close(() => process.exit(0)));
}
