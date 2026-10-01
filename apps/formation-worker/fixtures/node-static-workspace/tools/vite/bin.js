#!/usr/bin/env node
// Fixture-local build tool (not Vite): `build` copies index.html to dist/.
const fs = require("fs");
if (process.argv[2] !== "build") {
  console.error("fixture tool supports only `build`");
  process.exit(2);
}
fs.mkdirSync("dist", { recursive: true });
fs.copyFileSync("index.html", "dist/index.html");
console.log("fixture build wrote dist/index.html");
