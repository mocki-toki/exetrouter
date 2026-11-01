import type { Plugin } from "@opencode/plugin/gui"
import { Dashboard } from "./contract"
import { NativeClient } from "./native"
import { options } from "./options"
import type { DashboardState } from "./schemas"
import { DesktopClient } from "./desktop"

export const main: Plugin.MainSetup = (ctx) => {
  const client = new NativeClient(options(ctx.options))
  const desktop = new DesktopClient(options(ctx.options))
  let state: DashboardState = { revision: 0, snapshot: null, refreshing: false, error: null, update: null, clipboardPending: false }
  let pending: Promise<void> | undefined
  const refresh = () => {
    pending ??= (async () => {
      state = { ...state, refreshing: true }
      bridge.changed()
      try {
        await client.whenAvailable()
        const snapshot = await client.snapshot(ctx.signal)
        state = { ...state, revision: state.revision + 1, snapshot, refreshing: false, error: null }
      } catch {
        state = { ...state, revision: state.revision + 1, refreshing: false, error: "Snapshot unavailable. Check exrBinary, the saved exr profile and SSH key." }
      }
      if (!ctx.signal.aborted) bridge.changed()
    })().finally(() => { pending = undefined })
    return pending
  }
  const bridge = ctx.provide(Dashboard, {
    state: () => state,
    refresh,
    usage: (input, caller) => client.usage(input, AbortSignal.any([ctx.signal, caller.signal])),
    tokens: (_, caller) => client.tokens(AbortSignal.any([ctx.signal, caller.signal])),
    models: (_, caller) => client.models(AbortSignal.any([ctx.signal, caller.signal])),
    profile: async () => {
      const result = await desktop.request({ action: "profile" })
      if (!result.profile) throw new Error("Native profile unavailable.")
      return result.profile
    },
    desktop: async (input) => {
      // Once submitted, mutations follow the main lifetime, not a disappearing dialog.
      // Never abort/retry issuance when a window closes or a read refreshes.
      const result = await (input.action === "login_cancel" ? desktop.cancelLogin() : desktop.request(input))
      state = { ...state, clipboardPending: result.retryCopy || (state.clipboardPending && !["copy_retry", "copy_discard"].includes(input.action)), ...(result.update ? { update: result.update } : {}) }
      bridge.changed()
      if (!["profile", "login_start", "login_status", "login_cancel", "copy_retry", "copy_discard"].includes(input.action)) void refresh()
      return result
    },
    update: async (_, caller) => {
      const update = await client.update(AbortSignal.any([ctx.signal, caller.signal]))
      state = { ...state, update }
      bridge.changed()
      return update
    },
  })
  // One passive poll for the whole app, not one subprocess per window.
  const timer = setInterval(() => { void refresh() }, 30_000)
  void refresh()
  // Match the TUI's single passive startup release check; never install in polling.
  const checking = process.env.EXR_NO_UPDATE_CHECK === undefined ? (pending ?? Promise.resolve()).then(() => client.update(ctx.signal)).then((update) => {
    if (!ctx.signal.aborted) { state = { ...state, update }; bridge.changed() }
  }, () => {}) : Promise.resolve()
  return async () => { clearInterval(timer); await pending; await checking; await desktop.close() }
}
