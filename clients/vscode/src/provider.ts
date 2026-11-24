import * as v from "vscode"
import type { Model } from "./catalog"
import { generate, history, type Message, type Content, type Part } from "./responses"

function content(p: unknown): Content {
  if (p instanceof v.LanguageModelTextPart) return { type: "input_text", text: p.value }
  if (p instanceof v.LanguageModelDataPart) {
    if (/^image\/(png|jpeg|webp|gif)$/.test(p.mimeType))
      return {
        type: "input_image",
        image_url: `data:${p.mimeType};base64,${Buffer.from(p.data).toString("base64")}`,
      }
    if (p.mimeType.startsWith("text/") || p.mimeType === "application/json")
      return { type: "input_text", text: new TextDecoder().decode(p.data) }
  }
  throw new Error("The conversation contains an unsupported content type.")
}
export function message(m: v.LanguageModelChatRequestMessage, convert = content): Message {
  // 1.147's host dispatch can deliver System (3), absent from the public enum.
  const role =
    Number(m.role) === 3
      ? "developer"
      : m.role === v.LanguageModelChatMessageRole.Assistant
        ? "assistant"
        : m.role === v.LanguageModelChatMessageRole.User
          ? "user"
          : undefined
  if (!role) throw new Error("Unsupported conversation role.")
  return {
    role,
    parts: m.content.map((p): Part => {
      if (p instanceof v.LanguageModelToolCallPart)
        return { type: "function_call", call_id: p.callId, name: p.name, arguments: JSON.stringify(p.input) }
      if (p instanceof v.LanguageModelToolResultPart) {
        const parts = p.content.map(convert)
        return {
          type: "function_call_output",
          call_id: p.callId,
          output: parts.every((c) => c.type === "input_text") ? parts.map((c) => c.text).join("\n") : parts,
        }
      }
      return convert(p)
    }),
  }
}
export async function cancellable<T>(
  token: v.CancellationToken,
  work: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const c = new AbortController(),
    d = token.onCancellationRequested(() => c.abort())
  try {
    if (token.isCancellationRequested) throw new v.CancellationError()
    return await work(c.signal)
  } catch (e) {
    if (c.signal.aborted) throw new v.CancellationError()
    throw e
  } finally {
    d.dispose()
  }
}
interface Info extends v.LanguageModelChatInformation {
  model: Model
  base: string
  epoch: number
}
export interface Access {
  readonly epoch: number
  catalog(signal: AbortSignal): Promise<{ base: string; models: Model[] }>
  token(base: string): Thenable<string | undefined>
  readonly effort: string
}
export function provider(access: Access, changed: v.Event<void>): v.LanguageModelChatProvider<Info> {
  return {
    onDidChangeLanguageModelChatInformation: changed,
    provideLanguageModelChatInformation(options, token) {
      return cancellable(token, async (signal) => {
        const epoch = access.epoch,
          { base, models } = await access.catalog(signal)
        if (epoch !== access.epoch) return []
        return models
          .filter((m) => m.chat)
          .map((model) => ({
            id: model.id,
            family: model.id,
            version: model.id,
            name: model.name,
            detail: model.route,
            tooltip: `${model.route} · Output limit: ${model.output?.toLocaleString() ?? "Unknown"} · Reasoning: ${model.efforts.join(", ") || "Not reported"}`,
            maxInputTokens: model.input!,
            maxOutputTokens: model.output ?? 0,
            capabilities: { toolCalling: model.tools, imageInput: model.vision },
            model,
            base,
            epoch,
          }))
      }).catch((e) => {
        if (options.silent && !(e instanceof v.CancellationError)) return []
        throw e
      })
    },
    provideLanguageModelChatResponse(info, messages, options, progress, token) {
      return cancellable(token, async (signal) => {
        const bearer = await access.token(info.base)
        if (!bearer || info.epoch !== access.epoch)
          throw new Error("The connection changed. Select an ExetRouter model again.")
        const effort = options.modelOptions?.reasoningEffort ?? access.effort
        const tools = options.tools?.map((t) => ({
          type: "function",
          name: t.name,
          description: t.description,
          parameters: t.inputSchema ?? { type: "object", properties: {} },
          strict: false,
        }))
        if (options.toolMode === v.LanguageModelChatToolMode.Required && !tools?.length)
          throw new Error("Tool selection is required but no tools were supplied.")
        await generate(
          info.base,
          bearer,
          {
            model: info.id,
            input: history(messages.map((m) => message(m))),
            ...(tools?.length
              ? {
                  tools,
                  tool_choice:
                    options.toolMode === v.LanguageModelChatToolMode.Required ? "required" : "auto",
                  ...(info.model.parallelTools ? { parallel_tool_calls: true } : {}),
                }
              : {}),
            ...(typeof effort === "string" && info.model.efforts.includes(effort)
              ? { reasoning: { effort } }
              : {}),
          },
          (part) =>
            progress.report(
              "text" in part
                ? new v.LanguageModelTextPart(part.text)
                : new v.LanguageModelToolCallPart(part.id, part.name, part.input),
            ),
          signal,
        )
      })
    },
    async provideTokenCount(_, value, token) {
      if (token.isCancellationRequested) throw new v.CancellationError()
      const { countTokens, setMergeCacheSize } = await import("gpt-tokenizer/esm/encoding/o200k_base")
      setMergeCacheSize(0)
      // Vision uses image patches, not text tokens for the base64 transport. There is no
      // per-model patch budget in the catalog; reserve an estimate without encoding the image.
      let imageBudget = 0
      const forCount = (p: unknown): Content => {
        if (p instanceof v.LanguageModelDataPart && p.mimeType.startsWith("image/")) {
          imageBudget += 1536
          return { type: "input_image", image_url: "" }
        }
        return content(p)
      }
      const str = typeof value === "string" ? value : JSON.stringify(history([message(value, forCount)]))
      return countTokens(str, { disallowedSpecial: new Set() }) + imageBudget
    },
  }
}
