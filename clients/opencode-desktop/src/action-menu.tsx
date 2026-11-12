import { For, Show, createEffect, createSignal, onCleanup } from "solid-js"
import { Portal } from "solid-js/web"
import { useHost } from "./ui"

type Action = { label: string; disabled?: boolean; checked?: boolean; run(): void }
type Entry = Action | { label: string; disabled?: boolean; items: readonly Action[] }

/** Ui.Menu's item API cannot contain submenus, so keep this small accessible
 * root local until the SDK exposes a composable menu root. */
export function ActionMenu(props: { label: string; entries: readonly Entry[] }) {
  const ui = useHost().ui
  const [open, setOpen] = createSignal(false), [sub, setSub] = createSignal<number>()
  const [position, setPosition] = createSignal({ left: 0, top: 0, flip: false })
  let trigger!: HTMLSpanElement, menu: HTMLDivElement | undefined
  const close = (focus = false) => { setOpen(false); setSub(undefined); if (focus) trigger.querySelector("button")?.focus() }
  const focus = (nested = false, last = false) => {
    const root = nested ? menu?.querySelector('[role="menu"]') : menu
    const items = root ? [...root.querySelectorAll<HTMLButtonElement>(':scope > .exr-action-entry > button:not(:disabled)')] : []
    items[last ? items.length - 1 : 0]?.focus()
  }
  createEffect(() => {
    const button = trigger.querySelector("button")
    button?.setAttribute("aria-haspopup", "menu")
    button?.setAttribute("aria-expanded", String(open()))
    if (!open()) return
    const outside = (event: PointerEvent) => { if (!menu?.contains(event.target as Node) && !trigger.contains(event.target as Node)) close() }
    const scroll = (event: Event) => { if (!menu?.contains(event.target as Node)) close() }
    const resize = () => close()
    document.addEventListener("pointerdown", outside)
    document.addEventListener("scroll", scroll, true)
    window.addEventListener("resize", resize)
    onCleanup(() => { document.removeEventListener("pointerdown", outside); document.removeEventListener("scroll", scroll, true); window.removeEventListener("resize", resize) })
    queueMicrotask(() => focus())
  })
  const toggle = () => {
    if (open()) { close(); return }
    const rect = trigger.getBoundingClientRect()
    setPosition({ left: Math.max(8, Math.min(window.innerWidth - 196, rect.right - 188)), top: Math.min(rect.bottom + 4, Math.max(8, window.innerHeight - 250)), flip: rect.right + 160 > window.innerWidth })
    setOpen(true)
  }
  const run = (entry: Action) => { close(true); entry.run() }
  return <span ref={trigger} onKeyDown={(event) => { if (event.key === "ArrowDown") { event.preventDefault(); if (!open()) toggle(); else focus() } }}>
    <ui.IconButton icon="outline-dots" label={props.label} onClick={toggle} />
    <Show when={open()}><Portal><div ref={menu} class="exr exr-action-menu" role="menu" aria-label={props.label} style={{ left: `${position().left}px`, top: `${position().top}px` }} onKeyDown={(event) => {
      if (event.key === "Escape") { event.preventDefault(); close(true); return }
      if (event.key === "Tab") { close(true); return }
      const root = (event.target as HTMLElement).closest('[role="menu"]')
      const items = root ? [...root.querySelectorAll<HTMLButtonElement>(':scope > .exr-action-entry > button:not(:disabled)')] : []
      const current = items.indexOf(event.target as HTMLButtonElement)
      if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        event.preventDefault(); event.stopPropagation()
        const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (current + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length
        items[next]?.focus()
      }
      if (event.key === "ArrowRight" && root === menu && (event.target as HTMLElement).getAttribute("aria-haspopup")) { event.preventDefault(); const i = Number((event.target as HTMLElement).dataset.index); setSub(i); queueMicrotask(() => focus(true)) }
      if (event.key === "ArrowLeft" && root !== menu) { event.preventDefault(); event.stopPropagation(); const i = sub(); setSub(undefined); menu?.querySelector<HTMLButtonElement>(`[data-index="${i}"]`)?.focus() }
    }}>
      <For each={props.entries}>{(entry, index) => <div class="exr-action-entry" onPointerEnter={() => setSub("items" in entry && !entry.disabled ? index() : undefined)}>
        <button type="button" role="menuitem" data-index={index()} disabled={entry.disabled} aria-haspopup={"items" in entry ? "menu" : undefined} aria-expanded={"items" in entry ? sub() === index() : undefined} onClick={() => "items" in entry ? setSub(sub() === index() ? undefined : index()) : run(entry)}>{entry.label}<Show when={"items" in entry}><ui.Icon name="chevron-right" size="small" /></Show></button>
        <Show when={"items" in entry && sub() === index()}><div class="exr-action-menu exr-action-submenu" role="menu" aria-label={entry.label} data-flip={position().flip ? "" : undefined}><For each={"items" in entry ? entry.items : []}>{(item) => <div class="exr-action-entry"><button type="button" role="menuitemradio" aria-checked={!!item.checked} disabled={item.disabled} onClick={() => run(item)}>{item.label}<Show when={item.checked}><ui.Icon name="check" size="small" /></Show></button></div>}</For></div></Show>
      </div>}</For>
    </div></Portal></Show>
  </span>
}
