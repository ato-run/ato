// One Task Board instance, two Resources: personal (port "tasks") and team
// (port "team_tasks"). Verifies independent bindings, isolation, CAS conflict
// and revoke, on the real SQLite Coordinator.
import { readFile } from "node:fs/promises";
const BASE = "http://127.0.0.1:8801";
const token = "smoke";
const at = "2026-11-10T00:00:00.000Z";
const call = async (method, path, body) => {
  const response = await fetch(BASE + path, {
    method,
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { parsed = text; }
  return { status: response.status, body: parsed };
};
const source = await readFile(process.env.SAMPLE_HANDLER, "utf8");
for (const [operation, semantics, access, moduleExport] of [
  ["todo_list", "observation", "read", "listTodos"],
  ["todo_create", "evolution", "read_write", "createTodo"],
]) {
  const registered = await call("POST", "/v1/protocols/todo.board@1/operations", {
    operation, semantics, access, module_source: source, module_export: moduleExport, publisher: "alice/app",
  });
  if (registered.status !== 201) throw new Error("register failed: " + JSON.stringify(registered.body));
}
const personal = await call("POST", "/v1/data-resources", {
  protocol: "todo.board@1",
  document: { tasks: { p1: { id: "p1", title: "Personal only", columnId: "todo", order: 1000, createdAt: at, updatedAt: at } }, columns: [{ id: "todo", title: "Todo", order: 1000 }], revision: 1 },
});
const team = await call("POST", "/v1/data-resources", {
  protocol: "todo.board@1",
  document: { tasks: { t1: { id: "t1", title: "Team ticket", columnId: "todo", order: 1000, createdAt: at, updatedAt: at } }, columns: [{ id: "todo", title: "Todo", order: 1000 }], revision: 1 },
});
const bind = async (resourceId, port) => {
  const bound = await call("POST", `/v1/data-resources/${resourceId}/bindings`, {
    port, protocol: "todo.board@1", access: "operations", operations: ["todo_list", "todo_create"], instance_id: "board-ui",
  });
  if (bound.status !== 201) throw new Error("bind failed: " + JSON.stringify(bound.body));
  return bound.body.binding_id;
};
const personalBinding = await bind(personal.body.resource_id, "tasks");
const teamBinding = await bind(team.body.resource_id, "team_tasks");
const list = async (bindingId) => {
  const document = await call("GET", `/v1/data-resources/${bindingId === teamBinding ? team.body.resource_id : personal.body.resource_id}/document`);
  const invoked = await call("POST", `/v1/bindings/${bindingId}/operations`, {
    operation: "todo_list", operation_id: `list-${Math.random()}`, expected_revision: document.body.revision, expected_generation: document.body.generation, arguments: {},
  });
  if (invoked.status !== 200) throw new Error("invoke failed: " + JSON.stringify(invoked.body));
  return invoked.body.result.tasks.map((task) => task.id).sort();
};
const personalTasks = await list(personalBinding);
const teamTasks = await list(teamBinding);
console.log("personal binding sees:", personalTasks);
console.log("team binding sees:", teamTasks);
if (personalTasks.join() !== "p1" || teamTasks.join() !== "t1") throw new Error("bindings are not isolated");
const teamBefore = await call("GET", `/v1/data-resources/${team.body.resource_id}/document`);
const created = await call("POST", `/v1/bindings/${teamBinding}/operations`, {
  operation: "todo_create", operation_id: "create-1", expected_revision: teamBefore.body.revision, expected_generation: teamBefore.body.generation, arguments: { title: "Team only write", at },
});
if (created.status !== 200) throw new Error("team create failed: " + JSON.stringify(created.body));
const personalAfter = await call("GET", `/v1/data-resources/${personal.body.resource_id}/document`);
if (Object.keys(personalAfter.body.document.tasks).length !== 1) throw new Error("personal board changed");
console.log("team write -> team revision", created.body.revision, "/ personal revision", personalAfter.body.revision);
const conflict = await call("POST", `/v1/bindings/${teamBinding}/operations`, {
  operation: "todo_create", operation_id: "create-2", expected_revision: teamBefore.body.revision, expected_generation: teamBefore.body.generation, arguments: { title: "Stale write", at },
});
console.log("stale write:", conflict.status, conflict.body.error);
if (conflict.status !== 409) throw new Error("CAS conflict was not refused");
const revoked = await call("DELETE", `/v1/bindings/${teamBinding}`);
if (revoked.status !== 200) throw new Error("revoke failed: " + JSON.stringify(revoked.body));
const afterRevoke = await call("POST", `/v1/bindings/${teamBinding}/operations`, {
  operation: "todo_list", operation_id: "after-revoke", expected_revision: created.body.revision, expected_generation: 0, arguments: {},
});
console.log("team invoke after revoke:", afterRevoke.status, afterRevoke.body.error);
if (afterRevoke.status === 200) throw new Error("revoked binding still invoked");
const personalStill = await list(personalBinding);
if (personalStill.join() !== "p1") throw new Error("personal binding broke after team revoke");
console.log("PASS: one instance, two Resources, isolated bindings, CAS, revoke");
