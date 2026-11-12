import { Integration, Plugin, Provider } from "@opencode/plugin"
import { Catalog, providerID, providerPackage } from "./catalog"
import { options } from "./options"
import { Accounts, ExetRouter } from "./rpc"
import { SessionAccounts, observeHeader } from "./session-account"

export default Plugin.define({
  id: "exetrouter",
  async setup(ctx) {
    const config = options(ctx.options)
    await ctx.integration.transform((editor) => {
      editor.method.update({ integrationID: providerID, method: { type: "key", label: "ExetRouter token" } })
      editor.method.update({ integrationID: providerID, method: { type: "env", names: ["EXETROUTER_TOKEN"] } })
      editor.update(providerID, (integration) => { integration.name = "ExetRouter" })
    })
    const catalog = new Catalog(config, async () => {
      const connection = await ctx.integration.connection.active(providerID)
      if (!connection) return undefined
      const credential = await ctx.integration.connection.resolve(connection)
      return credential?.type === "key" && credential.key ? { connection, token: credential.key } : undefined
    }, () => ctx.provider.reload())

    await ctx.provider.transform((editor) => {
      editor.add({
        info: {
          ...Provider.Info.empty(providerID),
          integrationID: Integration.ID.make(providerID),
          name: "ExetRouter",
          activation: "enabled",
          package: providerPackage,
          settings: {
            baseURL: config.baseURL,
            transport: config.transport,
            store: false,
            compaction: { type: "native" },
          },
        },
        models: catalog.inventory?.models ?? [],
        sourceConnection: catalog.inventory?.connection,
      })
    })
    await ctx.rpc.register(ExetRouter, {
      status: async () => catalog.status,
      refresh: () => catalog.refresh(),
    })
    const accounts = new SessionAccounts((sessionID, account) => { void accountRpc.events.emit("changed", { sessionID, account }).catch(() => {}) })
    const accountRpc = await ctx.rpc.register(Accounts, { get: async ({ sessionID }) => accounts.get(sessionID) })
    await ctx.session.hook("model.request", (event) => {
      accounts.request(event)
      if (event.kind === "primary" && event.model.providerID === providerID) event.headers[observeHeader] = "true"
    })
    await ctx.session.hook("http.response", (event) => accounts.http(event), { providerID })
    await ctx.session.hook("experimental.ws.receive", (event) => accounts.websocket(event), { providerID })

    const events = new AbortController()
    const watch = (async () => {
      try {
        for await (const _ of ctx.event.subscribe({ types: ["credential.updated", "credential.switched"], signal: events.signal })) {
          await catalog.refresh()
        }
      } catch { /* Closed with the plugin or an unavailable service. No payload/error logging. */ }
    })()
    const timer = setInterval(() => { void catalog.refresh().catch(() => {}) }, config.refreshSeconds * 1000)
    // Do not delay loading the GUI on a slow or unavailable model endpoint.
    void catalog.refresh().catch(() => {})
    return async () => {
      clearInterval(timer)
      events.abort()
      await catalog.close()
      await watch
    }
  },
})
