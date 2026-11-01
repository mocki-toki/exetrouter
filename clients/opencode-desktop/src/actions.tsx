import { Show, createSignal, onCleanup, type JSX } from "solid-js"
import type { Context, DialogHandle } from "@opencode/plugin/gui"
import { Dashboard } from "./contract"
import type { Connection, DesktopAction, DesktopResult, Profile } from "./schemas"
import { Notice, Segments, useDashboardState, useDialog, useHost } from "./ui"

const outcomeWarning = "The operation did not complete normally. Its outcome may be unknown. Refresh metadata before repeating it."
type Open = Context["dialogs"]["open"]

function ActionDialog(props: { title: string; description: string; submit: string; dialog: DialogHandle; input(): DesktopAction; valid?(): boolean; danger?: boolean; done?(value: DesktopResult): void; children?: JSX.Element }) {
  const ctx = useHost(), ui = ctx.ui
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal(""), [finished, setFinished] = createSignal(false)
  let disposed = false
  onCleanup(() => { disposed = true })
  const submit = async () => {
    if (busy() || error() || finished() || props.valid?.() === false) return
    setBusy(true)
    try {
      const result = await ctx.main(Dashboard).desktop(props.input(), { signal: ctx.signal })
      if (result.status === "clipboard_unavailable") {
        if (!disposed) setError("No native clipboard helper is available. The token was not issued or rotated. Install a clipboard helper before trying again.")
        return
      }
      props.done?.(result)
      ctx.toast.show({ variant: result.retryCopy ? "error" : "success", title: result.retryCopy ? "Token issued, but not copied. Retry copying in Tokens." : result.copied ? "Secret copied to clipboard" : "Changes applied" })
      if (!disposed) { setFinished(true); props.dialog.close() }
    } catch { if (!disposed) setError(outcomeWarning) }
    finally { if (!disposed) setBusy(false) }
  }
  return <ui.Dialog title={props.title} description={props.description} footer={<>
    <ui.Button variant="ghost" disabled={busy()} onClick={props.dialog.close}>{error() ? "Close" : "Cancel"}</ui.Button>
    <ui.Button variant={props.danger ? "danger" : "neutral"} busy={busy()} disabled={!!error() || finished() || props.valid?.() === false} onClick={() => void submit()}>{props.submit}</ui.Button>
  </>}><Show when={props.children || error()}><div class="exr exr-dialog-content exr-form" onKeyDown={(event) => { if (event.key === "Enter" && !event.isComposing) { event.preventDefault(); void submit() } }}>
    {props.children}<Show when={error()}><Notice tone="warning">{error()}</Notice></Show>
  </div></Show></ui.Dialog>
}

export function tokenForm(open: Open, token: { id: string; name: string } | undefined, done: () => void) {
  open((dialog) => {
    const ui = useHost().ui
    const [name, setName] = createSignal(token?.name ?? ""), [days, setDays] = createSignal("90")
    const valid = () => !!name().trim() && new TextEncoder().encode(name().trim()).length <= 80 && !/[\u0000-\u001f\u007f]/.test(name()) && (!!token || /^\d+$/.test(days()) && Number(days()) >= 1 && Number(days()) <= 365)
    return <ActionDialog dialog={dialog} title={token ? "Rename token" : "Create token"} description={token ? "Change the encrypted label without changing its secret." : "The secret will be copied directly to your clipboard, never displayed."} submit={token ? "Save name" : "Create and copy"} valid={valid}
      input={() => token ? { action: "token_rename", id: token.id, name: name().trim() } : { action: "token_create", name: name().trim(), days: Number(days()) }} done={done}>
      <label>Token name<ui.TextInput label="Token name" value={name()} onInput={setName} /></label>
      <Show when={!token}><label>Expires in days<ui.TextInput label="Expires in days" value={days()} onInput={setDays} /></label><p class="exr-caption exr-field-help">Choose 1–365 days. Clipboard support is checked before issuance.</p></Show>
    </ActionDialog>
  })
}

export function confirmAction(open: Open, options: { title: string; description: string; submit: string; input: DesktopAction; done?(): void; danger?: boolean }) {
  open((dialog) => <ActionDialog dialog={dialog} title={options.title} description={options.description} submit={options.submit} danger={options.danger} input={() => options.input} done={options.done} />)
}

export function ClipboardNotice(props: { refresh(): void }) {
  const ctx = useHost(), ui = ctx.ui, open = useDialog()
  const state = useDashboardState()
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal(false)
  const retry = async () => {
    if (busy()) return
    setBusy(true); setError(false)
    try {
      const result = await ctx.main(Dashboard).desktop({ action: "copy_retry" }, { signal: ctx.signal })
      if (result.copied) ctx.toast.show({ variant: "success", title: "Secret copied to clipboard" })
      else setError(true)
      props.refresh()
    } catch { setError(true) } finally { setBusy(false) }
  }
  return <Show when={state()?.clipboardPending}><Notice tone="warning"><p>A token was issued, but its secret has not been copied. Do not issue it again.</p>
    <div class="exr-inline-start"><ui.Button size="small" busy={busy()} onClick={() => void retry()}>Retry copying</ui.Button><ui.Button size="small" variant="ghost" onClick={() => confirmAction(open, { title: "Discard uncopied secret?", description: "This secret cannot be retrieved afterward. You can rotate the token to replace it.", submit: "Discard secret", input: { action: "copy_discard" }, danger: true })}>Discard secret</ui.Button></div>
    <Show when={error()}><p>Clipboard is still unavailable. The same secret is retained for another copy attempt.</p></Show>
  </Notice></Show>
}

