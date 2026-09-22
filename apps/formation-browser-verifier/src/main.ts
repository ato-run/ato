// Entry point: one request on stdin, one result on stdout.
//
// stdout carries the result and nothing else. Diagnostics go to stderr,
// briefly; the caller keeps only a short tail. Credentials come from the
// environment the caller forwards (JEV_API_KEY, ANTHROPIC_API_KEY) and are
// never written anywhere.

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

async function main(): Promise<number> {
  let request;
  try {
    request = RequestSchema.parse(JSON.parse(await readStdin()));
  } catch {
    process.stderr.write("formation-browser-verifier: the request is not a valid verification request\n");
    return 2;
  }
  const agentModel = process.env.ATO_BROWSER_AGENT_MODEL || DEFAULT_AGENT_MODEL;
  const keyVariable = agentKeyVariable(agentModel);
  const agentKey = keyVariable ? (process.env[keyVariable] ?? "") : "";
  const judgeKey = process.env.JEV_API_KEY ?? "";

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
