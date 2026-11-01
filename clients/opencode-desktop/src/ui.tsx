import { For, Show, createContext, createMemo, useContext, getOwner, runWithOwner, createSignal, createUniqueId, onCleanup, type Accessor, type JSX } from "solid-js"
import { usePlugin, type Context, type IconName } from "@opencode/plugin/gui"
import type { definition } from "./definition"
import { Dashboard } from "./contract"
import type { DashboardState } from "./schemas"

export const useHost = () => usePlugin<typeof definition>()
const DialogOwner = createContext<Context["dialogs"]["open"]>()
const DashboardCache = createContext<Accessor<DashboardState | undefined>>()
export function useDashboardState() {
  const ctx = useHost()
  return useContext(DashboardCache) ?? createMemo((previous: DashboardState | undefined) => ctx.main(Dashboard).state() ?? previous)
}
/** Dialogs belong to the stable panel, never to a refreshed list row. */
export function DialogScope(props: { state: Accessor<DashboardState | undefined>; children: JSX.Element }) {
  const ctx = usePlugin(), owner = getOwner()
  const open: Context["dialogs"]["open"] = (render, options) => runWithOwner(owner, () => ctx.dialogs.open(render, options))!
  return <DashboardCache.Provider value={props.state}><DialogOwner.Provider value={open}>{props.children}</DialogOwner.Provider></DashboardCache.Provider>
}
export const useDialog = () => useContext(DialogOwner) ?? usePlugin().dialogs.open
export type Tone = "neutral" | "success" | "warning" | "danger"

export function Badge(props: { children: JSX.Element; tone?: Tone }) {
  return <span class="exr-badge" data-tone={props.tone ?? "neutral"}>{props.children}</span>
}

export function Section(props: { title: string; description?: string; action?: JSX.Element; children: JSX.Element }) {
  return <section class="exr-section">
    <div class="exr-section-heading"><div><h3>{props.title}</h3><Show when={props.description}><p class="exr-caption">{props.description}</p></Show></div>{props.action}</div>
    {props.children}
  </section>
}

/** Scoped equivalents of app/settings/{list,row}.tsx; their CSS is lazy-loaded by the host. */
export function List(props: { children: JSX.Element }) {
  return <div class="exr-list" data-component="exr-settings-list">{props.children}</div>
}

export function Row(props: { title: JSX.Element; description?: JSX.Element; children?: JSX.Element }) {
  return <div class="exr-list-row" data-component="exr-settings-row">
    <div class="exr-row-main" data-slot="row-copy"><div data-slot="row-title">{props.title}</div><Show when={props.description}><div data-slot="row-description">{props.description}</div></Show></div>
    <Show when={props.children}><div data-slot="row-control">{props.children}</div></Show>
  </div>
}

/** The SDK exposes TextInput, but not SettingsSearchField's leading/clear adornments. */
export function SearchField(props: { label: string; value: string; onInput(value: string): void }) {
  const ui = useHost().ui
  let root: HTMLDivElement | undefined
  const clear = () => { props.onInput(""); root?.querySelector("input")?.focus({ preventScroll: true }) }
  return <div class="exr-search" ref={root} onKeyDown={(event) => {
    if (event.key !== "Escape" || event.isComposing || !props.value) return
    event.preventDefault(); event.stopPropagation(); clear()
  }}>
    <span class="exr-search-icon" aria-hidden="true"><ui.Icon name="magnifying-glass" size="small" /></span>
    <ui.TextInput type="search" label={props.label} placeholder={`${props.label}…`} value={props.value} onInput={props.onInput} class="exr-search-input" />
    <Show when={props.value}><span class="exr-search-clear"><ui.IconButton icon="circle-xmark" label={`Clear ${props.label.toLowerCase()}`} size="small" onClick={clear} /></span></Show>
  </div>
}

export function Notice(props: { children: JSX.Element; tone?: Tone }) {
  const ui = useHost().ui
  return <div class="exr-notice" data-tone={props.tone ?? "neutral"} role={props.tone === "danger" ? "alert" : "status"}>
    <ui.Icon name={props.tone === "danger" || props.tone === "warning" ? "circle-exclamation" : "info"} size="small" /><div>{props.children}</div>
  </div>
}

export function EmptyState(props: { title: string; description: string; icon?: IconName }) {
  const ui = useHost().ui
  return <div class="exr-empty"><span class="exr-empty-icon"><ui.Icon name={props.icon ?? "archive"} size="large" /></span><h4>{props.title}</h4><p>{props.description}</p></div>
}

