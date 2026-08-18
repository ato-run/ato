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
const delayedAckBridgeSource = bridgeSource.replace(
  'send({ type: "ack", request_id: value.request_id });',
  'setTimeout(() => send({ type: "ack", request_id: value.request_id }), 75);',
);
if (delayedAckBridgeSource === bridgeSource) throw new Error("Browser Bridge ACK hook was not found");
const disconnectingBridgeSource = bridgeSource.replace(
  "dispatch(value.event);",
  'socket.close(4000, "injected replay disconnect"); return;',
);
if (disconnectingBridgeSource === bridgeSource) throw new Error("Browser Bridge dispatch hook was not found");
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
      await expect(page.locator("body")).toHaveAttribute("data-trusted-clicks", "0");
    },
    async duringReplay(page) {
      await page.locator("#increment").click();
    },
    async continue(page) {
      await page.locator("#increment").click();
      await expect(page.locator("#count")).toHaveText("3");
      await expect(page.locator("body")).toHaveAttribute("data-trusted-clicks", "1");
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

test("Browser plus HTTP Evolution fails closed before double apply", async ({ browser }) => {
  await runScenario(browser, "http-stateful-counter", {
    httpAdapter: true,
    replayRejected: true,
    async record(page) {
      await page.locator("#increment").click();
      await expect(page.locator("#count")).toHaveText("1");
      await expect(page.locator("body")).toHaveAttribute("data-requests", "1");
    },
  });
});

test("DOM event profile preserves focus and safe KeyboardEvent.key", async ({ browser }) => {
  await runScenario(browser, "focus-keyboard", {
    async record(page) {
      await page.locator("#action").click();
      await page.keyboard.press("Enter");
      await expect(page.locator("#status")).toHaveText("1");
    },
    async assertReplayed(page) {
      await expect(page.locator("#status")).toHaveText("1");
      await expect(page.locator("#action")).toBeFocused();
    },
    async continue(page) {
      await page.keyboard.press("Enter");
      await expect(page.locator("#status")).toHaveText("2");
    },
  });
});

test("Bridge disconnect during Replay fails the Run and cleans control state", async ({ browser }) => {
  await runScenario(browser, "click-counter", {
    replayFailure: "disconnect",
    async record(page) {
      await page.locator("#increment").click();
      await expect(page.locator("#count")).toHaveText("1");
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
  const authorRuntime = join(root, "author-private-runtime");
  const recipientRuntime = join(root, "recipient-private-runtime");
  await Promise.all([
    mkdir(project),
    mkdir(authorHome),
    mkdir(recipientHome),
    mkdir(authorRuntime),
    mkdir(recipientRuntime),
  ]);
  const upstreamPort = await unusedPort();
  const publicPort = scenario.httpAdapter ? await unusedPort() : upstreamPort;
  const origin = `http://127.0.0.1:${publicPort}`;
  await copyFile(join(fixtures, "server.mjs"), join(project, "server.mjs"));
  await copyFile(join(fixtures, fixtureName, "index.html"), join(project, "index.html"));
  await writeFile(
    join(project, "capsule.toml"),
    capsuleToml({ upstreamPort, publicPort, origin, httpAdapter: scenario.httpAdapter }),
  );
  let portableRun;
  let authorContext;
  let recipientContext;
  try {
    await ato(["init", project], authorHome, authorRuntime);
    const initialHead = (await readFile(join(project, ".capsule/refs/heads/main"), "utf8")).trim();
    const authorBrowser = await openRecordedBrowser(browser, project, origin, undefined, authorRuntime);
    authorContext = authorBrowser.context;
    const authorPage = authorContext.pages()[0];
    await scenario.record(authorPage);

    // The final physical interaction has completed in the page, but may still
    // be in the Bridge/socket/coalescing frontier when stop begins.
    await ato(["stop", project], authorHome, authorRuntime);
    expect(
      (await readdir(authorRuntime)).some((name) => name.startsWith("browser-")),
    ).toBe(false);
    expect(
      (await readdir(join(project, ".capsule/runs"))).some((name) => name.startsWith("browser-")),
    ).toBe(false);
    const finalHead = (await readFile(join(project, ".capsule/refs/heads/main"), "utf8")).trim();
    expect(finalHead).not.toBe(initialHead);
    const records = await browserRecords(project);
    const allRecords = await recordedEvents(project);
    expect(records.length).toBeGreaterThan(0);
    for (const record of records) {
      expect(record.adapter_id).toBe("ato.browser@1");
      expect(record.protocol_id).toBe("ato.browser@1");
      expect(record.head_after).not.toBe(record.head_before);
    }
    expect(allRecords.at(-1).head_after).toBe(finalHead);

    const bundle = join(root, `${fixtureName}.capsule`);
    if (scenario.replayRejected) {
      await expect(
        ato(
          ["encap", `${project}@main`, "--materialize", "ato.replay@1", "-o", bundle],
          authorHome,
          authorRuntime,
        ),
      ).rejects.toMatchObject({
        stderr: expect.stringContaining(
          "Browser-driven network effects cannot currently be replayed through both Browser and HTTP adapters",
        ),
      });
      return;
    }
    await ato(
      ["encap", `${project}@main`, "--materialize", "ato.replay@1", "-o", bundle],
      authorHome,
      authorRuntime,
    );
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
    expect(replay.records.length).toBe(allRecords.length);
    if (scenario.httpAdapter) {
      expect(replay.required_adapters).toContain("ato.http@1");
      expect(allRecords.some((record) => record.adapter_id === "ato.http@1")).toBe(true);
    }
    const portablePayloadText = portable.payloads
      .map((payload) => Buffer.from(payload.bytes, "base64").toString("utf8"))
      .join("\n");
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.channel_credential);
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.browser_session);
    expect(portablePayloadText).not.toContain(authorBrowser.bootstrap.control_url);

    portableRun = spawn(atoBin, ["run", bundle], {
      env: {
        ...process.env,
        ATO_HOME: recipientHome,
        ATO_BROWSER_RUNTIME_DIR: recipientRuntime,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const portableOutput = captureOutput(portableRun);
    const recipientProject = await waitForPortableProject(recipientHome, recipientRuntime);
    if (scenario.replayFailure === "disconnect") {
      await expect(
        openRecordedBrowser(
          browser,
          recipientProject,
          origin,
          undefined,
          recipientRuntime,
          scenario.replayFailure,
        ),
      ).rejects.toThrow();
      const exit = await waitForExit(portableRun, portableOutput);
      expect(exit.code).not.toBe(0);
      expect(exit.stderr).toContain("Browser Bridge disconnected");
      await waitFor(async () => {
        const names = await readdir(recipientRuntime).catch(() => []);
        return !names.some((name) => name.startsWith("browser-"));
      });
      return;
    }
    const recipientBrowser = await openRecordedBrowser(
      browser,
      recipientProject,
      origin,
      scenario.duringReplay,
      recipientRuntime,
    ).catch((error) => {
      throw new Error(`recipient Browser failed: ${portableOutput.stderr}`, { cause: error });
    });
    recipientContext = recipientBrowser.context;
    const recipientPage = recipientContext.pages()[0];
    expect(await recipientPage.evaluate(() => Object.keys(globalThis).filter((key) => key.startsWith("__ATO_BROWSER_")))).toEqual([]);
    expect(await recipientPage.evaluate(() => localStorage.length)).toBe(0);
    expect(await recipientPage.evaluate(() => sessionStorage.length)).toBe(0);
    await scenario.assertReplayed(recipientPage);
    await scenario.continue(recipientPage);
    await recipientPage.evaluate(() => fetch("/__shutdown"));
    const exit = await waitForExit(portableRun, portableOutput);
    expect(exit.code, exit.stderr).toBe(0);
  } finally {
    await authorContext?.close().catch(() => {});
    await recipientContext?.close().catch(() => {});
    if (portableRun && portableRun.exitCode === null) portableRun.kill("SIGTERM");
    await rm(root, { recursive: true, force: true });
  }
}

async function openRecordedBrowser(browser, project, origin, duringReplay, runtimeDir, replayFailure) {
  const bootstrapPath = await waitFor(async () => {
    const runs = runtimeDir ?? join(project, ".capsule/runs");
    const names = await readdir(runs).catch(() => []);
    const name = names.find((candidate) => candidate.startsWith("browser-") && candidate.endsWith(".json"));
    return name ? join(runs, name) : null;
  });
  const bootstrap = JSON.parse(await readFile(bootstrapPath, "utf8"));
  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  const injectedBridge = replayFailure === "disconnect"
    ? disconnectingBridgeSource
    : duringReplay
      ? delayedAckBridgeSource
      : bridgeSource;
  const page = await context.newPage();
  const driver = await installIsolatedBridge(context, page, bootstrap, injectedBridge);
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
  await driver.bindToNavigation();
  if (duringReplay) {
    await waitFor(async () => (await driver.value("globalThis.__ATO_BROWSER_LIFECYCLE__")) === "restoring");
    await duringReplay(page);
  }
  try {
    await waitFor(async () => (await driver.value("globalThis.__ATO_BROWSER_READY__")) === true);
  } catch (error) {
    const diagnostic = await driver.value(`({
      ready: globalThis.__ATO_BROWSER_READY__,
      lifecycle: globalThis.__ATO_BROWSER_LIFECYCLE__,
      bridgeError: globalThis.__ATO_BROWSER_ERROR__,
      bootstrapVisible: "__ATO_BROWSER_BOOTSTRAP__" in globalThis,
      origin: globalThis.location.origin,
    })`);
    const workerLog = await readFile(join(project, ".capsule/runs/output.log"), "utf8").catch(() => "<unavailable>");
    await context.close().catch(() => {});
    throw new Error(
      `Browser Bridge did not become active: ${JSON.stringify(diagnostic)}; worker=${workerLog}`,
      { cause: error },
    );
  }
  return { context, bootstrap };
}

async function installIsolatedBridge(context, page, bootstrap, source) {
  const cdp = await context.newCDPSession(page);
  const worldName = `ato.browser.bridge.${bootstrap.browser_session}`;
  let executionContextId;
  cdp.on("Runtime.executionContextCreated", ({ context: executionContext }) => {
    if (executionContext.name === worldName) executionContextId = executionContext.id;
  });
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `globalThis.__ATO_BROWSER_BOOTSTRAP__ = ${JSON.stringify(bootstrap)};\n${source}`,
    worldName,
  });
  return {
    async bindToNavigation() {
      await waitFor(() => executionContextId);
    },
    async value(expression) {
      if (!executionContextId) return undefined;
      const result = await cdp.send("Runtime.evaluate", {
        expression,
        contextId: executionContextId,
        returnByValue: true,
      });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
      return result.result.value;
    },
  };
}

async function browserRecords(project) {
  return (await recordedEvents(project)).filter((record) => record.adapter_id === "ato.browser@1");
}

async function recordedEvents(project) {
  const directory = join(project, ".capsule/records/main");
  const names = (await readdir(directory)).sort();
  const records = await Promise.all(names.map(async (name) => JSON.parse(await readFile(join(directory, name), "utf8"))));
  return records.sort((left, right) => left.id.seq - right.id.seq);
}

async function waitForPortableProject(home, runtimeDir) {
  return waitFor(async () => {
    const runtimeFiles = await readdir(runtimeDir).catch(() => []);
    if (!runtimeFiles.some((value) => value.startsWith("browser-") && value.endsWith(".json"))) {
      return null;
    }
    const cache = join(home, "cache");
    const names = await readdir(cache).catch(() => []);
    for (const name of names) {
      if (!name.startsWith("portable-run-")) continue;
      const project = join(cache, name, "workspace");
      const stat = await readdir(project).catch(() => null);
      if (stat) return project;
    }
    return null;
  });
}

async function ato(args, home, runtimeDir) {
  return execFileAsync(atoBin, args, {
    env: {
      ...process.env,
      ATO_HOME: home,
      ...(runtimeDir ? { ATO_BROWSER_RUNTIME_DIR: runtimeDir } : {}),
    },
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

function captureOutput(child) {
  const output = { stderr: "" };
  child.stderr.on("data", (chunk) => { output.stderr += chunk; });
  return output;
}

async function waitForExit(child, output = captureOutput(child)) {
  const code = child.exitCode ?? await new Promise((resolvePromise, reject) => {
    child.once("error", reject);
    child.once("exit", resolvePromise);
  });
  return { code, stderr: output.stderr };
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

function capsuleToml({ upstreamPort, publicPort, origin, httpAdapter }) {
  const http = httpAdapter ? `
[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:${publicPort}"
upstream = "127.0.0.1:${upstreamPort}"
ready_path = "/"
` : "";
  return `schema = 1

[[process]]
id = "app"
command = ["node", "server.mjs", "${upstreamPort}"]
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
${http}

[encap]
materializers = ["ato.replay@1"]
`;
}
