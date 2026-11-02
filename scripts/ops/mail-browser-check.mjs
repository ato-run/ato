#!/usr/bin/env node
// Browser check for the mail client Surface.
//
// Opens a real headless Chrome through the DevTools protocol, loads the mail
// client URL a person would use, optionally signs in with the staging test
// account, and records what the browser actually rendered. It is deliberately
// dependency-free (Node 22 built-ins only) so it can run on the Runner host,
// a CI box or an operator laptop.
//
// Required environment:
//   MAIL_CLIENT_URL    the URL a browser opens (hosted Run URL, staging ingress,
//                      or a local Docker pair)
//   OUT_DIR            directory for login.png, inbox.png, result.json
// Optional:
//   MAIL_USER / MAIL_PASSWORD   when set, perform a real sign-in
//   MAIL_EXPECT_TEXT            substring that must appear in the mailbox page
//   MAIL_LOGIN_ONLY=1           capture the login page only
//   CHROME_BIN                  Chrome/Chromium executable
//
// Exit codes: 0 pass, 2 expectation failure, 1 infrastructure failure.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const url = process.env.MAIL_CLIENT_URL;
const outDir = process.env.OUT_DIR;
if (!url) fail('MAIL_CLIENT_URL is required');
if (!outDir) fail('OUT_DIR is required');
fs.mkdirSync(outDir, { recursive: true });
const chromeBin = process.env.CHROME_BIN || defaultChrome();
if (!chromeBin) fail('CHROME_BIN is not set and no default Chrome was found');
const loginOnly = process.env.MAIL_LOGIN_ONLY === '1';
const user = process.env.MAIL_USER;
const password = process.env.MAIL_PASSWORD;
if (!loginOnly && (!user || !password)) fail('MAIL_USER and MAIL_PASSWORD are required unless MAIL_LOGIN_ONLY=1');
const expectText = process.env.MAIL_EXPECT_TEXT || '';

const profile = path.join(outDir, '.chrome-profile');
fs.rmSync(profile, { recursive: true, force: true });
fs.mkdirSync(profile, { recursive: true });
const chrome = spawn(chromeBin, [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--disable-breakpad',
  '--disable-crash-reporter', `--crash-dumps-dir=${path.join(profile, 'crashes')}`,
  '--no-first-run', '--no-default-browser-check', '--remote-debugging-port=0',
  `--user-data-dir=${profile}`, 'about:blank',
], { stdio: ['ignore', 'ignore', 'pipe'] });
let chromeStderr = '';
chrome.stderr.on('data', (chunk) => { chromeStderr += chunk.toString().slice(0, 4000); });

try {
  const port = await waitForDevtoolsPort(profile);
  const target = await newTarget(port);
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let nextId = 0;
  const pending = new Map();
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); }
  };
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, (message) => message.error ? reject(new Error(JSON.stringify(message.error))) : resolve(message.result));
    ws.send(JSON.stringify({ id, method, params }));
  });

  await call('Page.enable');
  await call('Network.enable');
  await call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await call('Page.navigate', { url });
  await waitForTitle(call, (title) => title.length > 0);
  const loginTitle = await evaluate(call, 'document.title');
  const loginUrl = await evaluate(call, 'location.href');
  await screenshot(call, path.join(outDir, 'login.png'));

  const result = { url, loginUrl, loginTitle, signedIn: false, inboxUrl: null, inboxTitle: null, expectedText: expectText || null, expectedTextFound: null, screenshots: ['login.png'] };
  if (loginOnly) {
    finish(ws, 0, result);
  }

  // Fill the server-rendered login form and submit it like a person would.
  const filled = await evaluate(call, `(() => {
    const username = document.querySelector('input[name=username], input[type=email], input[name=email]');
    const secret = document.querySelector('input[name=password], input[type=password]');
    if (!username || !secret) return false;
    username.value = ${JSON.stringify(user)};
    secret.value = ${JSON.stringify(password)};
    username.dispatchEvent(new Event('input', { bubbles: true }));
    secret.dispatchEvent(new Event('input', { bubbles: true }));
    const form = username.closest('form');
    if (!form) return false;
    form.requestSubmit ? form.requestSubmit() : form.submit();
    return true;
  })()`);
  if (!filled) failBrowser('the login form has no username/password inputs');
  await waitForTitle(call, (title) => /inbox|mail/i.test(title), 15000);
  const inboxTitle = await evaluate(call, 'document.title');
  const inboxUrl = await evaluate(call, 'location.href');
  const bodyText = await evaluate(call, 'document.body.innerText.slice(0, 4000)');
  await screenshot(call, path.join(outDir, 'inbox.png'));
  const signedIn = /inbox|mail/i.test(inboxTitle) && /mailbox|inbox|deleted|sent/i.test(bodyText);
  result.signedIn = signedIn;
  result.inboxUrl = inboxUrl;
  result.inboxTitle = inboxTitle;
  result.screenshots.push('inbox.png');
  if (expectText) result.expectedTextFound = bodyText.includes(expectText);
  if (!signedIn) failBrowser('sign-in did not render a mailbox page');
  if (expectText && !result.expectedTextFound) {
    writeResult(result);
    finish(ws, 2, result);
  }
  writeResult(result);
  finish(ws, 0, result);
} catch (error) {
  failBrowser(error.message + (chromeStderr ? `\nchrome: ${chromeStderr.slice(-800)}` : ''));
} finally {
  chrome.kill('SIGKILL');
}

function defaultChrome() {
  const candidates = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ];
  return candidates.find((candidate) => fs.existsSync(candidate)) || null;
}

async function waitForDevtoolsPort(profileDir) {
  const portFile = path.join(profileDir, 'DevToolsActivePort');
  for (let attempt = 0; attempt < 100; attempt++) {
    if (fs.existsSync(portFile)) {
      const [port] = fs.readFileSync(portFile, 'utf8').split('\n');
      if (/^\d+$/.test(port)) return Number(port);
    }
    await sleep(200);
  }
  throw new Error('Chrome did not expose a DevTools port');
}

async function newTarget(port) {
  const response = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' });
  if (!response.ok) throw new Error(`could not create a DevTools target: ${response.status}`);
  return response.json();
}

async function evaluate(call, expression) {
  const result = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error('page evaluation failed: ' + JSON.stringify(result.exceptionDetails));
  return result.result.value;
}

async function waitForTitle(call, predicate, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const title = await evaluate(call, 'document.title');
    if (predicate(title || '')) return title;
    await sleep(250);
  }
  throw new Error('timed out waiting for the page title');
}

async function screenshot(call, file) {
  const result = await call('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(file, Buffer.from(result.data, 'base64'));
}

function writeResult(result) {
  fs.writeFileSync(path.join(outDir, 'result.json'), JSON.stringify(result, null, 2) + '\n');
}

function finish(ws, code, result) {
  try { ws.close(); } catch {}
  console.log(JSON.stringify(result));
  process.exit(code);
}

function failBrowser(message) {
  const result = { ok: false, error: message };
  try { writeResult(result); } catch {}
  console.error('browser check failed: ' + message);
  process.exit(1);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sleep(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }
