// Entry point: one request on stdin, one result on stdout.
//
// stdout carries the result and nothing else. Diagnostics go to stderr,
// briefly; the caller keeps only a short tail. Model keys (JEV_API_KEY,
// DEEPSEEK_API_KEY) arrive on the file descriptor named by
// ATO_VERIFIER_SECRETS_FD — never in the environment — and are never written
// anywhere. Run by hand, the helper falls back to the environment and then
// removes the keys from it, so the browser it starts never inherits them.

import { closeSync, readFileSync } from "node:fs";

import { agentKeyVariable, StagehandDriver, DEFAULT_AGENT_MODEL, type BrowserDriver } from "./browser.ts";
import { DEFAULT_JEV_MODEL, JevJudge, type Judge } from "./judge.ts";
import { RequestSchema } from "./protocol.ts";
import { sequencer, verify } from "./verify.ts";

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks).toString("utf8");
}

class UnconfiguredDriver implements BrowserDriver {
  constructor(private readonly agentModel: string) {}
  async open(): Promise<never> {
    throw new Error("agent_not_configured: no model key for the browser agent");
  }
  async runTask(): Promise<never> {
    throw new Error("agent_not_configured");
  }
  async observe(): Promise<never> {
    throw new Error("agent_not_configured");
  }
  events() {
    return [];
  }
  identity() {
    return { stagehand_version: null, browser_version: null, agent_model: this.agentModel };
  }
  async close() {}
}

const SECRET_NAMES = ["JEV_API_KEY", "DEEPSEEK_API_KEY"] as const;

/// The model keys, read once. Afterwards none is left in `process.env`,
/// which is what a child process (the browser) would inherit.
function takeSecrets(): Record<string, string> {
  const secrets: Record<string, string> = {};
  const fd = process.env.ATO_VERIFIER_SECRETS_FD;
  delete process.env.ATO_VERIFIER_SECRETS_FD;
  if (fd !== undefined) {
    const descriptor = Number(fd);
    try {
      const parsed = JSON.parse(readFileSync(descriptor, "utf8") || "{}") as Record<string, unknown>;
      for (const name of SECRET_NAMES) {
        if (typeof parsed[name] === "string") secrets[name] = parsed[name] as string;
      }
    } finally {
      try {
        closeSync(descriptor);
      } catch {
        // already closed
      }
    }
  } else {
    for (const name of SECRET_NAMES) {
      const value = process.env[name];
      if (value) secrets[name] = value;
    }
  }
  for (const name of SECRET_NAMES) delete process.env[name];
  return secrets;
}

async function main(): Promise<number> {
  const secrets = takeSecrets();
  let request;
  try {
    request = RequestSchema.parse(JSON.parse(await readStdin()));
  } catch {
    process.stderr.write("formation-browser-verifier: the request is not a valid verification request\n");
    return 2;
  }
  const agentModel = process.env.ATO_BROWSER_AGENT_MODEL || DEFAULT_AGENT_MODEL;
  const keyVariable = agentKeyVariable(agentModel);
  const agentKey = keyVariable ? (secrets[keyVariable] ?? "") : "";
  const judgeKey = secrets.JEV_API_KEY ?? "";

  const sequence = sequencer();
  const browser: BrowserDriver = agentKey
    ? new StagehandDriver({
        agentModel,
        agentApiKey: agentKey,
        scratchDir: request.scratch_dir,
        chromePath: process.env.ATO_BROWSER_CHROME_PATH || undefined,
        sequence,
      })
    : new UnconfiguredDriver(agentModel);
  let judge: Judge | null = null;
  try {
    judge = judgeKey ? new JevJudge(judgeKey, process.env.ATO_JEV_MODEL || DEFAULT_JEV_MODEL) : null;
  } catch {
    judge = null;
  }

  const result = await verify(request, { browser, judge, sequence });
  await writeAll(JSON.stringify(result) + "\n");
  return 0;
}

/// Write to stdout and wait until it is flushed. `process.exit` right after a
/// write to a pipe can cut the result off mid-document.
function writeAll(text: string): Promise<void> {
  return new Promise((resolve, reject) =>
    process.stdout.write(text, (error) => (error ? reject(error) : resolve())),
  );
}

main().then(
  (code) => process.exit(code),
  () => {
    process.stderr.write("formation-browser-verifier: internal error\n");
    process.exit(1);
  },
);
