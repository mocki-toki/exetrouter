export function baseUrl(input: string): string {
  let u: URL
  try {
    u = new URL(input)
  } catch {
    throw new Error("Configure a valid ExetRouter API base URL.")
  }
  if (
    (u.protocol !== "https:" &&
      !(u.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(u.hostname))) ||
    u.username ||
    u.password ||
    /[?#]/.test(u.href) ||
    !u.pathname.replace(/\/+$/, "").endsWith("/v1")
  )
    throw new Error("Use an HTTPS or loopback API base URL ending in /v1, without credentials, query or fragment.")
  return u.href.replace(/\/+$/, "")
}
export const secretKey = (base: string) => `api-token:${baseUrl(base)}`
export function record(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {}
}
export const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : []
const positive = (n: unknown) => (typeof n === "number" && Number.isSafeInteger(n) && n > 0 ? n : undefined)
export function catalog(body: unknown) {
  const rows = record(body).data
  if (!Array.isArray(rows)) throw new Error("Invalid model catalog.")
  return rows.map((raw) => {
    const m = record(raw),
      meta = record(m.exetrouter),
      caps = strings(m.capabilities)
    if (typeof m.id !== "string" || !m.id) throw new Error("Invalid model ID.")
    const context = positive(meta.context_window) ?? positive(meta.max_context_window)
    const input =
      context &&
      Math.min(
        Math.floor((context * Math.min(positive(meta.effective_context_window_percent) ?? 95, 100)) / 100),
        positive(meta.auto_compact_token_limit) ?? context,
      )
    const efforts = strings(
      Array.isArray(meta.supported_reasoning_levels)
        ? meta.supported_reasoning_levels.map((l) => record(l).effort)
        : [],
    )
    return {
      id: m.id,
      name: typeof m.display_name === "string" ? m.display_name : m.id,
      route:
        m.source === "platform"
          ? "Platform API"
          : m.platform_fallback === true
            ? "Codex · Platform fallback"
            : "Codex",
      context,
      input,
      output: positive(meta.max_output_tokens),
      efforts,
      tools: caps.includes("tools"),
      parallelTools: meta.supports_parallel_tool_calls === true,
      vision: caps.includes("image_input"),
      chat:
        !!input &&
        !caps.includes("embeddings") &&
        !caps.includes("image_generation") &&
        strings(meta.input_modalities).includes("text"),
    }
  })
}
export type Model = ReturnType<typeof catalog>[number]

export class CatalogError extends Error {
  constructor(message: string, readonly status?: number) { super(message) }
}

export async function discover(base: string, token: string, signal: AbortSignal) {
  let r: Response
  try {
    r = await fetch(`${baseUrl(base)}/models`, {
      headers: { Authorization: `Bearer ${token}` },
      redirect: "error",
      signal,
    })
  } catch (error) {
    if (signal.aborted) throw error
    throw new CatalogError("Could not reach the ExetRouter API. Check the address, network and TLS.")
  }
  try {
    if (!r.ok || !r.body) throw new CatalogError(
      r.status === 401 || r.status === 403 ? `API token rejected (HTTP ${r.status}).`
        : r.status === 404 ? "The configured API address does not serve /v1/models."
        : `The API could not return its model catalog (HTTP ${r.status}).`, r.status)
    const chunks: Buffer[] = []
    let bytes = 0
    for await (const chunk of r.body as unknown as AsyncIterable<Uint8Array>) {
      bytes += chunk.length
      if (bytes > 8 * 1024 * 1024) throw new CatalogError("The model catalog exceeded the metadata limit.")
      chunks.push(Buffer.from(chunk))
    }
    let parsed: unknown
    try {
      parsed = JSON.parse(Buffer.concat(chunks).toString())
    } catch {
      throw new CatalogError("The configured endpoint did not return a valid model catalog.")
    }
    try { return catalog(parsed) }
    catch { throw new CatalogError("The configured endpoint did not return a valid model catalog.") }
  } finally {
    await r.body?.cancel().catch(() => {})
  }
}
