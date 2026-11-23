import { ErrorBoundary, For, Show, batch, createEffect, createMemo, createSignal, createUniqueId } from "solid-js"
import { createLatest } from "@opencode/plugin/gui"
import { Dashboard } from "./contract"
import type { ForecastLookup } from "./dashboard"
import { Overview } from "./overview"
import { UsageView } from "./usage-view"
import { ModelsView, SettingsView, TokensView } from "./resource-views"
import { DialogScope, Notice, RefreshButton, Segments, useDashboardState, useHost } from "./ui"

const tabs = ["Overview", "Usage", "Tokens", "Models", "Settings"] as const
type Tab = typeof tabs[number]

export function DashboardView(props: { remote: boolean; now: number; forecast: ForecastLookup }) {
  const ctx = useHost()
  const state = useDashboardState()
  const id = createUniqueId()
  const preferences = ctx.stores.preferences
  const storedTab = createMemo(() => preferences.value.tab)
  const [tab, setTab] = createSignal<Tab>(preferences.value.tab)
  const [visited, setVisited] = createSignal<ReadonlySet<Tab>>(new Set([preferences.value.tab]))
  createEffect(() => {
    const value = storedTab()
    batch(() => { setVisited((current) => new Set([...current, value])); setTab(value) })
  })
  const [profileRevision, setProfileRevision] = createSignal(0)
  const [manualRefresh, setManualRefresh] = createSignal(false)
  const bridge = ctx.main(Dashboard)
  const profileLive = createMemo((previous: number) => tab() === "Settings" ? state()?.revision ?? state()?.snapshot?.checkedAt ?? previous : previous, 0)
  const profile = createLatest(() => ({ revision: profileRevision(), live: profileLive() }), (_, signal) => bridge.profile({ signal }))
  const select = (value: Tab) => preferences.update((draft) => { draft.tab = value })
  const refresh = async () => { if (manualRefresh()) return; setManualRefresh(true); try { await bridge.refresh({ signal: ctx.signal }) } catch { ctx.toast.show({ variant: "error", title: "Could not refresh router snapshot" }) } finally { setManualRefresh(false) } }
  const revision = () => tab() + ":" + (state()?.revision ?? state()?.snapshot?.checkedAt ?? 0)
  return <DialogScope state={state}><div class="exr exr-dashboard">
    <div class="exr-panel-toolbar">
      <Segments tabs id={id} label="ExetRouter views" value={tab()} options={tabs.map((value) => ({ value, label: value, attention: value === "Settings" && state()?.update?.available === true }))} onChange={select} />
      <RefreshButton label="Refresh router snapshot" busy={manualRefresh()} onClick={() => void refresh()} />
    </div>
    <Show when={props.remote}><div class="exr-remote"><Notice tone="warning">Local exr profile. This is not the remote OpenCode server's router configuration.</Notice></div></Show>
    <For each={tabs}>{(name) => <section class="exr-pane" data-scrollable role="tabpanel" tabIndex={0} hidden={tab() !== name} id={`${id}-view-${name}`} aria-labelledby={`${id}-tab-${name}`}>
      <Show when={visited().has(name)}>
        <Show when={name === "Overview"}><Overview now={props.now} forecast={props.forecast} /></Show>
        <Show when={name === "Usage"}><ErrorBoundary fallback={(_, reset) => <div class="exr-view"><Notice tone="warning">Could not display usage. Other dashboard views remain available.</Notice><ctx.ui.Button onClick={reset}>Retry usage</ctx.ui.Button></div>}><UsageView now={props.now} revision={tab() === "Usage" ? revision() : undefined} /></ErrorBoundary></Show>
        <Show when={name === "Tokens"}><TokensView now={props.now} revision={tab() === "Tokens" ? revision() : undefined} /></Show>
        <Show when={name === "Models"}><ModelsView revision={tab() === "Models" ? revision() : undefined} /></Show>
        <Show when={name === "Settings"}><SettingsView profile={profile} refreshProfile={() => setProfileRevision((n) => n + 1)} /></Show>
      </Show>
    </section>}</For>
  </div></DialogScope>
}
