// SQLite vertical browser E2E: TODO (Resource writer) + Calendar (operation-only
// reader) served by two selfhost Coordinators sharing one SQLite database.
//
// Prerequisites: a built ato-coordinator binary, the ato-samples checkout for
// shared-json-todo, the ato-api fixture calendar app, and Playwright available
// on NODE_PATH. Run:
//
//   NODE_PATH=<repo>/node_modules node services/coordinator/e2e/sqlite-vertical.cjs
//
// The paths below assume this repository's workspace layout; override them with
// the environment variables when running elsewhere.
const { chromium } = require('playwright');
const { spawn, execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const WORK = process.env.ATO_VERTICAL_WORK || path.join(process.cwd(), '.tmp/sqlite-vertical');
const COORD = process.env.ATO_COORDINATOR_BIN || 'target/debug/ato-coordinator';
const DB = path.join(WORK, 'coordinator.sqlite');
const TODO_BASE = 'http://127.0.0.1:8790';
const CAL_BASE = 'http://127.0.0.1:8789';
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  fs.rmSync(WORK, { recursive: true, force: true });
  fs.mkdirSync(path.join(WORK, 'app'), { recursive: true });
  fs.mkdirSync(path.join(WORK, 'calendar'), { recursive: true });
  fs.cpSync(process.env.ATO_SAMPLES_TODO || path.join(process.cwd(), '../../ato-samples/samples/shared-json-todo'), path.join(WORK, 'app'), { recursive: true });
  fs.cpSync(process.env.ATO_CALENDAR_APP || path.join(process.cwd(), '../../ato-samples/samples/shared-json-calendar'), path.join(WORK, 'calendar'), { recursive: true });
  execFileSync(COORD, ['init', '--db', DB]);

  const todoServer = spawn(COORD, ['serve', '--db', DB, '--addr', '127.0.0.1:8790', '--token', 'smoke', '--apps-dir', path.join(WORK, 'app'), '--app-instance', 'todo-ui', '--app-port', 'tasks'], { stdio: 'ignore' });
  await wait(900);
  const call = async (base, method, p, body, auth = true) => {
    const response = await fetch(base + p, {
      method,
      headers: { ...(body ? { 'content-type': 'application/json' } : {}), ...(auth ? { authorization: 'Bearer smoke' } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    return [response.status, await response.text()];
  };
  let calendarServer = null;
  try {
    const source = fs.readFileSync(path.join(WORK, 'app/src/operations/todo.js'), 'utf8');
    for (const [operation, semantics, access, exportName] of [
      ['todo_board', 'observation', 'read', 'boardDocument'],
      ['todo_create', 'evolution', 'read_write', 'createTodo'],
      ['todo_list_due_dates', 'observation', 'read', 'listDueDates'],
    ]) {
      const [status, body] = await call(TODO_BASE, 'POST', '/v1/protocols/todo.board@1/operations', { operation, semantics, access, module_source: source, module_export: exportName, publisher: 'alice/app' });
      if (status !== 201) throw new Error('register ' + operation + ': ' + status + ' ' + body);
    }
    const [, createdBody] = await call(TODO_BASE, 'POST', '/v1/data-resources', { protocol: 'todo.board@1', document: {} });
    const resourceId = JSON.parse(createdBody).resource_id;
    await call(TODO_BASE, 'POST', '/v1/data-resources/' + resourceId + '/bindings', { port: 'tasks', protocol: 'todo.board@1', access: 'operations', operations: ['todo_board', 'todo_create', 'todo_list_due_dates'], instance_id: 'todo-ui' });
    for (const [key, title, due, done] of [
      ['seed-1', 'Due task one', '2026-11-20', false],
      ['seed-2', 'Due task two', '2026-11-25', false],
      ['seed-3', 'No date task', null, false],
    ]) {
      const [status, body] = await call(TODO_BASE, 'POST', '/__ato/data-ports/tasks/invoke', {
        operation: 'todo_create',
        arguments: { title, due_at: due, at: '2026-11-10T00:00:00.000Z' },
        idempotency_key: key,
      });
      if (status !== 200) throw new Error('seed: ' + body);
    }
    // Calendar binds to the same Resource with only the due-date projection.
    const [, calendarBindingBody] = await call(TODO_BASE, 'POST', '/v1/data-resources/' + resourceId + '/bindings', {
      port: 'tasks', protocol: 'todo.board@1', access: 'operations', operations: ['todo_list_due_dates'], instance_id: 'calendar-ui',
    });
    const calendarBinding = JSON.parse(calendarBindingBody);

    calendarServer = spawn(COORD, ['serve', '--db', DB, '--addr', '127.0.0.1:8789', '--token', 'smoke', '--apps-dir', path.join(WORK, 'calendar'), '--app-instance', 'calendar-ui', '--app-port', 'tasks'], { stdio: 'ignore' });
    await wait(900);

    const browser = await chromium.launch({ headless: true });
    const errors = [];
    const todoPage = await browser.newPage({ viewport: { width: 1100, height: 800 } });
    todoPage.on('pageerror', (error) => errors.push('todo: ' + error));
    await todoPage.goto(TODO_BASE + '/', { waitUntil: 'load' });
    await todoPage.waitForTimeout(1200);
    await todoPage.screenshot({ path: path.join(WORK, 'todo.png'), fullPage: true });
    const todoText = await todoPage.locator('body').innerText();
    console.log('TODO shows seeded task:', todoText.includes('Due task one'));
    console.log('TODO shows second due task:', todoText.includes('Due task two'));

    const calendarPage = await browser.newPage({ viewport: { width: 800, height: 600 } });
    calendarPage.on('pageerror', (error) => errors.push('calendar: ' + error));
    await calendarPage.goto(CAL_BASE + '/', { waitUntil: 'load' });
    await calendarPage.getByText('共有された期日: 2').waitFor({ timeout: 5000 });
    await calendarPage.screenshot({ path: path.join(WORK, 'calendar.png'), fullPage: true });
    console.log('Calendar due dates:', await calendarPage.locator('#due li').allTextContents());

    // Revoke the Calendar binding: every later invoke must be refused.
    const [revokeStatus] = await call(TODO_BASE, 'DELETE', '/v1/bindings/' + calendarBinding.binding_id, { binding_epoch: calendarBinding.binding_epoch });
    console.log('revoke status:', revokeStatus);
    await calendarPage.reload({ waitUntil: 'load' });
    await calendarPage.getByText('期日を取得できません').waitFor({ timeout: 5000 });
    console.log('Calendar after revoke:', await calendarPage.locator('#status').innerText());

    // TODO keeps working and its revision is untouched by the revocation.
    const [, resourceBody] = await call(TODO_BASE, 'GET', '/v1/data-resources/' + resourceId, null);
    console.log('resource revision after revoke:', JSON.parse(resourceBody).revision);
    await todoPage.reload({ waitUntil: 'load' });
    await todoPage.waitForTimeout(1000);
    console.log('TODO still shows task after revoke:', (await todoPage.locator('body').innerText()).includes('Due task one'));
    console.log('page errors:', errors.length ? errors : 'none');
    await browser.close();
  } finally {
    try { await call(TODO_BASE, 'POST', '/control/shutdown', null); } catch {}
    try { await call(CAL_BASE, 'POST', '/control/shutdown', null); } catch {}
    todoServer.kill();
    if (calendarServer) calendarServer.kill();
  }
})().catch((error) => { console.error(error); process.exit(1); });
