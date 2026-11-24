import { baseUrl, record } from "./catalog"

export type Content = { type: "input_text"; text: string } | { type: "input_image"; image_url: string }
export type Part =
  | Content
  | { type: "function_call"; call_id: string; name: string; arguments: string }
  | { type: "function_call_output"; call_id: string; output: string | Content[] }
export interface Message {
  role: "developer" | "assistant" | "user"
  parts: Part[]
}
export function history(messages: readonly Message[]) {
  const input: unknown[] = []
  for (const { role, parts } of messages) {
    let content: unknown[] = []
    const flush = () => {
      if (content.length)
        input.push({
          type: "message",
          role,
          content,
          ...(role === "assistant" ? { status: "completed" } : {}),
        })
      content = []
    }
    for (const part of parts) {
      if (part.type === "function_call" || part.type === "function_call_output") {
        flush()
        input.push(part)
      } else if (role === "assistant" && part.type === "input_text")
        content.push({ type: "output_text", text: part.text, annotations: [] })
      else content.push(part)
    }
    flush()
  }
  return input
}
export type Emission = { text: string } | { id: string; name: string; input: object }
const invalid = () => new Error("Invalid ExetRouter response stream.")
function failure(status: number, code?: unknown) {
  if (status === 429 || code === "usage_limit_reached" || code === "rate_limit_exceeded")
    return new Error("ExetRouter quota or concurrency limit reached.")
  if (status === 401 || status === 403)
    return new Error(`ExetRouter token rejected (HTTP ${status}). Check the token saved for this endpoint.`)
  if (code === "upstream_authentication_error")
    return new Error("The upstream account could not authenticate to the provider.")
  if (code === "upstream_interrupted")
    return new Error("The upstream response was interrupted. ExetRouter did not retry the request.")
  if (status >= 500) return new Error(`ExetRouter or its upstream is unavailable (HTTP ${status}).`)
  if (status >= 400) return new Error(`ExetRouter rejected the request (HTTP ${status}).`)
  return new Error("ExetRouter did not complete this response.")
}
function parse(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    throw invalid()
  }
}

/** Consume one Responses stream. Never retry a submitted POST or share state between calls. */
export async function generate(
  base: string,
  token: string,
  payload: object,
  emit: (part: Emission) => void,
  signal: AbortSignal,
) {
  const r = await fetch(`${baseUrl(base)}/responses`, {
    method: "POST",
    redirect: "error",
    signal,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      Accept: "text/event-stream",
    },
    body: JSON.stringify({ ...payload, store: false, stream: true }),
  })
  if (!r.ok || !r.body || !r.headers.get("content-type")?.includes("text/event-stream")) {
    await r.body?.cancel().catch(() => {})
    throw r.ok ? invalid() : failure(r.status)
  }
  const reader = r.body.getReader(),
    decoder = new TextDecoder()
  const calls = new Set<string>(),
    openCalls = new Map<number, { type: "function_call"; call_id: string; name: string }>(),
    lengths = new Map<string, number>()
  let pending = "",
    data: string[] = [],
    frameSize = 0,
    completed = false
  const finishItem = (value: unknown, index: number) => {
    const item = record(value)
    if (item.type === "function_call") {
      if (
        typeof item.call_id !== "string" ||
        !item.call_id ||
        typeof item.name !== "string" ||
        typeof item.arguments !== "string"
      )
        throw invalid()
      const open = openCalls.get(index)
      if (open && (open.call_id !== item.call_id || open.name !== item.name)) throw invalid()
      openCalls.delete(index)
      if (calls.has(item.call_id)) return
      const args = parse(item.arguments)
      if (!args || typeof args !== "object" || Array.isArray(args)) throw invalid()
      calls.add(item.call_id)
      emit({ id: item.call_id, name: item.name, input: args })
    } else if (item.type === "message" && Array.isArray(item.content)) {
      for (const [i, raw] of item.content.entries()) {
        const p = record(raw),
          text = p.type === "output_text" ? p.text : p.type === "refusal" ? p.refusal : undefined
        if (typeof text !== "string") throw invalid()
        const key = `${index}:${i}`,
          sent = lengths.get(key) ?? 0
        if (sent > text.length) throw invalid()
        if (sent < text.length) emit({ text: text.slice(sent) })
        lengths.set(key, text.length)
      }
    } else if (item.type !== "reasoning")
      throw new Error("This response contains an unsupported output item.")
  }
  const event = () => {
    const raw = data.join("\n")
    data = []
    frameSize = 0
    if (!raw || raw === "[DONE]") return
    const e = record(parse(raw))
    if (e.type === "response.completed") {
      const resp = record(e.response)
      if (resp.status != null && resp.status !== "completed") throw invalid()
      if (Array.isArray(resp.output)) resp.output.forEach(finishItem)
      if (openCalls.size) throw invalid()
      completed = true
    } else if (
      ["response.failed", "response.incomplete", "error", "response.cancelled"].includes(String(e.type))
    ) {
      const code = record(e.error ?? record(e.response).error).code
      throw failure(typeof e.status === "number" ? e.status : 0, code)
    } else if (e.type === "response.output_text.delta" || e.type === "response.refusal.delta") {
      if (typeof e.delta !== "string") throw invalid()
      const key = `${e.output_index}:${e.content_index}`
      lengths.set(key, (lengths.get(key) ?? 0) + e.delta.length)
      emit({ text: e.delta })
    } else if (e.type === "response.output_item.added") {
      const i = record(e.item)
      if (i.type === "function_call") {
        if (
          typeof i.call_id !== "string" ||
          typeof i.name !== "string" ||
          typeof e.output_index !== "number" ||
          openCalls.has(e.output_index)
        )
          throw invalid()
        openCalls.set(e.output_index, { type: "function_call", call_id: i.call_id, name: i.name })
      }
    } else if (e.type === "response.function_call_arguments.done") {
      const index = Number(e.output_index),
        open = openCalls.get(index)
      if (open) finishItem({ ...open, arguments: e.arguments }, index)
    } else if (e.type === "response.output_item.done") finishItem(e.item, Number(e.output_index))
  }
  try {
    while (!completed) {
      const chunk = await reader.read()
      if (chunk.done) throw new Error("ExetRouter stream ended before completion.")
      pending += decoder.decode(chunk.value, { stream: true })
      let split: number
      while ((split = pending.indexOf("\n")) >= 0 && !completed) {
        const line = pending.slice(0, split).replace(/\r$/, "")
        pending = pending.slice(split + 1)
        if (!line) event()
        else if (line.startsWith("data:")) {
          frameSize += line.length
          if (frameSize > 16 * 1024 * 1024) throw invalid()
          data.push(line.slice(5).replace(/^ /, ""))
        }
      }
      if (pending.length > 16 * 1024 * 1024 || calls.size + lengths.size + openCalls.size > 16384)
        throw invalid()
    }
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}
