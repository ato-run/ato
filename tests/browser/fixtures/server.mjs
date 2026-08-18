import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";

const port = Number.parseInt(process.argv[2], 10);
if (!Number.isSafeInteger(port) || port <= 0) throw new Error("port is required");

const contentTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
]);
let serverCount = 0;
let incrementRequests = 0;

const server = createServer(async (request, response) => {
  response.sendDate = false;
  response.setHeader("connection", "close");
  if (request.method === "POST" && request.url === "/increment") {
    serverCount += 1;
    incrementRequests += 1;
    const body = JSON.stringify({ count: serverCount, requests: incrementRequests });
    response.writeHead(200, {
      "content-type": "application/json",
      "content-length": Buffer.byteLength(body),
    }).end(body);
    return;
  }
  if (request.method === "GET" && request.url === "/__ato_test/state") {
    const body = JSON.stringify({ count: serverCount, requests: incrementRequests });
    response.writeHead(200, {
      "content-type": "application/json",
      "content-length": Buffer.byteLength(body),
    }).end(body);
    return;
  }
  if (request.url === "/__shutdown") {
    response.writeHead(204).end();
    server.close();
    return;
  }
  const requested = request.url === "/" ? "index.html" : request.url.slice(1);
  const file = normalize(join(process.cwd(), requested));
  if (!file.startsWith(process.cwd())) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(file);
    response.writeHead(200, {
      "content-type": contentTypes.get(extname(file)) ?? "application/octet-stream",
      "content-length": body.length,
    }).end(body);
  } catch {
    response.writeHead(404, { "content-length": 0 }).end();
  }
});

server.listen(port, "127.0.0.1");
