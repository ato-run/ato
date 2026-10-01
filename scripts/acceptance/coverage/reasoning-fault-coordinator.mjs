// Acceptance-only transparent proxy. Successful backend mutations commit
// before selected responses disappear; production Coordinator is unchanged.
import http from 'node:http';
import {spawn} from 'node:child_process';
import {appendFileSync, existsSync} from 'node:fs';
import {createHash} from 'node:crypto';

const config = JSON.parse(process.env.ATO_FORMATION_COORDINATOR_FAULTS);
const frontPort = Number(process.env.C2_PORT);
const backPort = Number(config.backend_port);
if (!Number.isInteger(frontPort) || !Number.isInteger(backPort) || backPort === frontPort ||
    !existsSync(config.backend_script) || !Array.isArray(config.drop_once)) throw Error('fault configuration');
const journal = 'transport-faults.jsonl';
const dropped = new Set();
// A restarted proxy must not replay a fault already delivered.
const {readFileSync} = await import('node:fs');
if (existsSync(journal)) for (const line of readFileSync(journal, 'utf8').trim().split('\n')) {
  if (line) { const row = JSON.parse(line); if (row.dropped) dropped.add(row.rule); }
}
const backend = spawn(process.execPath, [config.backend_script], {
  env: {...process.env, C2_PORT: String(backPort)}, stdio: 'inherit',
});
backend.on('exit', code => { server.close(); process.exit(code ?? 1); });
const server = http.createServer((request, response) => {
  const hash = createHash('sha256');
  request.on('data', chunk => hash.update(chunk));
  const upstream = http.request({hostname: '127.0.0.1', port: backPort,
    method: request.method, path: request.url, headers: request.headers}, incoming => {
    const path = request.url.split('?')[0];
    const rule = config.drop_once.findIndex(r => r.method === request.method &&
      new RegExp(r.path).test(path) && incoming.statusCode === r.status);
    const cut = rule >= 0 && !dropped.has(rule);
    if (cut) {
      incoming.resume();
      incoming.on('end', () => {
        dropped.add(rule);
        appendFileSync(journal, JSON.stringify({method: request.method, path,
          request_sha256: hash.digest('hex'), status: incoming.statusCode,
          rule, dropped: true, at_ms: Date.now()})+'\n', {mode: 0o600});
        response.destroy();
      });
    } else {
      response.writeHead(incoming.statusCode, incoming.headers);
      incoming.pipe(response);
    }
  });
  upstream.on('error', () => { response.writeHead(503); response.end(); });
  request.on('error', () => upstream.destroy());
  request.pipe(upstream);
});
server.listen(frontPort, '127.0.0.1');
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => {
  server.close(); backend.kill('SIGTERM');
});
