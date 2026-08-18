import { createReadStream } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";

const port = Number.parseInt(process.argv[2], 10);
if (!Number.isSafeInteger(port) || port <= 0) throw new Error("port is required");

const contentTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
]);

const server = createServer((request, response) => {
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
  response.writeHead(200, { "content-type": contentTypes.get(extname(file)) ?? "application/octet-stream" });
  createReadStream(file).on("error", () => response.writeHead(404).end()).pipe(response);
});

server.listen(port, "127.0.0.1");
