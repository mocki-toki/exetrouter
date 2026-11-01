import { For, Show, createMemo, createSignal } from "solid-js"
import { createLatest } from "@opencode/plugin/gui"
import { Dashboard } from "./contract"
import { count, date } from "./format"
import type { Profile, Tokens } from "./schemas"
import { Badge, CopyButton, EmptyState, Fields, List, Notice, ReadState, Row, SearchField, Section, useDashboardState, useDialog, useHost } from "./ui"
import { ClipboardNotice, confirmAction, configureConnection, loginAccount, tokenForm } from "./actions"

export function TokensView(props: { now: number; revision?: string }) {
  const ctx = useHost(), ui = ctx.ui, open = useDialog()
  const state = useDashboardState()
  const [revision, setRevision] = createSignal(0)
  const [query, setQuery] = createSignal("")
  const live = createMemo((previous: string) => props.revision ?? previous, "initial")
  const refresh = () => setRevision((n) => n + 1)
  const report = createLatest(() => ({ revision: revision(), live: live() }), (_, signal) => ctx.main(Dashboard).tokens({ signal }))
  const rows = createMemo(() => report.latest?.filter((token) => `${token.name} ${token.id}`.toLowerCase().includes(query().trim().toLowerCase())) ?? [])
  const status = (token: typeof Tokens.Type[number]) => token.status === "Active" && token.expiresAt !== null && token.expiresAt <= props.now ? "Expired" : token.status
  const details = (id: string) => open((dialog) => {
    const token = () => report.latest?.find((t) => t.id === id)
    return <ui.Dialog title={token()?.name ?? "Token details"} description="Metadata only. The secret cannot be retrieved." footer={<ui.Button onClick={dialog.close}>Done</ui.Button>}>
      <div class="exr exr-dialog-content"><Show when={token()}>{(t) => <Fields rows={[["Token ID", <span class="exr-inline-start"><code>{t().id}</code><CopyButton value={t().id} label="Copy token ID" /></span>], ["Name", t().name], ["Status", status(t())], ["Created", date(t().createdAt)], ["Expires", date(t().expiresAt)], ["Last used", date(t().lastUsedAt)]]} />}</Show></div>
    </ui.Dialog>
  })
  return <div class="exr-view"><Section title="Your tokens" action={<ui.Button size="small" disabled={state()?.clipboardPending} onClick={() => tokenForm(open, undefined, refresh)}>Create token</ui.Button>}>
    <ClipboardNotice refresh={refresh} />
    <SearchField label="Search tokens" value={query()} onInput={setQuery} />
    <ReadState loading={report.loading} error={report.error} hasData={report.latest !== undefined} noun="tokens" />
    <Show when={report.latest}><List><For each={rows()} fallback={<EmptyState icon="lock" title={query() ? "No matching tokens" : "No API tokens yet"} description={query() ? "Try another name or token ID." : "Create your first token to connect an app."} />}>
      {(token) => <Row title={token.name} description={<><code class="exr-truncate" title={token.id}>{token.id}</code><span>Expires {date(token.expiresAt)}</span><span>Last used {date(token.lastUsedAt)}</span></>}>
        <Badge tone={status(token) === "Active" ? "success" : "neutral"}>{status(token)}</Badge><ui.Menu label={`Actions for ${token.name}`} trigger="icon" items={[
          { label: "Rename token", onSelect: () => tokenForm(open, token, refresh) },
          { label: "Rotate token", disabled: state()?.clipboardPending, onSelect: () => confirmAction(open, { title: "Rotate token?", description: `Replace ${token.name}. The old secret stops working immediately. The new secret is copied directly to your clipboard.`, submit: "Rotate and copy", input: { action: "token_rotate", id: token.id, confirmed: true }, danger: true, done: refresh }) },
          { label: "Revoke token", disabled: token.status === "Revoked", onSelect: () => confirmAction(open, { title: "Revoke token?", description: `${token.name} will stop authorizing new requests. This cannot be undone.`, submit: "Revoke token", input: { action: "token_revoke", id: token.id, confirmed: true }, danger: true, done: refresh }) },
        ]} /><ui.Tooltip value="Token details"><ui.IconButton icon="chevron-right" label={`View ${token.name} details`} onClick={() => details(token.id)} /></ui.Tooltip>
      </Row>}
    </For></List></Show>
  </Section></div>
}

