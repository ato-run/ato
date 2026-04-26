#!/usr/bin/env node
/**
 * greeter-client — Client for the greeter-service IPC service.
 *
 * Demonstrates capsule IPC consumption:
 * 1. Reads socket path from CAPSULE_IPC_GREETER_SOCKET
 * 2. Sends JSON-RPC 2.0 requests over the Unix socket
 * 3. Prints the responses
 *
 * Usage (inside capsule runtime):
 *   ato open .
 *   # The IPC broker auto-starts greeter-service and injects env vars
 */

import net from "node:net";
import process from "node:process";

const socketEnv = process.env.CAPSULE_IPC_GREETER_SOCKET;
const DEFAULT_PORT = 50051;
// Use TCP port when env var is a number or unset; use Unix socket when it's a path
const SOCKET = socketEnv && isNaN(Number(socketEnv)) ? socketEnv : Number(socketEnv) || DEFAULT_PORT;

function rpcCall(method, params) {
  return new Promise((resolve, reject) => {
    const conn = net.createConnection(SOCKET, () => {
      const req = {
        jsonrpc: "2.0",
        method,
        params,
        id: Date.now(),
      };
      conn.write(JSON.stringify(req) + "\n");
    });

    let buf = "";
    conn.on("data", (chunk) => {
      buf += chunk.toString();
      const idx = buf.indexOf("\n");
      if (idx >= 0) {
        try {
          const resp = JSON.parse(buf.slice(0, idx));
          conn.end();
          if (resp.error) {
            reject(new Error(`${resp.error.code}: ${resp.error.message}`));
          } else {
            resolve(resp.result);
          }
        } catch (e) {
          conn.end();
          reject(e);
        }
      }
    });

    conn.on("error", reject);
    conn.setTimeout(5000, () => {
      conn.end();
      reject(new Error("Connection timed out"));
    });
  });
}

async function main() {
  console.log(`[greeter-client] Connecting to ${SOCKET}`);

  try {
    // Call health
    const health = await rpcCall("health");
    console.log("[greeter-client] Health:", JSON.stringify(health));

    // Call greet
    const greeting = await rpcCall("greet", { name: "Capsule" });
    console.log("[greeter-client] Greeting:", JSON.stringify(greeting));

    // Call greet with default
    const defaultGreeting = await rpcCall("greet");
    console.log("[greeter-client] Default:", JSON.stringify(defaultGreeting));

    console.log("[greeter-client] All calls succeeded ✓");
  } catch (err) {
    console.error("[greeter-client] Error:", err.message);
    process.exit(1);
  }
}

main();
