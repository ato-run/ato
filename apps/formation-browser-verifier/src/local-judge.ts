import { z } from "zod";

import { JUDGE_CRITERIA, JUDGE_INSTRUCTIONS, JudgeUnavailable, type Judge, type JudgeState } from "./judge.ts";
import type { JudgeDecision } from "./protocol.ts";

const MAX_BYTES = 48 * 1024;
const probability = z.number().min(0).max(1);
const DecisionSchema = z.strictObject({
  choice: z.enum(["complete", "incomplete", "verify_more"]),
  confidence: probability,
  probabilities: z.strictObject({
    complete: probability,
    incomplete: probability,
    verify_more: probability,
  }),
});
const ChatResponseSchema = z.object({
  model: z.string(),
  done: z.literal(true),
  message: z.object({ role: z.literal("assistant"), content: z.string() }),
});
const TagsResponseSchema = z.object({
  models: z.array(z.object({ name: z.string(), digest: z.string() })),
});

export class OllamaJudge implements Judge {
  readonly model: string;
  private readonly origin: string;

  constructor(endpoint: string, private readonly tag: string, private readonly digest: string) {
    const url = new URL(endpoint);
    if (url.protocol !== "http:" || !["127.0.0.1", "[::1]"].includes(url.hostname)
      || !url.port || url.origin !== endpoint || !/^[a-f0-9]{64}$/.test(digest)
      || !/^[a-z0-9_./-]+:[a-z0-9_.-]+$/.test(tag)
      || tag.endsWith(":cloud") || tag.endsWith("-cloud")) {
      throw new JudgeUnavailable("judge_not_configured");
    }
    this.origin = url.origin;
    this.model = `ollama/sha256-${digest}`;
  }

  async decide(state: JudgeState, signal: AbortSignal): Promise<JudgeDecision> {
    const body = JSON.stringify({
      model: this.tag,
      stream: false,
      think: false,
      options: { temperature: 0, num_predict: 256 },
      format: z.toJSONSchema(DecisionSchema),
      messages: [
        {
          role: "system",
          content: `${JUDGE_INSTRUCTIONS}\nChoice definitions: ${JSON.stringify(JUDGE_CRITERIA)}\n`
            + "Return one choice with your confidence and probabilities for each option.",
        },
        { role: "user", content: JSON.stringify(state) },
      ],
    });
    if (Buffer.byteLength(body) > MAX_BYTES) throw new JudgeUnavailable("judge_invalid_response");
    const deadline = AbortSignal.any([signal, AbortSignal.timeout(40_000)]);
    await this.checkPin(deadline);
    const raw = await this.request("/api/chat", deadline, body);
    try {
      const response = ChatResponseSchema.parse(raw);
      if (response.model !== this.tag) throw new JudgeUnavailable("judge_invalid_response");
      const decision = DecisionSchema.parse(JSON.parse(response.message.content));
      await this.checkPin(deadline);
      return { ...decision, model: this.model };
    } catch (error) {
      if (error instanceof JudgeUnavailable) throw error;
      throw new JudgeUnavailable("judge_invalid_response");
    }
  }

  private async checkPin(signal: AbortSignal): Promise<void> {
    const parsed = TagsResponseSchema.safeParse(await this.request("/api/tags", signal));
    if (!parsed.success || !parsed.data.models.some((model) =>
      model.name === this.tag && model.digest === this.digest)) {
      throw new JudgeUnavailable("judge_invalid_response");
    }
  }

  private async request(path: string, signal: AbortSignal, body?: string): Promise<unknown> {
    try {
      const response = await fetch(this.origin + path, {
        signal,
        redirect: "error",
        ...(body === undefined ? {} : {
          method: "POST", body, headers: { "content-type": "application/json" },
        }),
      });
      if (!response.ok || !response.body) throw new JudgeUnavailable("judge_unavailable");
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let size = 0;
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MAX_BYTES) {
          await reader.cancel();
          throw new JudgeUnavailable("judge_invalid_response");
        }
        chunks.push(value);
      }
      try {
        return JSON.parse(Buffer.concat(chunks).toString("utf8"));
      } catch {
        throw new JudgeUnavailable("judge_invalid_response");
      }
    } catch (error) {
      if (error instanceof JudgeUnavailable) throw error;
      throw new JudgeUnavailable("judge_unavailable");
    }
  }
}