export function configureConnection(open: Open, profile: typeof Profile.Type, done: () => void) {
  if (!profile.connection) return
  open((dialog) => {
    const ui = useHost().ui
    const [value, setValue] = createSignal<typeof Connection.Type>({ ...profile.connection! })
    const valid = () => value().mode === "standalone" ? !!value().state_dir.trim() && !!value().listen.trim() : !!value().host.trim() && !!value().ssh_user.trim() && !!value().identity.trim() && Number.isInteger(value().port) && value().port >= 1 && value().port <= 65535
    const field = (key: Exclude<keyof typeof Connection.Type, "mode" | "port">, label: string) => <label>{label}<ui.TextInput label={label} value={String(value()[key] ?? "")} onInput={(text) => setValue((v) => ({ ...v, [key]: text || (key === "api_url" ? null : "") }))} /></label>
    return <ActionDialog dialog={dialog} title="Configure connection" description="Save the native exr profile. Existing keys and private state are preserved." submit="Save connection" valid={valid} input={() => ({ action: "configure", connection: value(), confirmed: true })} done={done}>
      <Segments label="Connection type" value={value().mode} options={[{ value: "remote", label: "Remote server" }, { value: "standalone", label: "Standalone" }]} onChange={(mode) => setValue((v) => ({ ...v, mode }))} />
      <Show when={value().mode === "remote"} fallback={<>{field("listen", "Loopback listen address")}{field("state_dir", "Private state directory")}</>}>
        {field("api_url", "HTTP API URL (optional)")}{field("host", "SSH host")}
        <label>SSH port<ui.TextInput label="SSH port" value={String(value().port)} onInput={(text) => setValue((v) => ({ ...v, port: Number(text) }))} /></label>
        {field("ssh_user", "SSH username")}{field("identity", "Private key path")}
        <p class="exr-caption">Register your public key with the operator and verify the host key first. The private key itself is not stored in this profile.</p>
      </Show>
    </ActionDialog>
  })
}

export function loginAccount(open: Open, account: { id: number; email: string } | undefined, done: () => void) {
  open((dialog) => <LoginDialog dialog={dialog} account={account} done={done} />)
}

function LoginDialog(props: { dialog: DialogHandle; account?: { id: number; email: string }; done(): void }) {
  const ctx = useHost(), ui = ctx.ui
  const [login, setLogin] = createSignal<DesktopResult["login"]>(null), [error, setError] = createSignal(false)
  let disposed = false, completed = false, timer: ReturnType<typeof setTimeout> | undefined
  const bridge = ctx.main(Dashboard)
  const poll = async () => {
    try {
      const result = await bridge.desktop({ action: "login_status" }, { signal: ctx.signal })
      if (disposed) return
      if (result.status === "complete") { completed = true; props.done(); props.dialog.close(); ctx.toast.show({ variant: "success", title: "Account signed in" }); return }
      if (result.status !== "pending") { setError(true); return }
      timer = setTimeout(() => void poll(), 2000)
    } catch { if (!disposed) setError(true) }
  }
  void bridge.desktop({ action: "login_start", account: props.account?.id ?? null }, { signal: ctx.signal }).then((result) => {
    if (disposed) { void bridge.desktop({ action: "login_cancel" }, { signal: ctx.signal }).catch(() => {}); return }
    if (!result.login) { setError(true); return }
    setLogin(result.login); timer = setTimeout(() => void poll(), 2000)
  }, () => { if (!disposed) setError(true) })
  onCleanup(() => { disposed = true; clearTimeout(timer); if (!completed) void bridge.desktop({ action: "login_cancel" }, { signal: ctx.signal }).catch(() => {}) })
  return <ui.Dialog title={props.account ? "Reauthorize account" : "Add account"} description={props.account?.email ?? "Sign in to ChatGPT through your browser."} footer={<ui.Button variant="ghost" onClick={props.dialog.close}>Cancel sign-in</ui.Button>}>
    <div class="exr exr-dialog-content"><Show when={error()} fallback={<Show when={login()} fallback={<div class="exr-loading"><ui.Loader />Requesting browser sign-in…</div>}>{(code) => <>
      <p>Enter this one-time code in your browser:</p><p class="exr-login-code">{code().code}</p>
      <ui.Button onClick={() => { try { const url = new URL(code().url); if (url.protocol === "https:" && ["auth.openai.com", "chatgpt.com"].includes(url.hostname)) void ctx.system.openExternal(url.href) } catch { setError(true) } }}>Open sign-in page</ui.Button>
      <p class="exr-caption">Waiting for sign-in. OAuth credentials are never sent to this window.{props.account ? " Sign in to the same account to reauthorize it." : ""}</p>
    </>}</Show>}><Notice tone="warning">Sign-in could not be completed. Close this dialog and inspect account metadata before trying again.</Notice></Show></div>
  </ui.Dialog>
}
