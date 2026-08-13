const http = require("http");
http
  .createServer((_req, res) => {
    // 200 on every path — the synthesized probe GETs "/".
    res.writeHead(200);
    res.end("ok");
  })
  .listen(8080, "0.0.0.0");
