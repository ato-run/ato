// Minimal Bun HTTP server for the source-node-bun-runtime-tools fixture.
//
// Uses Bun's native `Bun.serve` (so it only runs if `bun` is genuinely on the
// lifecycle PATH) and binds the port injected by the Ato runtime. Auto-shuts
// down after 30 s so the `ato run` process — and the test — exit on their own.
const port = Number(process.env.PORT ?? "0");

const server = Bun.serve({
  port,
  hostname: "127.0.0.1",
  fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/api/health") {
      return new Response(JSON.stringify({ status: "ok", runtime: "bun" }), {
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response("source-node-bun-runtime-tools-fixture\n", {
      headers: { "Content-Type": "text/plain" },
    });
  },
});

console.log(`bun server listening on 127.0.0.1:${server.port}`);

// Auto-shutdown so the ato process (and the test) can exit cleanly.
setTimeout(() => server.stop(), 30_000);