export function ModelsView(props: { revision?: string }) {
  const ctx = useHost(), ui = ctx.ui
  const [query, setQuery] = createSignal("")
  const live = createMemo((previous: string) => props.revision ?? previous, "initial")
  const report = createLatest(live, (_, signal) => ctx.main(Dashboard).models({ signal }))
  const rows = createMemo(() => report.latest?.filter((model) => `${model.name} ${model.id}`.toLowerCase().includes(query().trim().toLowerCase())) ?? [])
  return <div class="exr-view"><Section title="Models">
    <SearchField label="Search models" value={query()} onInput={setQuery} />
    <ReadState loading={report.loading} error={report.error} hasData={report.latest !== undefined} noun="models" />
    <Show when={report.latest}><List><For each={rows()} fallback={<EmptyState icon="models" title={query() ? "No matching models" : "No models available"} description={query() ? "Try another model name or ID." : "Refresh the catalog after connecting an account."} />}>
      {(model) => <Row title={model.name || model.id} description={<code class="exr-truncate" title={model.id}>{model.id}</code>}>
        <ui.Tooltip value={`${count(model.context)} token context window`}><span class="exr-model-context">{count(model.context)}<span class="exr-caption">context</span></span></ui.Tooltip><CopyButton value={model.id} label={`Copy ${model.id} ID`} />
      </Row>}
    </For></List></Show>
  </Section></div>
}

export function SettingsView(props: { profile: { readonly latest: typeof Profile.Type | undefined; readonly loading: boolean; readonly error: unknown }; refreshProfile(): void }) {
  const ctx = useHost(), ui = ctx.ui, open = useDialog()
  const changed = () => { props.refreshProfile(); void ctx.main(Dashboard).refresh({ signal: ctx.signal }).catch(() => {}) }
  return <div class="exr-view">
    <ClientUpdates refreshProfile={props.refreshProfile} />
    <Section title="Connection" action={<ui.Button size="small" disabled={!props.profile.latest?.connection} onClick={() => configureConnection(open, props.profile.latest!, changed)}>Configure</ui.Button>}>
      <ReadState loading={props.profile.loading} error={props.profile.error} hasData={!!props.profile.latest} noun="profile" />
      <Show when={props.profile.latest}>{(profile) => <>
        <List><Row title={profile().mode} description={<code class="exr-wrap">{profile().destination}</code>} />
          <For each={profile().fields}>{(field) => <Row title={field.label} description={<span class="exr-wrap">{field.value}</span>} />}</For>
        </List>
        <Show when={profile().mode === "Standalone"}><div class="exr-section-heading exr-subheading"><h4>Local accounts</h4><ui.Button size="small" onClick={() => loginAccount(open, undefined, changed)}>Add account</ui.Button></div><List><For each={profile().accounts} fallback={<Row title="No accounts yet" description="Add an account to get started." />}>{(account) => <Row title={account.email}><Badge>{account.state}</Badge><ui.Menu label={`Actions for ${account.email}`} trigger="icon" items={[
          { label: "Reauthorize account", onSelect: () => loginAccount(open, account, changed) },
          { label: "Disable account", disabled: account.state === "disabled", onSelect: () => confirmAction(open, { title: "Disable account?", description: `New requests will stop using ${account.email}. Existing responses can finish. Reauthorize it to enable it again.`, submit: "Disable account", input: { action: "account_disable", account: account.id, confirmed: true }, danger: true, done: changed }) },
        ]} /></Row>}</For></List><p class="exr-caption">The standalone API runs while this desktop bridge is open. An attached exr serve remains running when it closes.</p></Show>
      </>}</Show>
    </Section>
  </div>
}

function ClientUpdates(props: { refreshProfile(): void }) {
  const ctx = useHost(), ui = ctx.ui, open = useDialog()
  const [checking, setChecking] = createSignal(false), [error, setError] = createSignal(false)
  const state = useDashboardState()
  const update = () => state()?.update
  const check = async () => {
    if (checking()) return
    setChecking(true); setError(false)
    try { await ctx.main(Dashboard).update({ signal: ctx.signal }) }
    catch { setError(true) }
    finally { setChecking(false) }
  }
  return <Section title="Client updates"><div class="exr-update" data-available={update()?.available ? "" : undefined}>
        <List><Row title={<span class="exr-inline-start exr-update-title"><Show when={update()?.available}><ui.Icon name="download" size="small" /></Show>{update()?.available ? "An update is available" : update() ? "You're up to date" : "Not checked"}</span>} description={update() ? `exr ${update()!.current}${update()!.available ? ` → ${update()!.latest}` : ""}` : "No release information available."}>
        <ui.Button busy={checking()} onClick={() => void check()}>Check updates</ui.Button><Show when={update()?.available}><ui.Button variant="contrast" onClick={() => confirmAction(open, { title: "Update exr?", description: `Install ${update()!.latest} using the existing installation method and prefix. Settings, accounts and keys are preserved. Reopen the dashboard after installation.`, submit: "Install update", input: { action: "update_install", confirmed: true }, done: props.refreshProfile })}>Install update</ui.Button></Show></Row></List>
      <Show when={error()}><Notice tone="warning">Update check unavailable or disabled by EXR_NO_UPDATE_CHECK.</Notice></Show>
    </div></Section>
}
