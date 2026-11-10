// Selective team sharing on a real SQLite Coordinator.
//
// Owner marks one task shared; a consumer instance whose Port grants only
// todo_list_shared sees exactly that task and only its selected fields.
// Unsharing revokes visibility on the next call while the board and its
// revision continue to exist.
const BASE = "http://127.0.0.1:8799";
const token = "smoke";
const at = "2026-11-10T00:00:00.000Z";
const call = async (path, body) => {
  const response = await fetch(BASE + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { parsed = text; }
  return { status: response.status, body: parsed };
};

const board = {
  tasks: {
    a: { id: "a", title: "Team visible", description: "internal notes", columnId: "todo", dueAt: "2026-12-01", shared: true, order: 1000, createdAt: at, updatedAt: at },
    b: { id: "b", title: "Private", description: "secret", columnId: "todo", dueAt: "2026-12-02", shared: false, order: 2000, createdAt: at, updatedAt: at },
  },
  columns: [{ id: "todo", title: "Todo", order: 1000 }],
  revision: 1,
};

import { readFile } from "node:fs/promises";
const source = await readFile(process.env.SAMPLE_HANDLER, "utf8");
for (const [operation, semantics, access, moduleExport] of [
  ["todo_list_shared", "observation", "read", "listSharedTasks"],
  ["todo_set_shared", "evolution", "read_write", "setTaskSharing"],
]) {
  const registered = await call("/v1/protocols/todo.board@1/operations", {
    operation, semantics, access, module_source: source, module_export: moduleExport, publisher: "alice/app",
  });
  if (registered.status !== 201) throw new Error("register failed: " + JSON.stringify(registered.body));
}
const created = await call("/v1/data-resources", { protocol: "todo.board@1", document: board });
const resourceId = created.body.resource_id;
const consumer = await call(`/v1/data-resources/${resourceId}/bindings`, {
  port: "tasks", protocol: "todo.board@1", access: "operations",
  operations: ["todo_list_shared"], instance_id: "teammate-ui",
});
const consumerId = consumer.body.binding_id;
const owner = await call(`/v1/data-resources/${resourceId}/bindings`, {
  port: "tasks", protocol: "todo.board@1", access: "operations",
  operations: ["todo_set_shared"], instance_id: "owner-ui",
});
const ownerId = owner.body.binding_id;

const sharedView = async () => call(`/v1/bindings/${consumerId}/operations`, {
  operation: "todo_list_shared", operation_id: "view-" + Math.random(),
  expected_revision: (await call(`/v1/data-resources/${resourceId}/document`)).body.revision,
  expected_generation: 0, arguments: {},
});
const first = await sharedView();
if (first.status !== 200) throw new Error("shared view failed: " + JSON.stringify(first.body));
const tasks = first.body.result.tasks;
console.log("consumer sees:", JSON.stringify(tasks));
if (tasks.length !== 1 || tasks[0].task_id !== "a") throw new Error("consumer saw the wrong tasks");
if ("description" in tasks[0] || "subtasks" in tasks[0]) throw new Error("consumer saw private fields");

const unlisted = await call(`/v1/bindings/${consumerId}/operations`, {
  operation: "todo_set_shared", operation_id: "deny-1", expected_revision: 1, expected_generation: 0,
  arguments: { id: "a", shared: false, at },
});
if (unlisted.status === 200) throw new Error("consumer invoked an ungranted operation");
console.log("consumer invoke unlisted operation:", unlisted.status, JSON.stringify(unlisted.body));

const documentRead = await call(`/v1/data-resources/${resourceId}/document`);
if (documentRead.status !== 200) throw new Error("owner document read failed");

const unshare = await call(`/v1/bindings/${ownerId}/operations`, {
  operation: "todo_set_shared", operation_id: "unshare-1", expected_revision: documentRead.body.revision,
  expected_generation: documentRead.body.generation, arguments: { id: "a", shared: false, at },
});
if (unshare.status !== 200) throw new Error("unshare failed: " + JSON.stringify(unshare.body));
console.log("owner unshared a: revision", unshare.body.revision);
const after = await sharedView();
if (after.body.result.tasks.length !== 0) throw new Error("consumer still sees the unshared task");
console.log("consumer sees after unshare:", JSON.stringify(after.body.result.tasks));
const kept = await call(`/v1/data-resources/${resourceId}/document`);
if (!kept.body.document.tasks.a) throw new Error("the task disappeared from the board");
console.log("board keeps both tasks, revision", kept.body.revision, "digest", kept.body.content_digest.slice(0, 20));

const reshare = await call(`/v1/bindings/${ownerId}/operations`, {
  operation: "todo_set_shared", operation_id: "reshare-1", expected_revision: kept.body.revision,
  expected_generation: kept.body.generation, arguments: { id: "a", shared: true, at },
});
if (reshare.status !== 200) throw new Error("reshare failed");
const again = await sharedView();
if (again.body.result.tasks.length !== 1) throw new Error("resharing did not restore visibility");
console.log("PASS: selective sharing is operation-scoped and revocable; revision", again.body.result.tasks.length, "task visible");
