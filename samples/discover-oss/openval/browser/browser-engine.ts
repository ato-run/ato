type RequestMessage =
  | { kind: "cashflow"; payload: unknown; frequency: "annual" | "monthly" }
  | { kind: "parse"; bytes: ArrayBuffer }
  | { kind: "export"; payload: unknown };

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  cleanup: () => void;
};

let worker: Worker | undefined;
let nextId = 0;
const pending = new Map<number, Pending>();

function getWorker(): Worker {
  if (worker) return worker;
  const current = new Worker("/openval-worker.js");
  current.onmessage = ({ data }: MessageEvent<{ id: number; result?: unknown; error?: string }>) => {
    const request = pending.get(data.id);
    if (!request) return;
    pending.delete(data.id);
    request.cleanup();
    if (data.error) request.reject(new Error(data.error));
    else request.resolve(data.result);
  };
  current.onerror = () => {
    if (worker === current) worker = undefined;
    current.terminate();
    for (const request of pending.values()) {
      request.cleanup();
      request.reject(new Error("The local calculation engine stopped."));
    }
    pending.clear();
  };
  worker = current;
  return current;
}

export function runBrowserEngine(message: RequestMessage, signal?: AbortSignal): Promise<unknown> {
  if (signal?.aborted) return Promise.reject(new DOMException("Aborted", "AbortError"));
  const current = getWorker();
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    const abort = () => {
      pending.delete(id);
      current.postMessage({ id, kind: "cancel" });
      reject(new DOMException("Aborted", "AbortError"));
    };
    const cleanup = () => signal?.removeEventListener("abort", abort);
    pending.set(id, { resolve, reject, cleanup });
    signal?.addEventListener("abort", abort, { once: true });
    try {
      if (message.kind === "parse") current.postMessage({ ...message, id }, [message.bytes]);
      else current.postMessage({ ...message, id });
    } catch (error) {
      pending.delete(id);
      cleanup();
      reject(error instanceof Error ? error : new Error("Could not start the calculation."));
    }
  });
}
