#!/usr/bin/env node
import fs from "node:fs";

const argv = process.argv.slice(2);
if (argv.includes("--help")) {
  console.log("demo-npm-single-bin help");
  process.exit(0);
}

const inputPath = argv[0];
const outputIndex = argv.indexOf("-o");
if (!inputPath || outputIndex === -1 || outputIndex + 1 >= argv.length) {
  console.error("usage: demo-npm-single-bin <input> -o <output>");
  process.exit(2);
}

const outputPath = argv[outputIndex + 1];
const payload = {
  cwd: process.cwd(),
  argv,
  inputExists: fs.existsSync(inputPath),
  content: fs.readFileSync(inputPath, "utf8")
};
fs.writeFileSync(outputPath, JSON.stringify(payload));
console.log(JSON.stringify(payload));