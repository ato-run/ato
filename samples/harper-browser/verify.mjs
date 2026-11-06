import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

// Browser evidence only. The Rust Formation authority must separately bind
// this acceptance task to K and the exact attempt before it can be a receipt.
export async function verifyHarper(page, { url, directory }) {
  const target = new URL(url);
  const requests = [];
  const sockets = [];
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => {
    const u = new URL(request.url());
    const record = {
      origin: u.origin, path: u.pathname, method: request.method(),
      text_present: /This(?:%20|\+| )is(?:%20|\+| )a(?:%20|\+| )apple/i.test(request.url()) ||
        /This is (?:a|an) apple/.test(request.postData() ?? ''),
      finished: false, failure: null,
    };
    requests.push(record);
    request.response().then(response => {
      if (!response) return;
      response.finished().then(failure => {
        record.finished = !failure;
        record.failure = failure?.message ?? null;
      });
    }).catch(() => {});
  });
  page.on('requestfailed', request => {
    const record = requests.findLast(item => item.path === new URL(request.url()).pathname && !item.finished);
    if (record) record.failure = request.failure()?.errorText ?? 'failed';
  });
  page.on('websocket', socket => socket.on('framesent', frame => {
    sockets.push({ origin: new URL(socket.url()).origin,
      text_present: /This is (?:a|an) apple/.test(String(frame.payload)) });
  }));
  await mkdir(directory, { recursive: true });
  await page.goto(url, { waitUntil: 'networkidle' });
  const status = page.getByRole('status');
  await status.getByText('Ready', { exact: true }).waitFor({ timeout: 60000 });
  const input = page.getByRole('textbox', { name: 'English text' });
  async function correct() {
    await input.fill('This is a apple.');
    await page.getByRole('button', { name: 'Check grammar', exact: true }).click();
    await page.getByText('Incorrect indefinite article.', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Apply: an', exact: true }).click();
    await status.getByText('0 issues', { exact: true }).waitFor();
    assert.equal(await input.inputValue(), 'This is an apple.');
  }
  await correct();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await status.getByText('Saved in this browser', { exact: true }).waitFor();
  const downloadPending = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download text', exact: true }).click();
  const download = await downloadPending;
  await download.saveAs(`${directory}/harper-text.txt`);
  assert.equal(await readFile(`${directory}/harper-text.txt`, 'utf8'), 'This is an apple.');
  await page.reload({ waitUntil: 'networkidle' });
  await status.getByText('Ready', { exact: true }).waitFor();
  assert.equal(await input.inputValue(), 'This is an apple.');
  await page.context().setOffline(true);
  try { await correct(); }
  finally { await page.context().setOffline(false); }
  assert.deepEqual(errors, []);
  assert(requests.every(r => r.failure || r.origin === target.origin));
  assert(requests.every(r => r.failure || ['GET', 'HEAD'].includes(r.method) ||
    r.method === 'POST' && r.origin === target.origin && r.path.startsWith('/__ato/')));
  assert(requests.every(r => !r.text_present));
  assert(sockets.every(r => !r.text_present));
  await page.screenshot({ path: `${directory}/corrected.png`, fullPage: true });
  const result = {
    kind: 'browser-evidence', rust_contract_receipt: false,
    grammar_corrected: true, save_reload: true, download_bytes_match: true,
    offline_grammar: true, external_text_requests: 0, page_errors: errors,
    requests, sockets, checked_at: new Date().toISOString(),
  };
  await writeFile(`${directory}/browser-evidence.json`, JSON.stringify(result, null, 2) + '\n');
  return result;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const url = process.env.HARPER_URL ?? 'http://127.0.0.1:8767/';
  const target = new URL(url);
  assert.equal(target.origin, 'http://127.0.0.1:8767', 'This entry point is a local preflight only.');
  const directory = fileURLToPath(new URL('.tmp/browser/', import.meta.url));
  const browser = await chromium.launch({ channel: 'chrome', headless: false, chromiumSandbox: true });
  try {
    const context = await browser.newContext({ acceptDownloads: true, serviceWorkers: 'block' });
    const result = await verifyHarper(await context.newPage(), { url, directory });
    const fresh = await browser.newContext({ serviceWorkers: 'block' });
    const freshPage = await fresh.newPage();
    await freshPage.goto(url);
    await freshPage.getByRole('status').getByText('Ready', { exact: true }).waitFor();
    assert.equal(await freshPage.getByRole('textbox', { name: 'English text' }).inputValue(), 'This is a apple.');
    result.fresh_browser_does_not_inherit_text = true;
    result.browser_version = browser.version();
    await writeFile(`${directory}/browser-evidence.json`, JSON.stringify(result, null, 2) + '\n');
    console.log(JSON.stringify(result));
  } finally { await browser.close(); }
}
