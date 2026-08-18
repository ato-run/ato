import { expect, test } from "@playwright/test";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile, copyFile } from "node:fs/promises";
import { createServer } from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const testRoot = dirname(fileURLToPath(import.meta.url));
const repository = resolve(testRoot, "../..");
const fixtures = join(testRoot, "fixtures");
const bridgeSource = await readFile(
  join(repository, "extensions/adapters/browser/bridge/browser-bridge.js"),
  "utf8",
);
const atoBin = process.env.ATO_BIN ?? join(repository, "target/debug/ato");

test("click counter records, replays into a fresh browser, and continues", async ({ browser }) => {
  await runScenario(browser, "click-counter", {
    async record(page) {
      await page.locator("#increment").click();
      await page.locator("#increment").click();
      await expect(page.locator("#count")).toHaveText("2");
    },
    async assertReplayed(page) {
      await expect(page.locator("#count")).toHaveText("2");
    },
    async continue(page) {
      await page.locator("#increment").click();
      await expect(page.locator("#count")).toHaveText("3");
    },
  });
});

test("keyboard grid uses the same Adapter and Replay infrastructure", async ({ browser }) => {
  await runScenario(browser, "keyboard-grid", {
    async record(page) {
      await page.keyboard.press("ArrowRight");
      await page.keyboard.press("ArrowRight");
      await page.keyboard.press("ArrowDown");
      await expect(page.locator("body")).toHaveAttribute("data-x", "2");
      await expect(page.locator("body")).toHaveAttribute("data-y", "1");
    },
    async assertReplayed(page) {
      await expect(page.locator("body")).toHaveAttribute("data-x", "2");
      await expect(page.locator("body")).toHaveAttribute("data-y", "1");
    },
    async continue(page) {
      await page.keyboard.press("ArrowLeft");
      await expect(page.locator("body")).toHaveAttribute("data-x", "1");
    },
  });
});

test("pointer drag preserves the coalesced interaction frontier", async ({ browser }) => {
  await runScenario(browser, "pointer-drag", {
    async record(page) {
      await page.mouse.move(100, 100);
      await page.mouse.down();
      await page.mouse.move(300, 240, { steps: 8 });
      await page.mouse.up();
      await expect(page.locator("body")).toHaveAttribute("data-x", "300");
      await expect(page.locator("body")).toHaveAttribute("data-y", "240");
    },
    async assertReplayed(page) {
      await expect(page.locator("body")).toHaveAttribute("data-x", "300");
      await expect(page.locator("body")).toHaveAttribute("data-y", "240");
    },
    async continue(page) {
      await page.mouse.move(310, 250);
      await page.mouse.down();
      await page.mouse.move(360, 280);
      await page.mouse.up();
      await expect(page.locator("body")).toHaveAttribute("data-x", "360");
      await expect(page.locator("body")).toHaveAttribute("data-y", "280");
    },
  });
});

async function runScenario(browser, fixtureName, scenario) {
  const scratchRoot = join(repository, ".tmp");
  await mkdir(scratchRoot, { recursive: true });
  const root = await mkdtemp(join(scratchRoot, `browser-${fixtureName}-`));
  const project = join(root, "project");
  const authorHome = join(root, "author-home");
  const recipientHome = join(root, "recipient-home");
  await Promise.all([mkdir(project), mkdir(authorHome), mkdir(recipientHome)]);
  const port = await unusedPort();
  const origin = `http://127.0.0.1:${port}`;
  await copyFile(join(fixtures, "server.mjs"), join(project, "server.mjs"));
  await copyFile(join(fixtures, fixtureName, "index.html"), join(project, "index.html"));
  await writeFile(join(project, "capsule.toml"), capsuleToml(port, origin));
  let portableRun;
  let authorContext;
  let recipientContext;
  try {
    await ato(["init", project], authorHome);
    const initialHead = (await readFile(join(project, ".capsule/refs/heads/main"), "utf8")).trim();
    const authorBrowser = await openRecordedBrowser(browser, project, origin);
    authorContext = authorBrowser.context;
    const authorPage = authorContext.pages()[0];
    await scenario.record(authorPage);

    // The final physical interaction has completed in the page, but may still
    // be in the Bridge/socket/coalescing frontier when stop begins.
    await ato(["stop", project], authorHome);
    expect(
      (await readdir(join(project, ".capsule/runs"))).some((name) => name.startsWith("browser-")),
    ).toBe(false);
    const finalHead = (await readFile(join(project, ".capsule/refs/heads/main"), "utf8")).trim();
    expect(finalHead).not.toBe(initialHead);
    const records = await browserRecords(project);
    expect(records.length).toBeGreaterThan(0);
    for (const record of records) {
      expect(record.adapter_id).toBe("ato.browser@1");
      expect(record.protocol_id).toBe("ato.browser@1");
      expect(record.head_after).not.toBe(record.head_before);
    }
    expect(records.at(-1).head_after).toBe(finalHead);

    const bundle = join(root, `${fixtureName}.capsule`);
    await ato(["encap", `${project}@main`, "--materialize", "ato.replay@1", "-o", bundle], authorHome);
    const portable = JSON.parse(await readFile(bundle, "utf8"));
    expect(portable.index.root).toBe(finalHead);
    expect(portable.index.materializations).toEqual(
      expect.arrayContaining([expect.objectContaining({ materializer_id: "ato.replay@1" })]),
    );
    const replayEntry = portable.index.materializations.find(
      (entry) => entry.materializer_id === "ato.replay@1",
    );
    const replayPayload = portable.payloads.find(
      (payload) => payload.reference === replayEntry.descriptor_ref,
    );
    const replay = JSON.parse(Buffer.from(replayPayload.bytes, "base64").toString("utf8"));
    expect(replay.target).toBe(finalHead);
    expect(replay.required_adapters).toContain("ato.browser@1");
    expect(replay.required_protocols).toContain("ato.browser@1");
    expect(replay.records.length).toBe(records.length);
    const portablePayloadText = portable.payloads
      .map((payload) => Buffer.from(payload.bytes, "base64").toString("utf8"))
      .join("\n");
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.channel_credential);
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.browser_session);
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.control_url);

    portableRun = spawn(atoBin, ["run", bundle], {
      env: { ...process.env, ATO_HOME: recipientHome },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const recipientProject = await waitForPortableProject(recipientHome);
    const recipientBrowser = await openRecordedBrowser(browser, recipientProject, origin);
    recipientContext = recipientBrowser.context;
    const recipientPage = recipientContext.pages()[0];
    await scenario.assertReplayed(recipientPage);
    await scenario.continue(recipientPage);
    await recipientPage.evaluate(() => fetch("/__shutdown"));
    const exit = await waitForExit(portableRun);
    expect(exit.code, exit.stderr).toBe(0);
  } finally {
    await authorContext?.close().catch(() => {});
    await recipientContext?.close().catch(() => {});
    if (portableRun && portableRun.exitCode === null) portableRun.kill("SIGTERM");
    await rm(root, { recursive: true, force: true });
  }
}

async function openRecordedBrowser(browser, project, origin) {
  const bootstrapPath = await waitFor(async () => {
    const runs = join(project, ".capsule/runs");
    const names = await readdir(runs).catch(() => []);
    const name = names.find((candidate) => candidate.startsWith("browser-") && candidate.endsWith(".json"));
    return name ? join(runs, name) : null;
  });
  const bootstrap = JSON.parse(await readFile(bootstrapPath, "utf8"));
  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  await context.addInitScript({
    content: `globalThis.__ATO_BROWSER_BOOTSTRAP__ = ${JSON.stringify(bootstrap)};\n${bridgeSource}`,
  });
  const page = await context.newPage();
  // Adapter discovery is published when its control listener is ready, which
  // can precede the application HTTP listener on a loaded CI runner. Polling
  // with repeated page.goto() calls leaves overlapping/cancelled navigations
  // and can prevent the init-script Bridge from completing its handshake.
  await waitFor(async () => {
    try {
      const response = await fetch(origin, { signal: AbortSignal.timeout(1_000) });
      return response.ok;
    } catch {
      return false;
    }
  });
  await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 15_000 });
  try {
    await page.waitForFunction(() => globalThis.__ATO_BROWSER_READY__ === true, null, { timeout: 10_000 });
  } catch (error) {
    const diagnostic = await page.evaluate(() => ({
      ready: globalThis.__ATO_BROWSER_READY__,
      lifecycle: globalThis.__ATO_BROWSER_LIFECYCLE__,
      bridgeError: globalThis.__ATO_BROWSER_ERROR__,
      bootstrapVisible: "__ATO_BROWSER_BOOTSTRAP__" in globalThis,
      origin: globalThis.location.origin,
    }));
    const workerLog = await readFile(join(project, ".capsule/runs/output.log"), "utf8").catch(() => "<unavailable>");
    throw new Error(
      `Browser Bridge did not become active: ${JSON.stringify(diagnostic)}; worker=${workerLog}`,
      { cause: error },
    );
  }
  return { context, bootstrap };
}

async function browserRecords(project) {
  const directory = join(project, ".capsule/records/main");
  const names = (await readdir(directory)).sort();
  const records = await Promise.all(names.map(async (name) => JSON.parse(await readFile(join(directory, name), "utf8"))));
  return records.filter((record) => record.adapter_id === "ato.browser@1");
}

async function waitForPortableProject(home) {
  return waitFor(async () => {
    const cache = join(home, "cache");
    const names = await readdir(cache).catch(() => []);
    for (const name of names) {
      if (!name.startsWith("portable-run-")) continue;
      const project = join(cache, name, "workspace");
      const runs = await readdir(join(project, ".capsule/runs")).catch(() => []);
      if (runs.some((value) => value.startsWith("browser-") && value.endsWith(".json"))) return project;
    }
    return null;
  });
}

async function ato(args, home) {
  return execFileAsync(atoBin, args, {
    env: { ...process.env, ATO_HOME: home },
    timeout: 30_000,
  });
}

async function waitFor(predicate, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await predicate();
    if (value) return value;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 25));
  }
  throw new Error("condition did not become true");
}

async function waitForExit(child) {
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const code = await new Promise((resolvePromise, reject) => {
    child.once("error", reject);
    child.once("exit", resolvePromise);
  });
  return { code, stderr };
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolvePromise);
  });
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  await new Promise((resolvePromise) => server.close(resolvePromise));
  return port;
}

function capsuleToml(port, origin) {
  return `schema = 1

[[process]]
id = "app"
command = ["node", "server.mjs", "${port}"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[port]]
id = "app.browser"
node = "app"
protocol = "ato.browser@1"
role = "server"

[[adapter]]
port = "app.browser"
use = "ato.browser@1"

[adapter.config]
expected_origin = "${origin}"

[encap]
materializers = ["ato.replay@1"]
`;
}
