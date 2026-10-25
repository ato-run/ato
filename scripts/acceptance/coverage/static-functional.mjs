#!/usr/bin/env node
// Consume fresh Formation bundles unchanged; separate UI judgement from K.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { createRequire } from 'node:module';

const [root, playwrightModule, chromeExecutable, output] = process.argv.slice(2);
assert(root && playwrightModule && chromeExecutable && output,
  'usage: static-functional.mjs FRESH_ROOT PLAYWRIGHT_MODULE CHROME_EXECUTABLE OUTPUT_DIR');
const { chromium } = createRequire(import.meta.url)(playwrightModule);
const digest = bytes => 'sha256:' + createHash('sha256').update(bytes).digest('hex');
assert(!fs.existsSync(output), 'output directory must be new; preserve prior evidence');
fs.mkdirSync(output, { recursive: true });
const browser = await chromium.launch({ executablePath: chromeExecutable, headless: true,
  args: ['--disable-background-networking', '--disable-component-update'] });
const results = [];

try {
  for (const [index, name] of [['21', '2048'], ['22', 'reveal.js']]) {
    const observation = JSON.parse(fs.readFileSync(path.join(root, index + '.json')));
    assert.equal(observation.model_calls, 0);
    const formed = observation.result.result;
    const attempt = formed.attempts.find(item => item.receipt?.fully_satisfied);
    assert(attempt, 'fresh Rust receipt required');
    const route = formed.verified_routes.find(item => item.attempt_id === attempt.attempt_id);
    assert(route);
    const bundle = path.join(root, index, 'out/bundles', route.materialization_ref.slice(7));
    const manifestBytes = fs.readFileSync(path.join(bundle, 'manifest.json'));
    const manifest = JSON.parse(manifestBytes);
    assert.equal(digest(manifestBytes), route.materialization_ref);
    const files = new Map();
    for (const [name, item] of Object.entries(manifest.files)) {
      const bytes = fs.readFileSync(path.join(bundle, 'blobs/sha256', item.blob.slice(7)));
      assert.equal(digest(bytes), item.blob);
      assert.equal(bytes.length, item.size);
      files.set('/' + name, { bytes, mediaType: item.media_type });
    }
    assert.equal(digest(files.get('/' + manifest.entry_path).bytes),
      attempt.receipt.observations.find(item => item.id === 'root').evidence.body_sha256);
    const requests = [];
    const server = http.createServer((request, response) => {
      const pathname = new URL(request.url, 'http://127.0.0.1').pathname;
      const item = files.get(pathname === '/' ? '/' + manifest.entry_path : pathname);
      const status = request.method === 'GET' && item ? 200 : 404;
      requests.push({ method: request.method, path: pathname, status });
      response.writeHead(status, { 'Content-Type': item?.mediaType ?? 'text/plain',
        'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
      response.end(status === 200 ? item.bytes : 'not found');
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const origin = 'http://127.0.0.1:' + server.address().port;
    const context = await browser.newContext({ viewport: { width: 1100, height: 850 },
      serviceWorkers: 'block' });
    const blocked = [], errors = [];
    await context.route('**/*', async route => {
      const url = new URL(route.request().url());
      if (url.origin === origin) await route.continue();
      else { blocked.push(route.request().url()); await route.abort('blockedbyclient'); }
    });
    await context.routeWebSocket('**/*', socket => {
      blocked.push(socket.url()); socket.close();
    });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    const steps = [];
    let passed = false, failure = null;
    try {
      assert.equal((await page.goto(origin, { waitUntil: 'networkidle' })).status(), 200);
      if (name === '2048') {
        await page.locator('.tile-container .tile').first().waitFor();
        const readBoard = () => page.evaluate(() => ({
          tiles: [...document.querySelectorAll('.tile-container > .tile')].map(tile => ({
            position: [...tile.classList].find(value => value.startsWith('tile-position-')),
            value: Number(tile.querySelector('.tile-inner').textContent),
          })).sort((a, b) => a.position.localeCompare(b.position) || a.value - b.value),
          score: Number(document.querySelector('.score-container').firstChild.textContent),
        }));
        const initial = await readBoard();
        assert.equal(initial.tiles.length, 2);
        steps.push({ action: 'initial', ...initial });
        await page.screenshot({ path: path.join(output, '2048-before.png') });
        for (const key of ['ArrowLeft', 'ArrowUp', 'ArrowRight', 'ArrowDown',
          'ArrowLeft', 'ArrowUp', 'ArrowRight', 'ArrowDown']) {
          await page.keyboard.press(key);
          await page.waitForTimeout(220); // Allow source CSS/RAF animations to settle.
          await page.evaluate(() => new Promise(resolve =>
            requestAnimationFrame(() => requestAnimationFrame(resolve))));
          steps.push({ action: key, ...await readBoard() });
        }
        const final = steps.at(-1);
        assert(steps.slice(1).some((step, i) =>
          JSON.stringify(step.tiles) !== JSON.stringify(steps[i].tiles)));
        assert(final.score >= initial.score);
        assert(final.tiles.length > 0 && final.tiles.every(tile =>
          /^tile-position-[1-4]-[1-4]$/.test(tile.position) &&
          tile.value >= 2 && Number.isInteger(Math.log2(tile.value))));
        await page.screenshot({ path: path.join(output, '2048-after.png') });
      } else {
        const present = page.locator('.slides > section.present');
        await present.waitFor();
        const readSlide = () => page.evaluate(() => ({
          present: document.querySelector('.slides > section.present')?.textContent.trim(),
          classes: [...document.querySelectorAll('.slides > section')].map(section => section.className),
        }));
        assert.equal(await present.innerText(), 'Slide 1');
        steps.push({ action: 'initial', ...await readSlide() });
        await page.screenshot({ path: path.join(output, 'reveal-before.png') });
        await page.keyboard.press('ArrowRight');
        await page.waitForFunction(() => document.querySelector('.slides > section.present')?.textContent.trim() === 'Slide 2');
        await page.waitForTimeout(650); // The present class changes before the visual transition ends.
        steps.push({ action: 'ArrowRight', ...await readSlide() });
        await page.screenshot({ path: path.join(output, 'reveal-next.png') });
        await page.keyboard.press('ArrowLeft');
        await page.waitForFunction(() => document.querySelector('.slides > section.present')?.textContent.trim() === 'Slide 1');
        await page.waitForTimeout(650);
        steps.push({ action: 'ArrowLeft', ...await readSlide() });
        await page.screenshot({ path: path.join(output, 'reveal-return.png') });
      }
      passed = true;
    } catch (error) {
      failure = String(error);
      await page.screenshot({ path: path.join(output, index + '-failure.png') });
    } finally {
      await context.close();
      await new Promise(resolve => server.close(resolve));
    }
    results.push({ name, index: Number(index), browser_version: browser.version(),
      origin, materialization_ref: route.materialization_ref,
      verified_blob_count: files.size, fresh_receipt: attempt.receipt,
      functional_pass: passed, failure, steps, requests, blocked_external_requests: blocked,
      page_errors: errors, persistence: 'not measured; browser localStorage is not Ato state' });
    console.log(JSON.stringify({ name, functional_pass: passed, failure,
      blocked_external_requests: blocked.length, page_errors: errors }));
  }
} finally {
  await browser.close();
}
fs.writeFileSync(path.join(output, 'browser-results.json'), JSON.stringify({
  schema: 'ato.formation-static-browser-observation/1',
  source_modified_for_test: false, browser_judgement_in_typed_K: false,
  external_network: 'requests denied before send; service workers and WebSockets blocked',
  applications: results,
}, null, 2) + '\n');
if (results.some(item => !item.functional_pass)) process.exitCode = 1;
