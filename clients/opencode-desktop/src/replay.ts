import { createHash } from "node:crypto"
import { Model, Plugin } from "@opencode/plugin"
import { providerID, type Auth } from "./catalog"

const aliasPrefix = "exetrouter-replay:"
const ticketHeader = "x-exetrouter-replay-ticket"
const chatHeader = "x-exetrouter-replay-chat"
const proof = /^exrctx1\.[A-Za-z0-9_-]{86}\.[!-~]+$/
const digest = (value: string) => createHash("sha256").update(value).digest("hex")
type Context = Parameters<Parameters<typeof Plugin.define>[0]["setup"]>[0]

export function publicModelID(id: string) {
  return id.startsWith(aliasPrefix) ? id.slice(aliasPrefix.length) : id
}

export async function registerReplay(ctx: Context, baseURL: string, auth: () => Promise<Auth | undefined>) {
  const version = /^v?(\d+)\.(\d+)\.(\d+)(?:[-+]|$)/.exec(ctx.app?.version ?? "")
  if (!version || Number(version[1]) !== 2 || Number(version[2]) < 4 ||
      (Number(version[2]) === 4 && Number(version[3]) < 2)) return

  await ctx.model.transform((editor) => {
    for (const model of editor.list(providerID)) {
      if (!model.enabled || model.id !== model.modelID || model.id.startsWith(aliasPrefix)) continue
      const id = Model.ID.make(aliasPrefix + model.id)
      if (editor.get(providerID, id)) continue
      // A distinct catalog identity lets OpenCode rebuild the saved messages.
      // The actual provider model and selected variant stay the same.
      editor.update(providerID, id, (alias) => Object.assign(alias, model, {
        id, enabled: false, settings: { ...model.settings, compaction: { type: "summary" } },
      }))
    }
  })

  const keyFor = (sessionID: string, model: string) => `replay/${digest(JSON.stringify([baseURL, sessionID, model]))}`
  const checked = new Map<string, number>()
  const ticketFor = async (key: string) => {
    const value = await ctx.storage.get(key)
    return validTicket(value) ? value : undefined
  }

  await ctx.session.hook("experimental.model.resolve", async (event) => {
    if ((event.kind !== "primary" && event.kind !== "compaction") || event.model.id.startsWith(aliasPrefix)) return
    const key = keyFor(event.sessionID, event.model.id)
    try {
      if (Date.now() < (checked.get(key) ?? 0)) return
      const history = await ctx.session.context({ sessionID: event.sessionID })
      const saved = history.find((message) => message.type === "compaction" && message.status === "completed")
      const checkpoint = saved?.type === "compaction" && saved.status === "completed" &&
        saved.model?.providerID === providerID && saved.model.id === event.model.id
        ? encryptedCheckpoint(saved.native?.messages) : undefined
      if (!checkpoint) {
        checked.delete(key)
        checked.set(key, Date.now() + 30_000)
        if (checked.size > 256) checked.delete(checked.keys().next().value!)
        return
      }
      if (!await ticketFor(key)) {
        const { data: provider } = await ctx.provider.get({ providerID })
        if (provider.settings?.baseURL !== baseURL) return
        const { data: available } = await ctx.model.list()
        if (!available.some((model) => model.providerID === providerID && model.id === event.model.id && model.modelID === model.id)) return
        const credential = await auth()
        if (!credential) return
        const response = await fetch(`${baseURL}/responses/replay`, {
          method: "POST", redirect: "error", signal: AbortSignal.timeout(10_000),
          headers: { authorization: `Bearer ${credential.token}`, "content-type": "application/json" },
          body: JSON.stringify({ chat: digest(event.sessionID), model: event.model.id, checkpoint }),
        })
        await response.body?.cancel()
        const ticket = response.headers.get(ticketHeader)
        if (response.status !== 204 || !validTicket(ticket)) return
        // The ticket holds routing metadata only, never the checkpoint.
        await ctx.storage.set(key, ticket)
      }
      event.model = { ...event.model, id: Model.ID.make(aliasPrefix + event.model.id) }
    } catch { /* Keep native routing if the ownership exchange cannot be completed. */ }
  }, { providerID })

  return async (event: { sessionID: string, kind: string, model: { providerID: string, id: string }, baseURL?: string, headers: Record<string, string> }) => {
    if (event.model.providerID !== providerID || (event.kind !== "primary" && event.kind !== "compaction")) return
    if (event.baseURL !== baseURL) {
      if (event.model.id.startsWith(aliasPrefix)) throw new Error("ExetRouter replay routing is unavailable.")
      return
    }
    const ticket = await ticketFor(keyFor(event.sessionID, publicModelID(event.model.id))).catch(() => undefined)
    if (!ticket) {
      if (event.model.id.startsWith(aliasPrefix)) throw new Error("ExetRouter replay routing is unavailable.")
      return
    }
    event.headers[ticketHeader] = ticket
    event.headers[chatHeader] = digest(event.sessionID)
  }
}

function validTicket(value: unknown): value is string {
  return typeof value === "string" && value.length <= 512 && proof.test(value)
}

function encryptedCheckpoint(messages: unknown): string | undefined {
  if (!Array.isArray(messages)) return
  for (const message of messages) {
    if (message?.role !== "assistant" || !Array.isArray(message.content)) continue
    for (const part of message.content) {
      if (part?.type === "compaction" && typeof part.encrypted === "string" &&
          part.encrypted.length < 2_000_000 && proof.test(part.encrypted)) return part.encrypted
    }
  }
}
