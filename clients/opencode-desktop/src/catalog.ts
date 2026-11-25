import { createHash } from "node:crypto"
import { isDeepStrictEqual } from "node:util"
import { Model, Provider } from "@opencode/plugin"
import type { ConnectionInfo } from "@opencode/client"
import type { CatalogStatus } from "./schemas"
import type { Options } from "./options"

export const providerID = Provider.ID.make("exetrouter")
export const providerPackage = "@opencode/ai/providers/openai/responses"
const MAX_CATALOG_BYTES = 2 * 1024 * 1024

export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid ExetRouter metadata.")
  return value as Record<string, unknown>
}

function settings(value: unknown): Model.Settings {
  const source = object(value)
  const result: Record<string, string> = {}
  for (const key of ["reasoningEffort", "reasoningSummary", "textVerbosity"]) {
    if (source[key] !== undefined) {
      if (typeof source[key] !== "string" || source[key].length > 128) throw new Error("Invalid ExetRouter metadata.")
      result[key] = source[key]
    }
  }
  return result
}

export function parseModels(value: unknown): Model.Info[] {
  const providers = object(object(value).providers)
  const source = object(object(providers.exetrouter).models)
  if (Object.keys(source).length > 4096) throw new Error("Invalid ExetRouter metadata.")
  return Object.entries(source).map(([id, entry]) => {
    if (!id || id.length > 256 || /[\s\u0000-\u001f\u007f]/.test(id)) throw new Error("Invalid ExetRouter metadata.")
    const model = object(entry)
    const limit = object(model.limit)
    const capabilities = object(model.capabilities)
    const count = (key: string) => {
      const n = limit[key]
      if (typeof n !== "number" || !Number.isSafeInteger(n) || n < 0) throw new Error("Invalid ExetRouter metadata.")
      return n
    }
    const media = (key: string) => {
      const list = capabilities[key]
      if (!Array.isArray(list) || list.length > 16 || list.some((item) => typeof item !== "string" || item.length > 64)) {
        throw new Error("Invalid ExetRouter metadata.")
      }
      return list as string[]
    }
    if (typeof model.name !== "string" || model.name.length > 256 || typeof capabilities.tools !== "boolean" ||
      !Array.isArray(model.variants) || model.variants.length > 64) throw new Error("Invalid ExetRouter metadata.")
    const variants = model.variants.map((value) => {
      const variant = object(value)
      if (typeof variant.id !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(variant.id)) throw new Error("Invalid ExetRouter metadata.")
      return { id: Model.VariantID.make(variant.id), settings: settings(variant.settings ?? {}) }
    })
    if (new Set(variants.map((v) => v.id)).size !== variants.length) throw new Error("Invalid ExetRouter metadata.")
    // Never accept a package, endpoint, credential, headers or body from the remote catalog.
    return {
      ...Model.Info.default(providerID, Model.ID.make(id)),
      name: model.name,
      limit: { context: count("context"), input: count("input"), output: count("output") },
      capabilities: { tools: capabilities.tools, input: media("input"), output: media("output") },
      settings: settings(model.settings ?? {}),
      variants,
    }
  })
}

