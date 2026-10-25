#!/usr/bin/env node
// What Stagehand starts as "the browser" inside the verifier sandbox.
//
// It starts nothing itself. It asks the worker, over the socket in the
// scratch directory, to start the real browser with these arguments in a
// sandbox of the worker's own — beside this one, not inside it: no
// environment from here (the helper holds model keys), no view of the
// helper's processes, the same scratch directory for the profile. The worker
// chooses the executable and the sandbox; this sends only the arguments.
//
// This process stands in for the browser: it lives exactly as long as the
// browser does and exits with its code. Killing it — as the browser launcher
// does to stop "the browser" — closes the socket, and the worker stops the
// real browser.
"use strict";
const net = require("node:net");

const socketPath = process.env.ATO_BROWSER_LAUNCHER_SOCKET;
if (!socketPath) {
  process.stderr.write("chrome-contained: no browser launcher socket\n");
  process.exit(1);
}
const socket = net.connect(socketPath);
socket.on("error", (error) => {
  process.stderr.write(`chrome-contained: ${error.message}\n`);
  process.exit(1);
});
socket.write(JSON.stringify({ args: process.argv.slice(2) }) + "\n");
let answer = "";
socket.on("data", (chunk) => {
  answer += chunk.toString("utf8");
  const exited = answer.match(/^exit (-?\d+)$/m);
  if (exited) process.exit(Number(exited[1]));
  const refused = answer.match(/^refused (.*)$/m);
  if (refused) {
    process.stderr.write(`chrome-contained: refused: ${refused[1]}\n`);
    process.exit(1);
  }
});
socket.on("close", () => process.exit(1));
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  process.on(signal, () => {
    socket.destroy();
    process.exit(143);
  });
}
