const http = require("node:http");

const port = Number(process.env.PORT || "18880");
const marker = "Ato installed relaunch fixture";

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
      <p>install -> open -> stop -> ato://app/&lt;install_profile_key&gt; relaunch</p>
    </main>
  </body>
</html>`);
});

server.listen(port, "127.0.0.1", () => {
  console.log(`${marker} listening on http://127.0.0.1:${port}/`);
});
