import type { SessionAccount } from "./schemas"

export const observationHeader = "x-exetrouter-account"
export const observeHeader = "x-exetrouter-observe-account"

type Request = { sessionID: string; kind: string; model: { providerID: string; id: string } }

/** Bounded, transient metadata only. No frames, turn state, credentials or content retained. */
export class SessionAccounts {
  private readonly rows = new Map<string, SessionAccount>()
  constructor(private readonly changed: (sessionID: string, account: SessionAccount | null) => void, private readonly capacity = 256) {}
  get(sessionID: string) { return this.rows.get(sessionID) ?? null }

  request(event: Request) {
    if (event.kind === "primary" && event.model.providerID !== "exetrouter") this.set(event.sessionID, null)
  }

  http(event: Request & { response: Response }) {
    if (!event.response.ok) return
    this.observe(event, event.response.headers.get(observationHeader))
  }

  websocket(event: Request & { frame: string }) {
    if (event.kind !== "primary" || event.model.providerID !== "exetrouter" || event.frame.length > 65536) return
    // Parsing is transient and only a bounded metadata frame is projected. Never
    // consume/clone an HTTP body or modify either transport's response.
    try {
      const value = JSON.parse(event.frame)
      if (value?.type === "response.metadata") this.observe(event, value.headers?.[observationHeader])
    } catch { /* Invalid/non-metadata input is left entirely to the provider driver. */ }
  }

  private observe(event: Request, key: unknown) {
    if (event.kind !== "primary" || event.model.providerID !== "exetrouter") return
    // Other metadata frames can carry only turn state or timing. Their missing
    // alias says nothing about account selection: keep the last confirmation
    // through completion/idle until another valid alias or provider switch.
    if (typeof key !== "string" || !/^[a-f0-9]{64}$/.test(key)) return
    this.set(event.sessionID, { key, model: event.model.id, observedAt: Math.floor(Date.now() / 1000) })
  }

  private set(sessionID: string, account: SessionAccount | null) {
    const old = this.get(sessionID)
    if (!account && !old) return
    this.rows.delete(sessionID)
    if (account) {
      this.rows.set(sessionID, account)
      if (this.rows.size > this.capacity) {
        const evicted = this.rows.keys().next().value!
        this.rows.delete(evicted); this.changed(evicted, null)
      }
    }
    this.changed(sessionID, account)
  }
}
