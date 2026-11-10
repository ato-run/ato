// Saved-state round trip on a real SQLite Coordinator (G5).
//
//   COORDINATOR_BIN=.../ato-coordinator ATO_BIN=.../ato \
//   SAMPLE_HANDLER=.../shared-json-todo/src/operations/todo.js \
//   node e2e/saved-state-roundtrip.mjs
//
// Proves the export envelope travels: export-set from one authority, import-set
// into a fresh authority, identical document digests, and a tampered file is
// refused before it creates anything.
import { execFileSync, spawn } from "node:child_process";
import { mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import assert from "node:assert/strict";

const coordinator = process.env.COORDINATOR_BIN;
const ato = process.env.ATO_BIN;
const handler = process.env.SAMPLE_HANDLER;
if (!coordinator || !ato || !handler) {
  console.error("COORDINATOR_BIN, ATO_BIN and SAMPLE_HANDLER are required");
  process.exit(2);
}
const source = await readFile(handler, "utf8");
// Keep artifacts inside the checkout (never /tmp).
const work = process.env.SAVED_STATE_WORK || ".tmp/saved-state-e2e";
await rm(work, { recursive: true, force: true });
await mkdir(work, { recursive: true });

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("exit", (code) => resolve({ code, stdout, stderr }));
  });
}

async function serve(database, port, token) {
  const init = await run(coordinator, ["init", "--db", database]);
  assert.equal(init.code, 0, init.stderr);
  const child = spawn(coordinator, ["serve", "--db", database, "--addr", `127.0.0.1:${port}`, "--token", token], { stdio: "ignore" });
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`);
      if (response.ok) return child;
    } catch {
      // not listening yet
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("coordinator did not start");
}

const token = "saved-state-token";
const call = async (port, path, body) => {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: response.status, body: await response.json().catch(() => null) };
};

const first = await serve(join(work, "one.sqlite"), 8791, token);
try {
  assert.equal((await call(8791, "/v1/protocols/todo.board@1/operations", {
    operation: "todo_create", semantics: "evolution", access: "read_write",
    module_source: source, module_export: "createTodo", publisher: "alice/app",
  })).status, 201);
  const created = await call(8791, "/v1/data-resources", { protocol: "todo.board@1", document: {} });
  assert.equal(created.status, 201, JSON.stringify(created.body));
  const resourceId = created.body.resource_id;
  const binding = await call(8791, `/v1/data-resources/${resourceId}/bindings`, {
    port: "tasks", protocol: "todo.board@1", access: "operations",
    operations: ["todo_create"], instance_id: "todo-ui",
  });
  assert.equal(binding.status, 201, JSON.stringify(binding.body));
  const invoked = await call(8791, `/v1/bindings/${binding.body.binding_id}/operations`, {
    operation: "todo_create", operation_id: "saved-state-1", expected_revision: 1,
    expected_generation: 0, arguments: { title: "Travels", due_at: "2026-12-20", at: "2026-11-10T00:00:00.000Z" },
  });
  assert.equal(invoked.status, 200, JSON.stringify(invoked.body));
  assert.equal(invoked.body.revision, 2);

  // The canonical saved state is one hosted-compatible archive per Resource.
  const archiveDir = join(work, "saved-state");
  const sourceDetail = await call(8791, `/v1/data-resources/${resourceId}/document`);
  const sourceDigest = sourceDetail.body.content_digest;
  const exported = await run(ato, ["data", "resource", "export-set", "--out", archiveDir, "--coordinator", "http://127.0.0.1:8791", "--token", token]);
  assert.equal(exported.code, 0, exported.stderr);
  const archives = (await readdir(archiveDir)).filter((name) => name.endsWith(".zip"));
  assert.equal(archives.length, 1, "expected one saved-state archive");

  const second = await serve(join(work, "two.sqlite"), 8792, token);
  try {
    const imported = await run(ato, ["data", "resource", "import-set", "--file", archiveDir, "--coordinator", "http://127.0.0.1:8792", "--token", token]);
    assert.equal(imported.code, 0, imported.stderr);
    const listed = await call(8792, "/v1/data-resources");
    assert.equal(listed.body.resources.length, 1);
    const detail = await call(8792, `/v1/data-resources/${listed.body.resources[0].resource_id}/document`);
    assert.equal(detail.body.content_digest, sourceDigest);
    assert.equal(Object.values(detail.body.document.tasks)[0].title, "Travels");
    console.log("PASS: saved state imported with the same digest", sourceDigest);

    // The imported state must be live for every entry point, not just a file
    // on disk: register the handler on this authority, expose it to MCP, bind
    // the imported Resource, invoke through MCP, then read the same revision
    // through the CLI.
    const importedId = listed.body.resources[0].resource_id;
    assert.equal((await call(8792, "/v1/protocols/todo.board@1/operations", {
      operation: "todo_create", semantics: "evolution", access: "read_write",
      module_source: source, module_export: "createTodo", publisher: "alice/app",
    })).status, 201);
    assert.equal((await call(8792, "/v1/protocols/todo.board@1/operations/todo_create/expose", { mcp: true })).status, 200);
    assert.equal((await call(8792, `/v1/data-resources/${importedId}/bindings`, {
      port: "tasks", protocol: "todo.board@1", access: "operations",
      operations: ["todo_create"], instance_id: "mcp",
    })).status, 201);
    const mcp = await call(8792, "/mcp", {
      jsonrpc: "2.0", id: 1, method: "tools/call",
      params: { name: "todo_create", arguments: { title: "After restore", due_at: "2026-12-21", at: "2026-11-10T00:00:00.000Z" } },
    });
    assert.equal(mcp.body.result.isError, false, JSON.stringify(mcp.body));
    const mcpRevision = JSON.parse(mcp.body.result.content[0].text).revision;
    assert.equal(mcpRevision, 2, "MCP must commit on top of the imported revision 1");
    const inspected = await run(ato, ["data", "resource", "inspect", importedId, "--coordinator", "http://127.0.0.1:8792", "--token", token]);
    assert.equal(inspected.code, 0, inspected.stderr);
    assert.match(inspected.stdout, new RegExp(`revision: ${mcpRevision}\\b`), inspected.stdout);
    console.log("PASS: imported state is live for CLI and MCP on the same revision", mcpRevision);

    const archivePath = join(archiveDir, archives[0]);
    const tamperDir = join(work, "tamper");
    execFileSync("unzip", ["-o", "-q", archivePath, "-d", tamperDir]);
    const dataPath = join(tamperDir, "data.json");
    const tamperedDocument = JSON.parse(await readFile(dataPath, "utf8"));
    const taskId = Object.keys(tamperedDocument.tasks)[0];
    tamperedDocument.tasks[taskId].title = "Tampered";
    await writeFile(dataPath, JSON.stringify(tamperedDocument));
    await rm(archivePath);
    execFileSync("zip", ["-0", "-q", "-j", archivePath, join(tamperDir, "metadata.json"), dataPath]);
    const tampered = await run(ato, ["data", "resource", "import-set", "--file", archiveDir, "--coordinator", "http://127.0.0.1:8792", "--token", token]);
    assert.notEqual(tampered.code, 0, "tampered import must fail");
    assert.match(tampered.stderr, /export_digest_mismatch/);
    const afterTamper = await call(8792, "/v1/data-resources");
    assert.equal(afterTamper.body.resources.length, 1, "tampered import created a resource");
    console.log("PASS: tampered saved state refused before creating anything");
  } finally {
    second.kill();
  }
} finally {
  first.kill();
}
