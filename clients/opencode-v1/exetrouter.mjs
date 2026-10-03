// OpenCode V1 1.18.34: provider-scoped transport via its public config hook.
// V1 retries API/status/stream errors independently of SDK maxRetries.
// End failed operations with a fixed Error, without retaining their payloads.
const MESSAGE = "ExetRouter operation ended without completion. Automatic retries are disabled; inspect the connection before submitting a new request.";
const MAX_EVENT = 1024 * 1024;
const wrapped = Symbol("exetrouter-v1-fetch");
const failure = () => new Error(MESSAGE);
const deadline = (value) => typeof value === "number" && Number.isFinite(value) && value > 0 ? value : 300_000;

function transport(fetchFn, options) {
  const headerMs = deadline(options.headerTimeout);
  const idleMs = deadline(options.chunkTimeout);
  const totalMs = typeof options.timeout === "number" && Number.isFinite(options.timeout) && options.timeout > 0 ? options.timeout : undefined;
  const result = async (input, init = {}) => {
    const abort = new AbortController();
    const caller = init.signal ?? (input instanceof Request ? input.signal : undefined);
    const signal = caller ? AbortSignal.any([caller, abort.signal]) : abort.signal;
    const error = () => caller?.aborted ? new DOMException("Operation cancelled", "AbortError") : failure();
    const header = setTimeout(() => abort.abort(), headerMs);
    const total = totalMs ? setTimeout(() => abort.abort(), totalMs) : undefined;
    const cleanup = () => { clearTimeout(header); clearTimeout(total); };
    let response;
    try {
      response = await fetchFn(input, {...init, signal, redirect: "error"});
      clearTimeout(header);
      if (!response.ok || !response.body || !response.headers.get("content-type")?.includes("text/event-stream")) {
        await response.body?.cancel().catch(() => {});
        throw failure();
      }
    } catch {
      cleanup();
      abort.abort();
      throw error();
    }
    const reader = response.body.getReader();
    const decoder = new TextDecoder("utf-8", {fatal: true});
    const encoder = new TextEncoder();
    let pending = "";
    let complete = false;
    let ended = false;
    const stop = async () => {
      ended = true;
      cleanup();
      abort.abort();
      await reader.cancel().catch(() => {});
    };
    const body = new ReadableStream({
      async pull(controller) {
        let timer;
        try {
          // Emit one event per pull: no unbounded downstream queue.
          while (!ended) {
            const boundary = /\r?\n\r?\n/.exec(pending);
            if (boundary) {
              const frame = pending.slice(0, boundary.index + boundary[0].length);
              pending = pending.slice(frame.length);
              const data = frame.split(/\r?\n/).filter((line) => line.startsWith("data:"))
                .map((line) => line.slice(5).trimStart()).join("\n");
              if (data) {
                if (data === "[DONE]") throw failure();
                const event = JSON.parse(data);
                if (["error", "response.failed", "response.incomplete"].includes(event.type)) throw failure();
                if (event.type === "response.completed") {
                  if (event.response?.status !== "completed") throw failure();
                  complete = true;
                }
              }
              controller.enqueue(encoder.encode(frame));
              if (complete) { await stop(); controller.close(); }
              return;
            }
            const part = await Promise.race([
              reader.read(),
              new Promise((_, reject) => { timer = setTimeout(() => { abort.abort(); reject(failure()); }, idleMs); }),
            ]);
            clearTimeout(timer);
            if (part.done) throw failure();
            // Bound undecoded data as well as the pending event.
            if (part.value.byteLength > MAX_EVENT || encoder.encode(pending).byteLength + part.value.byteLength > MAX_EVENT) throw failure();
            pending += decoder.decode(part.value, {stream: true});
          }
        } catch {
          clearTimeout(timer);
          await stop();
          controller.error(error());
        }
      },
      async cancel() { await stop(); },
    });
    return new Response(body, {status: response.status, headers: response.headers});
  };
  result[wrapped] = true;
  return result;
}

export default async function ExetRouterV1() {
  return {
    async config(config) {
      const provider = config.provider?.exetrouter;
      if (!provider) return;
      if (provider.npm !== "@ai-sdk/openai") throw failure();
      const options = provider.options ??= {};
      if (!options.fetch?.[wrapped]) options.fetch = transport(options.fetch ?? fetch, options);
      // The bridge owns deadlines so V1 cannot wrap them in retryable errors.
      options.headerTimeout = false;
      options.chunkTimeout = 0;
      options.timeout = false;
    },
    "chat.params": async (input, output) => {
      if (input.model.providerID !== "exetrouter") return;
      if (input.model.api?.npm !== "@ai-sdk/openai") throw failure();
      output.maxOutputTokens = undefined;
      output.temperature = undefined;
      output.topP = undefined;
      output.topK = undefined;
      output.options.store = false;
    },
  };
}