export function ReadState(props: { loading: boolean; error: unknown; hasData: boolean; noun?: string }) {
  const ui = useHost().ui
  return <>
    <Show when={props.loading && !props.hasData}><div class="exr-loading" role="status"><ui.Loader /><span>Loading {props.noun ?? "data"}…</span></div></Show>
    <Show when={props.error}><Notice tone="warning">Could not load {props.noun ?? "data"}. Check your exr profile and connection.{props.hasData ? " Previous data is shown below." : ""}</Notice></Show>
  </>
}

export function Stat(props: { label: string; value: JSX.Element; detail?: JSX.Element }) {
  return <div class="exr-stat"><span class="exr-caption">{props.label}</span><strong>{props.value}</strong><Show when={props.detail}><span class="exr-caption">{props.detail}</span></Show></div>
}

export function Fields(props: { rows: readonly (readonly [string, JSX.Element])[] }) {
  return <dl class="exr-fields"><For each={props.rows}>{(row) => <div><dt>{row[0]}</dt><dd>{row[1]}</dd></div>}</For></dl>
}

export function RefreshButton(props: { label: string; busy?: boolean; onClick(): void }) {
  const ui = useHost().ui
  return <ui.Tooltip value={props.label}><ui.IconButton icon="outline-rotate-clockwise" label={props.label} busy={props.busy} onClick={props.onClick} /></ui.Tooltip>
}

export function CopyButton(props: { value: string; label: string }) {
  const ctx = useHost(), ui = ctx.ui
  const [copied, setCopied] = createSignal(false)
  const [busy, setBusy] = createSignal(false)
  let reset: ReturnType<typeof setTimeout> | undefined
  let disposed = false
  onCleanup(() => { disposed = true; clearTimeout(reset) })
  const copy = async () => {
    if (busy()) return
    setBusy(true)
    try {
      await ctx.system.copy(props.value)
      if (disposed) return
      setCopied(true)
      clearTimeout(reset)
      reset = setTimeout(() => setCopied(false), 2000)
    } catch { if (!disposed) ctx.toast.show({ variant: "error", title: "Could not copy to clipboard" }) }
    finally { if (!disposed) setBusy(false) }
  }
  return <ui.Tooltip value={copied() ? "Copied" : props.label}><ui.IconButton icon={copied() ? "check" : "copy"} label={copied() ? "Copied" : props.label} size="small" busy={busy()} onClick={() => void copy()} /></ui.Tooltip>
}

export function MenuSelect<T extends string>(props: { label: string; value: T; options: readonly { value: T; label: string }[]; onChange(value: T): void }) {
  const ui = useHost().ui
  return <div class="exr-menu-select"><ui.Menu label={`${props.label}: ${props.options.find((o) => o.value === props.value)?.label ?? props.value}`} trigger="button" icon="chevron-down" placement="bottom-start"
    items={props.options.map((o) => ({ label: o.label, icon: o.value === props.value ? "check" as const : undefined, onSelect: () => props.onChange(o.value) }))} /></div>
}

/** The host SDK has no tabs/segmented control; this is the one accessible implementation. */
export function Segments<T extends string>(props: { label: string; value: T; options: readonly { value: T; label: string; attention?: boolean }[]; onChange(value: T): void; tabs?: boolean; id?: string }) {
  const id = props.id ?? createUniqueId()
  const navigate: JSX.EventHandler<HTMLDivElement, KeyboardEvent> = (event) => {
    const current = props.options.findIndex((o) => o.value === props.value)
    const length = props.options.length
    const next = event.key === "ArrowRight" ? (current + 1) % length : event.key === "ArrowLeft" ? (current + length - 1) % length : event.key === "Home" ? 0 : event.key === "End" ? length - 1 : null
    if (next === null) return
    event.preventDefault()
    props.onChange(props.options[next].value)
    event.currentTarget.querySelectorAll<HTMLButtonElement>("button")[next]?.focus()
  }
  return <div class={props.tabs ? "exr-tabs" : "exr-segments"} data-variant={props.tabs ? "line" : "pill"} role={props.tabs ? "tablist" : "group"} aria-label={props.label} onKeyDown={navigate}>
    <For each={props.options}>{(option) => <div data-slot="tab-wrapper" data-selected={props.value === option.value ? "" : undefined}><button type="button" data-slot="tab-trigger" role={props.tabs ? "tab" : undefined} id={props.tabs ? `${id}-tab-${option.value}` : undefined}
      aria-controls={props.tabs ? `${id}-view-${option.value}` : undefined} aria-selected={props.tabs ? props.value === option.value : undefined}
      aria-pressed={!props.tabs ? props.value === option.value : undefined} tabIndex={props.value === option.value ? 0 : -1}
      data-active={props.value === option.value} onClick={() => props.onChange(option.value)}>{option.label}<Show when={option.attention}><span class="exr-attention" aria-label="Update available" /></Show></button></div>}</For>
  </div>
}
