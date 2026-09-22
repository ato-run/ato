// The origin boundary: a proxy that refuses everything, and records what it
// refused.
//
// Chrome is started with this proxy for ALL traffic and exactly one bypass:
// the realized candidate's `host:port`. `<-loopback>` removes Chrome's implicit
// loopback bypass, so another port on 127.0.0.1, `localhost`, `[::1]`, a LAN
// address, the internet, a WebSocket or a redirect target — every request
// that is not to the candidate — is sent here instead, answered 403 and never
// forwarded. The decision is made by Chrome's network stack before any
// connection to the destination exists; nothing here dials out.

import { createServer, type Server, type Socket } from "node:net";

import { bound } from "./protocol.ts";

export interface RefusedRequest {
  /// The request target as the browser sent it: an absolute URL for plain
  /// HTTP, `host:port` for CONNECT (TLS, WebSocket over TLS).
  target: string;
  method: string;
}

const MAX_RECORDED = 64;
const MAX_HEAD_BYTES = 8 * 1024;

export class OriginGuard {
  readonly refused: RefusedRequest[] = [];
  refusedTotal = 0;

  private constructor(
    private readonly server: Server,
    readonly port: number,
    private readonly onRefused: (request: RefusedRequest) => void,
  ) {}

  /// `onRefused` sees every refused request as it happens.
  static async start(onRefused: (request: RefusedRequest) => void = () => {}): Promise<OriginGuard> {
    const sockets = new Set<Socket>();
    let guard: OriginGuard | null = null;
    const server = createServer((socket) => {
      sockets.add(socket);
      socket.on("close", () => sockets.delete(socket));
      socket.on("error", () => {});
      let head = "";
      let answered = false;
      socket.on("data", (chunk) => {
        if (answered) return;
        head += chunk.toString("latin1");
        const end = head.indexOf("\r\n");
        if (end < 0 && head.length < MAX_HEAD_BYTES) return;
        answered = true;
        const [method = "", target = ""] = head.slice(0, end < 0 ? MAX_HEAD_BYTES : end).split(" ");
        guard?.record({ method: bound(method, 16), target: bound(target, 1024) });
        socket.end("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
      });
    });
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => resolve());
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("guard_proxy_unavailable");
    guard = new OriginGuard(server, address.port, onRefused);
    server.on("close", () => sockets.forEach((s) => s.destroy()));
    return guard;
  }

  private record(request: RefusedRequest) {
    this.refusedTotal++;
    this.onRefused(request);
    if (this.refused.length < MAX_RECORDED) this.refused.push(request);
  }

  /// Chrome flags that route everything but `targetOrigin` through the guard.
  chromeArgs(targetOrigin: string): string[] {
    const target = new URL(targetOrigin);
    return [
      `--proxy-server=http://127.0.0.1:${this.port}`,
      `--proxy-bypass-list=<-loopback>;${target.host}`,
    ];
  }

  async close(): Promise<void> {
    await new Promise<void>((resolve) => this.server.close(() => resolve()));
  }
}