export async function loadModels(baseURL: string, token: string, signal: AbortSignal, request = fetch): Promise<Model.Info[]> {
  const response = await request(`${baseURL}/models/opencode-v2`, {
    headers: { authorization: `Bearer ${token}` },
    redirect: "error",
    signal,
  })
  if (!response.ok || !response.body) {
    await response.body?.cancel()
    throw new Error("ExetRouter catalog is unavailable.")
  }
  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_CATALOG_BYTES) throw new Error("ExetRouter catalog is too large.")
      chunks.push(value)
    }
    const bytes = new Uint8Array(size)
    let offset = 0
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
    return parseModels(JSON.parse(new TextDecoder().decode(bytes)))
  } catch {
    throw new Error("ExetRouter catalog could not be read.")
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

export interface Auth {
  connection: ConnectionInfo
  token: string
}
export interface Inventory {
  models: Model.Info[]
  connection: ConnectionInfo
}
function connectionIdentity({ connection }: Auth): string {
  return connection.type === "env" ? `env:${connection.name}` : `credential:${connection.id}`
}
function sameAuth(a: Auth | undefined, b: Auth | undefined): boolean {
  return a && b ? a.token === b.token && connectionIdentity(a) === connectionIdentity(b) : a === b
}

type Snapshot = { models: Model.Info[]; fetchedAt: number }
const cacheKey = Symbol.for("exetrouter.opencode.catalog.v1")
const host = globalThis as typeof globalThis & { [cacheKey]?: Map<string, Snapshot> }
// OpenCode can reload the plugin module as well as its location. Retain only
// bounded, validated metadata in the server process, never the bearer or a disk cache.
const retained = host[cacheKey] ??= new Map()
const maxRetained = 8
const maxAge = 2 * 60 * 60 * 1000

/** One bounded refresh, coalesced callers, no previous account's inventory after a credential change. */
export class Catalog {
  inventory?: Inventory
  status: CatalogStatus
  private owner?: Auth
  private pending?: Promise<CatalogStatus>
  private rerun = false
  private stop = new AbortController()

  constructor(
    private config: Options,
    private auth: () => Promise<Auth | undefined>,
    private publish: () => Promise<void>,
    private load = loadModels,
  ) {
    this.status = { state: "needs_token", baseURL: config.baseURL, models: 0, fetchedAt: null }
  }

  private key(auth: Auth): string {
    return createHash("sha256").update(JSON.stringify([this.config.baseURL, connectionIdentity(auth), auth.token])).digest("hex")
  }

  private install(auth: Auth, snapshot: Snapshot, state: "ready" | "stale") {
    this.inventory = { models: snapshot.models, connection: auth.connection }
    this.status = { ...this.status, state, models: snapshot.models.length, fetchedAt: snapshot.fetchedAt }
  }

  /** Seed a new location before its background discovery, for the exact active connection only. */
  async restore(): Promise<void> {
    const auth = await this.auth().catch(() => undefined)
    if (!auth || this.stop.signal.aborted) return
    const key = this.key(auth)
    const snapshot = retained.get(key)
    if (!snapshot) return
    retained.delete(key)
    if (Date.now() - snapshot.fetchedAt > maxAge) return
    retained.set(key, snapshot)
    this.owner = auth
    this.install(auth, snapshot, "stale")
    await this.publish()
  }

  refresh(): Promise<CatalogStatus> {
    if (this.stop.signal.aborted) return Promise.resolve(this.status)
    if (this.pending) { this.rerun = true; return this.pending }
    this.pending = this.run().finally(() => { this.pending = undefined })
    return this.pending
  }

  private async run(): Promise<CatalogStatus> {
    do {
      this.rerun = false
      let published = this.inventory
      try {
        const auth = await this.auth()
        if (!auth) {
          this.owner = undefined
          this.inventory = undefined
          this.status = { ...this.status, models: 0, fetchedAt: null, state: "needs_token" }
          if (published) await this.publish()
          continue
        }
        if (!sameAuth(auth, this.owner)) {
          this.owner = auth
          this.inventory = undefined
          this.status = { ...this.status, models: 0, fetchedAt: null, state: "needs_token" }
          if (published) {
            await this.publish()
            published = undefined
          }
        }
        const signal = AbortSignal.any([this.stop.signal, AbortSignal.timeout(15_000)])
        const models = await this.load(this.config.baseURL, auth.token, signal)
        if (this.stop.signal.aborted) return this.status
        if (!sameAuth(auth, await this.auth())) {
          this.inventory = undefined
          this.owner = undefined
          this.status = { ...this.status, models: 0, fetchedAt: null, state: "needs_token" }
          this.rerun = true
        } else {
          const snapshot = { models, fetchedAt: Date.now() }
          const key = this.key(auth)
          retained.delete(key)
          retained.set(key, snapshot)
          if (retained.size > maxRetained) retained.delete(retained.keys().next().value!)
          this.install(auth, snapshot, "ready")
        }
      } catch {
        // A failed request must not retain another connection's cached inventory.
        let current: Auth | undefined
        try { current = await this.auth() } catch { /* Treat unresolved identity as unknown. */ }
        if (!sameAuth(current, this.owner)) {
          this.inventory = undefined
          this.owner = undefined
        }
        this.status = this.inventory
          ? { ...this.status, state: "stale" }
          : { ...this.status, state: "error", models: 0, fetchedAt: null }
      }
      if (!this.stop.signal.aborted && !isDeepStrictEqual(published, this.inventory)) await this.publish()
    } while (this.rerun && !this.stop.signal.aborted)
    return this.status
  }

  async close(): Promise<void> {
    this.stop.abort()
    await this.pending
    this.inventory = undefined
    this.owner = undefined
  }
}
