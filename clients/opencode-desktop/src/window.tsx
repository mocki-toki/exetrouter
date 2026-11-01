import { ComposerPill, Command, MenuItem, Panel, Style, createKeyed, type Context, type Plugin } from "@opencode/plugin/gui"
import { createMemo, createSignal, getOwner, onCleanup, runWithOwner } from "solid-js"
import type { DashboardState, SessionAccount } from "./schemas"
import { Accounts } from "./rpc"
import { Dashboard } from "./contract"
import { quotaIndicator } from "./indicator"
import { openAccount } from "./overview"
import { DashboardView } from "./panel"
import css from "./window.css?inline"

export const setup: Plugin.Setup = (ctx) => {
  const owner = getOwner()
  const state = createMemo((previous: DashboardState | undefined) => ctx.main(Dashboard).state() ?? previous)
  const [accounts, setAccounts] = createSignal(new Map<string, SessionAccount>())
  const key = (server: string, session: string) => `${server}\0${session}`
  const remember = (id: string, account: SessionAccount | null) => setAccounts((old) => {
    const next = new Map(old); next.delete(id)
    if (account) next.set(id, account)
    if (next.size > 128) next.delete(next.keys().next().value!)
    return next
  })
  // Same snapshot + subscription pattern as OpenCode Desktop's own Goal pill.
  createKeyed(() => {
    const session = ctx.sessions.current()
    return session?.server.connected ? `${key(session.server.id, session.id)}\0${session.directory}` : undefined
  }, () => {
    const session = ctx.sessions.current()
    if (!session) return
    const id = key(session.server.id, session.id), rpc = session.server.client.rpc(Accounts)
    const controller = new AbortController()
    let heard = false
    onCleanup(() => controller.abort())
    onCleanup(rpc.events.on("changed", (event) => {
      if (event.location.directory !== session.directory || event.data.sessionID !== session.id) return
      heard = true; remember(id, event.data.account)
    }, { signal: controller.signal }))
    void rpc.get({ sessionID: session.id }, { location: { directory: session.directory }, signal: controller.signal }).then((account) => {
      if (!controller.signal.aborted && !heard) remember(id, account)
    }, () => {})
  })
  const [now, setNow] = createSignal(Math.floor(Date.now() / 1000))
  const clock = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000)
  const dialog: Context["dialogs"]["open"] = (render, options) => runWithOwner(owner, () => ctx.dialogs.open(render, options))!
  ctx.add(Style, css)
  const open = () => { const session = ctx.sessions.current(); if (session) ctx.layout.open(`${ctx.id}:dashboard`, session) }
  ctx.add(Command, (): Command => ({ id: "open", title: "Open ExetRouter", keywords: "router quota usage limits", enabled: !!ctx.sessions.current(), run: open }))
  ctx.add(MenuItem, (): MenuItem => ({ menu: "session.panel", id: "open", title: "ExetRouter", icon: "server", run: open }))
  ctx.add(ComposerPill, {
    id: "quota",
    show: (input) => {
      const indicator = quotaIndicator(state(), Math.floor(Date.now() / 1000), accounts().get(key(input.session.server.id, input.session.id)) ?? null)
      if (!indicator) return
      return { label: indicator.label, title: indicator.title, icon: "server", run: () => {
        if (indicator.id !== null) openAccount(ctx, dialog, indicator.id, now, state)
        else ctx.layout.open(`${ctx.id}:dashboard`, input.session)
      } }
    },
  })
  const tab = { id: "dashboard", title: "ExetRouter", closable: "compact" as const, fallback: true }
  ctx.add(Panel, { id: "dashboard", region: "side", list: (input) => input.open.includes("dashboard") ? [tab] : [], render: (props) => <DashboardView remote={!props.session.server.builtin} /> })
  return () => clearInterval(clock)
}
