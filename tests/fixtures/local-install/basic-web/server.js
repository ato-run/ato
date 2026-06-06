const http = require("node:http");

const port = Number(process.env.PORT || "18890");
const marker = "Ato local-install basic-web fixture";

const server = http.createServer((request, response) => {
  if (request.url === "/health") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ ok: true, marker }));
    return;
  }

  response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  response.end(`<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <title>${marker}</title>
  </head>
  <body>
    <main>
      <h1>${marker}</h1>
      <p>ato install --from-local -> ato launch &lt;ipk&gt; -> Ready</p>
    </main>
  </body>
</html>`);
});

server.listen(port, "127.0.0.1", () => {
  console.log(`${marker} listening on http://127.0.0.1:${port}/`);
});
