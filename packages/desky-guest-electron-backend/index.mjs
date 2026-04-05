import fs from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";

export function createGuestContext({ adapter, defaultPort, sampleRoot }) {
  return {
    sampleRoot,
    adapter: process.env.DESKY_SESSION_ADAPTER || adapter,
    sessionId: process.env.DESKY_SESSION_ID || "desky-session",
    guestMode: process.env.ATO_GUEST_MODE === "1" ? "1" : null,
    host: process.env.DESKY_SESSION_HOST || "127.0.0.1",
    port: Number.parseInt(
      process.env.DESKY_SESSION_PORT || String(defaultPort),
      10,
    ),
    bindAddr() {
      return `${this.host}:${this.port}`;
    },
    isGuestMode() {
      return this.guestMode === "1";
    },
    checkEnv() {
      return {
        ok: true,
        adapter: this.adapter,
        session_id: this.sessionId,
        ato_guest_mode: this.guestMode,
      };
    },
    ping(message = "") {
      return {
        ok: true,
        adapter: this.adapter,
        session_id: this.sessionId,
        command: "ping",
        echo: message,
      };
    },
    resolveAllowedPath(relativePath) {
      const candidate = path.resolve(this.sampleRoot, relativePath);
      if (
        candidate !== this.sampleRoot &&
        !candidate.startsWith(`${this.sampleRoot}${path.sep}`)
      ) {
        throw new Error(
          `BoundaryPolicyError: Guest file read is outside the allowed root: ${relativePath}`,
        );
      }
      return candidate;
    },
  };
}

export function builtinResult(context, command, payload = {}) {
  if (command === "check_env") {
    return context.checkEnv();
  }

  if (command === "ping") {
    return context.ping(messageFromPayload(payload));
  }

  return null;
}

export function messageFromPayload(payload = {}) {
  return typeof payload?.message === "string" ? payload.message : "";
}

export function createElectronGuestRuntime({ app, ipcMain, context }) {
  const commandHandlers = new Map();
  const ipcHandlers = new Map();
  let mainWindow = null;
  let rpcServer = null;

  async function executeCommand(command, payload = {}) {
    const builtin = builtinResult(context, command, payload);
    if (builtin) {
      return builtin;
    }

    const handler = commandHandlers.get(command);
    if (handler) {
      return handler(payload, runtime);
    }

    return {
      ok: true,
      adapter: context.adapter,
      session_id: context.sessionId,
      command,
      echo: messageFromPayload(payload),
    };
  }

  function jsonResponse(res, statusCode, payload) {
    const body = JSON.stringify(payload);
    res.writeHead(statusCode, {
      "Content-Type": "application/json",
      "Content-Length": Buffer.byteLength(body),
    });
    res.end(body);
  }

  function createRpcServer() {
    return createServer(async (req, res) => {
      if (req.method === "GET" && req.url === "/health") {
        jsonResponse(res, 200, {
          ok: true,
          adapter: context.adapter,
          session_id: context.sessionId,
          guest_mode: context.guestMode,
        });
        return;
      }

      if (req.method !== "POST" || req.url !== "/rpc") {
        jsonResponse(res, 404, { ok: false, error: "not_found" });
        return;
      }

      try {
        const chunks = [];
        for await (const chunk of req) {
          chunks.push(chunk);
        }
        const raw = Buffer.concat(chunks).toString("utf8") || "{}";
        const request = JSON.parse(raw);
        const params = request.params || {};
        const result = await executeCommand(
          params.command || "unknown",
          params.payload || {},
        );
        jsonResponse(res, 200, {
          jsonrpc: "2.0",
          id: request.id,
          result,
        });
      } catch (error) {
        jsonResponse(res, 500, {
          jsonrpc: "2.0",
          error: String(error),
        });
      }
    });
  }

  function registerDefaultIpcHandlers() {
    ipcMain.handle("desky:ping", async (_event, payload = {}) =>
      executeCommand("ping", payload),
    );

    ipcMain.handle("desky:invoke", async (_event, payload = {}) =>
      executeCommand(payload.command || "unknown", payload.payload || {}),
    );

    for (const [channel, handler] of ipcHandlers.entries()) {
      ipcMain.handle(channel, async (_event, payload = {}) =>
        handler(payload, runtime),
      );
    }
  }

  function shutdown() {
    if (rpcServer) {
      rpcServer.close();
    }
    if (!app.isQuiting) {
      app.quit();
    }
  }

  const runtime = {
    context,
    executeCommand,
    registerCommand(command, handler) {
      commandHandlers.set(command, handler);
    },
    registerIpcChannel(channel, handler) {
      ipcHandlers.set(channel, handler);
    },
    resolveAllowedPath(relativePath) {
      return context.resolveAllowedPath(relativePath);
    },
    async readWorkspaceFile(relativePath, encoding = "utf8") {
      const resolved = context.resolveAllowedPath(relativePath);
      return fs.readFile(resolved, encoding);
    },
    getMainWindow() {
      return mainWindow;
    },
    setMainWindow(window) {
      mainWindow = window;
      return mainWindow;
    },
    start({ createWindow }) {
      app.whenReady().then(() => {
        registerDefaultIpcHandlers();
        rpcServer = createRpcServer();
        rpcServer.listen(context.port, context.host);

        if (!context.isGuestMode() && typeof createWindow === "function") {
          createWindow(runtime);
        }
      });

      app.on("window-all-closed", () => {
        if (!context.isGuestMode()) {
          app.quit();
        }
      });

      app.on("before-quit", () => {
        app.isQuiting = true;
        if (rpcServer) {
          rpcServer.close();
        }
      });

      process.on("SIGTERM", shutdown);
      process.on("SIGINT", shutdown);
    },
  };

  return runtime;
}
