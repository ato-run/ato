#!/usr/bin/env node
/**
 * greeter-service — Minimal IPC Shared Service
 *
 * Listens on a TCP port (default 50051) or Unix socket and responds to
 * JSON-RPC 2.0 requests.
 *
 * Methods:
 *   greet({ name }) → { greeting: "Hello, <name>!" }
 *   health()        → { status: "ok", uptime_ms }
 *
 * Environment:
 *   CAPSULE_IPC_GREETER_SOCKET — TCP port number or Unix socket path
 */

import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const DEFAULT_PORT = 50051;
const socketEnv = process.env.CAPSULE_IPC_GREETER_SOCKET;
// Use TCP port when env var is a number or unset; use Unix socket when it's a path
const SOCKET = socketEnv && isNaN(Number(socketEnv)) ? socketEnv : Number(socketEnv) || DEFAULT_PORT;
const isUnixSocket = typeof SOCKET === "string";
const startedAt = Date.now();

if (isUnixSocket) {
  // Remove stale socket
  try { fs.unlinkSync(SOCKET); } catch (_) { /* ok */ }
  const dir = path.dirname(SOCKET);
  if (dir !== ".") fs.mkdirSync(dir, { recursive: true });
}

function handleRequest(req) {
  switch (req.method) {
    case "greet": {
      const name = req.params?.name ?? "World";
      return { greeting: `Hello, ${name}!` };
    }
    case "health":
      return { status: "ok", uptime_ms: Date.now() - startedAt };
    default:
      return {
        __error: { code: -32601, message: `Method '${req.method}' not found` },
      };
  }
}

const server = net.createServer((conn) => {
  let buf = "";
  conn.on("data", (chunk) => {
    buf += chunk.toString();
    // Simple newline-delimited JSON
    let idx;
    while ((idx = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, idx);
      buf = buf.slice(idx + 1);
      try {
        const req = JSON.parse(line);
        const result = handleRequest(req);
        if (result.__error) {
          conn.write(
            JSON.stringify({
              jsonrpc: "2.0",
              error: result.__error,
              id: req.id,
            }) + "\n",
          );
        } else {
          conn.write(
            JSON.stringify({
              jsonrpc: "2.0",
              result,
              id: req.id,
            }) + "\n",
          );
        }
      } catch (e) {
        conn.write(
          JSON.stringify({
            jsonrpc: "2.0",
            error: { code: -32700, message: "Parse error" },
            id: null,
          }) + "\n",
        );
      }
    }
  });
});

server.listen(SOCKET, () => {
  const addr = isUnixSocket ? SOCKET : `tcp://localhost:${SOCKET}`;
  console.log(`[greeter-service] Listening on ${addr}`);
});

// Graceful shutdown
process.on("SIGTERM", () => {
  console.log("[greeter-service] SIGTERM received, shutting down");
  server.close(() => {
    if (isUnixSocket) {
      try { fs.unlinkSync(SOCKET); } catch (_) { /* ok */ }
    }
    process.exit(0);
  });
});
