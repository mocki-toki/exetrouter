import * as v from "vscode"
import { baseUrl, CatalogError, discover, record, secretKey, type Model } from "./catalog"
import {
  accountRows,
  routerRow,
  statusSummary,
  tokenRows,
  type Row,
} from "./dashboard"
import { Native, periods, groups, profileStamp, type Group, type TokenAction } from "./native"
import { provider } from "./provider"
import { chartDocument, metrics, quotaChart, usageChart, type Chart, type Metric } from "./charts"

const config = () => v.workspace.getConfiguration("exetrouter")
const setting = (key: string) => config().get<string>(key, "").trim()
const areas = ["overview", "tokens", "models"] as const
type Area = (typeof areas)[number]
const unavailable = (label: string): Row[] => [{ label }]
function modelRow(m: Model): Row {
  const detail = [
    m.id,
    `Context: ${m.context?.toLocaleString() ?? "Unknown"}`,
    `Input: ${m.input?.toLocaleString() ?? "Unknown"}`,
    `Output: ${m.output?.toLocaleString() ?? "Unknown"}`,
    `Reasoning: ${m.efforts.join(", ") || "Not reported"}`,
    ...(m.tools ? ["Tools"] : []),
    ...(m.vision ? ["Image input"] : []),
    ...(!m.chat ? ["Not in chat picker"] : []),
  ]
  return {
    label: m.name,
    description: m.route,
    detail: detail.join(" · "),
    children: [
      { label: "Copy model ID", command: "exetrouter.copyModel", args: [m.id] },
      ...detail.map((label) => ({ label })),
    ],
  }
}

export function activate(ctx: v.ExtensionContext) {
  let epoch = 0,
    native: Native | undefined,
    stale = false,
    lastRead = 0,
    accounts: Row[] | undefined,
    router: Row | undefined,
    refreshing: Promise<void> | undefined
  let inventory: { base: string; models: Model[]; at: number } | undefined
  let lastCatalog: typeof inventory
  const modelsChanged = new v.EventEmitter<void>(),
    treeChanged = new Map<Area, v.EventEmitter<void>>()
  const lifetime = new AbortController()
  let tokenSnapshot: { at: number; rows: Row[] } | undefined
  let lastHistory: { key: string; at: number; chart: Chart } | undefined
  const usageViews = new Set<(reset?: boolean) => void>()
  const usageCache = new Map<string, { owner: Native; at: number; report: unknown }>()
  let usagePanel: v.WebviewPanel | undefined
  let historyPanel: v.WebviewPanel | undefined
  const status = v.window.createStatusBarItem(v.StatusBarAlignment.Right, 50)
  status.name = "ExetRouter: All Profile Accounts"
  status.command = "exetrouter.accounts"
  status.show()
  const renderStatus = () => {
    status.text = accounts
      ? `$(organization) EXR ${statusSummary(accounts)}${stale ? " · stale" : ""}`
      : "$(organization) EXR ?"
    status.tooltip = accounts
      ? [
          "All exr profile accounts",
          ...(stale ? [`Stale · last read ${new Date(lastRead).toLocaleString()}`] : []),
          ...accounts.map((a) => `${a.label}: ${a.detail}`),
        ].join("\n")
      : "exr profile accounts unavailable"
  }
  renderStatus()
  const clearProfile = () => {
    native?.dispose()
    native = undefined
    accounts = undefined
    router = undefined
    stale = false
    tokenSnapshot = undefined
    usageCache.clear()
    lastHistory = undefined
    historyPanel?.dispose()
    historyPanel = undefined
    inventory = undefined
    lastCatalog = undefined
    renderStatus()
  }
  const client = () => {
    if (native && native.stamp !== profileStamp()) {
      clearProfile()
      epoch++
      refreshing = undefined
      for (const e of treeChanged.values()) e.fire()
      for (const update of usageViews) update(true)
      modelsChanged.fire()
    }
    return (native ??= new Native(setting("executable") || "exr"))
  }
  const base = async () => {
    const configured = setting("apiBaseUrl")
    if (configured) {
      try { return baseUrl(configured) }
      catch { throw new CatalogError("Set a valid ExetRouter API address ending in /v1.") }
    }
    const p = await client().profile().catch(() => undefined)
    if (!p?.base) throw new CatalogError("API address is missing. Connect an API token to set it.")
    return p.base
  }
  const token = (url: string) => {
    if (native && native.stamp !== profileStamp()) client()
    return ctx.secrets.get(secretKey(url))
  }
  const fetchModels = async (signal: AbortSignal) => {
    const before = epoch,
      url = await base(),
      bearer = await token(url)
    if (before !== epoch || !bearer) return { base: url, models: [] }
    if (inventory?.base === url && Date.now() - inventory.at < 60000) return inventory
    const deadline = AbortSignal.timeout(15000)
    const models = await discover(url, bearer, AbortSignal.any([signal, lifetime.signal, deadline]))
      .catch((error) => {
        if (deadline.aborted && !signal.aborted) throw new CatalogError("The API did not return the catalog within 15 seconds.")
        throw error
      })
    if (before !== epoch) return { base: url, models: [] }
    return (lastCatalog = inventory = { base: url, models, at: Date.now() })
  }
  const refreshAccounts = () => {
    // Resolve identity before capturing the epoch or assigning a pending refresh.
    let n: Native
    try {
      n = client()
    } catch {
      renderStatus()
      return Promise.resolve()
    }
    if (refreshing) return refreshing
    const before = epoch
    refreshing = (async () => {
      try {
        await n.profile().catch(() => undefined)
        const report = await n.report("doctor"),
          rows = accountRows(report)
        if (epoch === before && n === client()) {
          accounts = rows
          router = routerRow(report)
          stale = false
          lastRead = Date.now()
        }
      } catch {
        if (epoch === before && n === client()) stale = accounts !== undefined
      }
      if (epoch === before) {
        renderStatus()
        treeChanged.get("overview")?.fire()
        refreshing = undefined
      }
    })()
    return refreshing
  }
  const refresh = () => {
    epoch++
    inventory = undefined
    refreshing = undefined
    renderStatus()
    for (const e of treeChanged.values()) e.fire()
    for (const update of usageViews) update()
    modelsChanged.fire()
    return refreshAccounts()
  }
  const register = (id: string, fn: (arg?: unknown) => unknown) =>
    ctx.subscriptions.push(v.commands.registerCommand(`exetrouter.${id}`, fn))
  register("connect", async () => {
    let url: string
    try { url = await base() }
    catch {
      const address = await v.window.showInputBox({
        title: "ExetRouter API Address",
        prompt: "Enter the router's API base URL, ending in /v1.",
        value: setting("apiBaseUrl"),
        validateInput: (value) => {
          try { baseUrl(value.trim()); return undefined }
          catch { return "Use an HTTPS URL (or loopback HTTP) ending in /v1." }
        },
      })
      if (!address) return
      url = baseUrl(address.trim())
      await config().update("apiBaseUrl", url, v.ConfigurationTarget.Global)
    }
    const value = await v.window.showInputBox({
      title: `Connect to ${url}`,
      prompt: "Enter an existing ExetRouter API token.",
      password: true,
      ignoreFocusOut: true,
    })
    if (value?.trim()) await ctx.secrets.store(secretKey(url), value.trim())
  })
  register("forget", async () => {
    const url = await base()
    if (
      (await v.window.showWarningMessage(
        `Forget the saved API token for ${url}?`,
        { modal: true },
        "Forget",
      )) === "Forget"
    )
      await ctx.secrets.delete(secretKey(url))
  })
  register("refresh", refresh)
  register("accounts", async () => {
    await refreshAccounts()
    if (!accounts?.length) {
      await v.window.showInformationMessage(
        accounts ? "No profile accounts reported." : "Native exr account report unavailable.",
      )
      return
    }
    await v.window.showQuickPick(
      accounts.map((a) => ({ label: a.label, description: a.description, detail: a.detail })),
      {
        title: stale
          ? `All exr profile accounts · Stale since ${new Date(lastRead).toLocaleString()}`
          : "All exr profile accounts",
        matchOnDetail: true,
      },
    )
  })
  register("copyModel", async (id) => {
    const { models } = await fetchModels(lifetime.signal)
    const selected =
      models.find((m) => m.id === id) ??
      (await v.window.showQuickPick(
        models.map((m) => ({ label: m.name, description: m.id, detail: modelRow(m).detail, id: m.id })),
        { title: "Search models · select to copy ID", matchOnDescription: true, matchOnDetail: true },
      ))
    if (selected) await v.env.clipboard.writeText(selected.id)
  })
  const attachChart = (
    view: v.WebviewView | v.WebviewPanel,
    kind: Chart["kind"], title: string,
    load: (selection: { period: (typeof periods)[number]; group: Group; metric: Metric }, refresh: boolean) => Promise<{ chart?: Chart; notice?: string }>,
  ) => {
    const webview = view.webview
    const assets = v.Uri.joinPath(ctx.extensionUri, "assets")
    webview.options = { enableScripts: true, localResourceRoots: [assets] }
    let sequence = 0, disposed = false, active = false
    let selection = { period: "7d" as (typeof periods)[number], group: "model" as Group, metric: "tokens" as Metric }
    const reload = async (fresh = true) => {
      if (!active || disposed) return
      const request = ++sequence
      void webview.postMessage({ type: "chart", title, notice: "Loading…", loading: true })
      const next = await load(selection, fresh).catch(() => ({ notice: "Native report unavailable." }))
      if (!disposed && sequence === request) void webview.postMessage({ type: "chart", title, ...next })
    }
    const listener = webview.onDidReceiveMessage((value: unknown) => {
      const m = record(value)
      if (m.type !== "chart") return
      const period = periods.find((p) => p === m.period), group = groups.find((g) => g === m.group)
      const metric = metrics.find((m1) => m1 === m.metric)
      if (!period || (kind === "usage" && (!group || !metric))) return
      active = true
      selection = { period, group: group ?? "model", metric: metric ?? "tokens" }
      void reload(m.refresh === true)
    })
    const update = (reset = false) => {
      if (reset && active && !disposed) void webview.postMessage({ type: "chart", title, notice: "Profile changed." })
      void reload()
    }
    if (kind === "usage") usageViews.add(update)
    view.onDidDispose(() => { disposed = true; usageViews.delete(update); listener.dispose() })
    webview.html = chartDocument(webview, v.Uri.joinPath(assets, "charts.js"), v.Uri.joinPath(assets, "charts.css"), kind)
  }
  const loadUsage: Parameters<typeof attachChart>[3] = async ({ period, group, metric }, refresh) => {
    const n = client(), key = `${period}:${group}`, cached = usageCache.get(key)
    const old = cached?.owner === n ? cached : undefined
    if (!refresh && old && Date.now() - old.at < 60000) return { chart: usageChart(old.report, metric) }
    try {
      const report = await n.report("usage", period, group)
      if (n !== client()) return {}
      const chart = usageChart(report, metric)
      if (usageCache.size >= 16 && !usageCache.has(key)) usageCache.delete(usageCache.keys().next().value!)
      usageCache.set(key, { owner: n, at: Date.now(), report })
      return { chart }
    } catch {
      if (n !== client() || !old) return { notice: "Usage report unavailable." }
      return { chart: usageChart(old.report, metric), notice: `Stale report · ${new Date(old.at).toLocaleString()}` }
    }
  }
  ctx.subscriptions.push(v.window.registerWebviewViewProvider("exetrouter.usage", {
    resolveWebviewView(view) { attachChart(view, "usage", "Usage", loadUsage) },
  }, { webviewOptions: { retainContextWhenHidden: true } }))
  register("openUsage", () => {
    if (usagePanel) return usagePanel.reveal()
    const panel = v.window.createWebviewPanel("exetrouter.usageChart", "ExetRouter Usage", v.ViewColumn.Active, { retainContextWhenHidden: true })
    usagePanel = panel
    panel.onDidDispose(() => { if (usagePanel === panel) usagePanel = undefined })
    attachChart(panel, "usage", "Usage", loadUsage)
  })
  register("history", async () => {
    await refreshAccounts()
    const n = client(),
      choices = (accounts ?? []).filter((a) => a.accountId)
    if (!choices.length) {
      await v.window.showInformationMessage("No profile accounts with quota history are available.")
      return
    }
    const a = await v.window.showQuickPick(choices, {
      title: `Select an account for quota history${stale ? " · stale account list" : ""}`,
    })
    if (!a?.accountId) return
    if (n !== client()) return
    const account = a.accountId
    historyPanel?.dispose()
    const panel = v.window.createWebviewPanel("exetrouter.quotaChart", "ExetRouter Quota History", v.ViewColumn.Active, { retainContextWhenHidden: true })
    historyPanel = panel
    panel.onDidDispose(() => { if (historyPanel === panel) historyPanel = undefined })
    attachChart(panel, "quota", a.label, async ({ period }) => {
      const key = `${account}:${period}`
      try {
        const chart = quotaChart(await n.history(account, period))
        if (n !== client()) return {}
        lastHistory = { key, chart, at: Date.now() }
        return { chart }
      } catch {
        if (n !== client() || lastHistory?.key !== key) return { notice: "Quota history unavailable." }
        return { chart: lastHistory.chart, notice: `Stale report · ${new Date(lastHistory.at).toLocaleString()}` }
      }
    })
  })
  let managing = false
  const tokenAction = async (entry: "create" | "existing" | "clipboard", id?: unknown) => {
    if (managing) return
    managing = true
    try {
      const n = client(),
        profile = await n.profile(),
        target = profile.destination ?? profile.base ?? profile.mode
      const clipboard = entry === "clipboard" || (entry === "create" && n.canRetryCopy)
      if (clipboard && !n.canRetryCopy) return
      let action: TokenAction | undefined,
        confirm = "",
        subject = ""
      const namePrompt = async (value = "") =>
        v.window.showInputBox({
          title: "Token name",
          value,
          ignoreFocusOut: true,
          validateInput: (name) =>
            name.trim() && Buffer.byteLength(name.trim()) <= 80 && !/\p{Cc}/u.test(name)
              ? undefined
              : "Enter 1–80 bytes without control characters.",
        })
      if (clipboard) {
        const choice = await v.window.showQuickPick(["Retry clipboard copy", "Discard uncopied secret"], { title: `Pending Clipboard Copy · ${target}` })
        if (!choice) return
        const retry = choice === "Retry clipboard copy"
        action = { action: retry ? "copy_retry" : "copy_discard" }
        confirm = retry ? "Retry copy" : "Discard"
        subject = retry ? "Copy the previously issued secret again" : "Discard the uncopied secret permanently"
      } else if (entry === "create") {
        const name = (await namePrompt())?.trim()
        if (!name) return
        const d = await v.window.showInputBox({
          title: "Expires in days",
          value: "90",
          validateInput: (d) => (/^\d+$/.test(d) && +d >= 1 && +d <= 365 ? undefined : "Choose 1–365 days."),
        })
        if (!d) return
        action = { action: "token_create", name, days: Number(d) }
        confirm = "Create and copy"
        subject = `Create “${name}” for ${d} days and copy its secret`
      } else {
        if (typeof id !== "string") return
        const t = tokenRows(await n.report("tokens")).find((t) => t.tokenId === id)
        if (!t?.tokenId) return
        const op = await v.window.showQuickPick(
          t.tokenState === "Revoked"
            ? ["Rename"]
            : [
                "Rename",
                ...(t.tokenState === "Disabled" ? ["Resume", "Fast Mode…"] : t.tokenState === "Active" ? ["Pause", "Fast Mode…"] : []),
                ...(n.canRetryCopy ? [] : ["Rotate"]),
                "Revoke",
              ],
          { title: `${t.label} · ${t.tokenId}` },
        )
        if (!op) return
        confirm = op
        subject = `${op} “${t.label}” (${t.tokenId})`
        if (op === "Rename") {
          const name = (await namePrompt(t.label))?.trim()
          if (!name) return
          action = { action: "token_rename", id: t.tokenId, name }
          subject += ` to “${name}”`
        } else if (op === "Pause" || op === "Resume")
          action = { action: "token_set_enabled", id: t.tokenId, enabled: op === "Resume", confirmed: true }
        else if (op === "Fast Mode…") {
          const policies = [
            { label: "Default", detail: "Inherit the server policy", value: null },
            { label: "Allow", detail: "Allow priority requests from this token", value: true },
            { label: "Block", detail: "Strip priority from requests from this token", value: false },
          ].map((policy) => ({ ...policy, description: policy.value === t.fastMode ? "Current" : undefined }))
          const policy = await v.window.showQuickPick(policies, { title: `Fast Mode · ${t.label}` })
          if (!policy || policy.value === t.fastMode) return
          action = { action: "token_fast_mode", id: t.tokenId, enabled: policy.value, confirmed: true }
          confirm = "Set Fast Mode"
          subject = `Set Fast Mode for “${t.label}” to ${policy.label}`
        }
        else if (op === "Rotate") {
          action = { action: "token_rotate", id: t.tokenId, confirmed: true }
          subject += " and invalidate its old secret"
        } else {
          action = { action: "token_revoke", id: t.tokenId, confirmed: true }
          subject += " permanently"
        }
      }
      if (
        (await v.window.showWarningMessage(
          `${subject}?`,
          { modal: true, detail: `Native exr profile: ${target}` },
          confirm,
        )) !== confirm ||
        n !== client()
      )
        return
      const result = await n.action(profile, action)
      if (result === "copy_failed")
        void v.window.showWarningMessage(
          "The secret was not copied. Use the pending clipboard action in Tokens to retry the same copy.",
        )
      else if (result === "clipboard_unavailable")
        void v.window.showWarningMessage("Native clipboard unavailable. No token was issued or rotated.")
      else
        void v.window.showInformationMessage(
          result === "copied" ? "Secret copied by exr." : "Token action completed.",
        )
    } finally {
      managing = false
      treeChanged.get("tokens")?.fire()
    }
  }
  register("createToken", () => tokenAction("create"))
  register("manageToken", (id) => tokenAction("existing", id))
  register("tokenClipboard", () => tokenAction("clipboard"))
  const rows = async (area: Area): Promise<Row[]> => {
    try {
      if (area === "overview")
        return [
          ...(router ? [router] : []),
          ...(stale
            ? [{ label: "Stale account report", description: new Date(lastRead).toLocaleString() }]
            : []),
          ...(accounts?.length
            ? accounts
            : unavailable(accounts ? "No profile accounts reported" : "Profile accounts unavailable")),
        ]
      if (area === "models") {
        try {
          const url = await base()
          if (!(await token(url))) return [{ label: "Connect API token", description: "No token saved for this API address", command: "exetrouter.connect" }]
          const { models } = await fetchModels(lifetime.signal)
          return models.length ? models.map(modelRow) : unavailable("The API returned no available models")
        } catch (error) {
          const failure = error instanceof CatalogError ? error : undefined
          const denied = failure?.status === 401 || failure?.status === 403
          const url = await base().catch(() => undefined)
          const cached = !denied && url && lastCatalog?.base === url ? lastCatalog : undefined
          if (denied) inventory = lastCatalog = undefined
          return [
            { label: failure?.message ?? "Could not read the API model catalog.", command: denied || !url ? "exetrouter.connect" : "exetrouter.refresh" },
            ...(cached ? [{ label: "Stale catalog", description: new Date(cached.at).toLocaleString() }, ...cached.models.map(modelRow)] : []),
          ]
        }
      }
      const n = client()
      await n.profile().catch(() => undefined)
      let result: Row[]
      try {
        const data = await n.report("tokens")
        if (n !== client()) return []
        result = tokenRows(data)
        tokenSnapshot = { at: Date.now(), rows: result }
      } catch {
        if (n !== client()) return []
        const old = tokenSnapshot
        result = old
          ? [{ label: "Stale report", description: new Date(old.at).toLocaleString() }, ...old.rows]
          : unavailable("Native report unavailable")
      }
      return [
        ...(n.canRetryCopy ? [{ label: "Pending clipboard copy…", command: "exetrouter.tokenClipboard" }] : []),
        ...(result.length ? result : unavailable("No profile tokens reported")),
      ]
    } catch {
      return unavailable("Native report unavailable")
    }
  }
  for (const area of areas) {
    const changed = new v.EventEmitter<void>()
    treeChanged.set(area, changed)
    ctx.subscriptions.push(
      changed,
      v.window.registerTreeDataProvider(`exetrouter.${area}`, {
        onDidChangeTreeData: changed.event,
        async getChildren(row?: Row) {
          const before = epoch
          if (row) return row.children ?? []
          const data = await rows(area)
          return epoch === before ? data : []
        },
        getTreeItem(row: Row) {
          const item = new v.TreeItem(
            row.label,
            row.children?.length ? v.TreeItemCollapsibleState.Collapsed : v.TreeItemCollapsibleState.None,
          )
          item.description = row.description
          item.tooltip = row.detail ?? row.description
          if (row.command)
            item.command = {
              command: row.command,
              title: row.label,
              arguments: row.args,
            }
          return item
        },
      }),
    )
  }
  const timer = setInterval(() => {
    void refreshAccounts()
  }, 60000)
  ctx.subscriptions.push(
    status,
    modelsChanged,
    v.lm.registerLanguageModelChatProvider(
      "exetrouter",
      provider(
        {
          get epoch() {
            return epoch
          },
          catalog: fetchModels,
          token,
          get effort() {
            return setting("reasoningEffort")
          },
        },
        modelsChanged.event,
      ),
    ),
    ctx.secrets.onDidChange((e) => {
      if (e.key.startsWith("api-token:")) {
        lastCatalog = undefined
        void refresh()
      }
    }),
    v.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("exetrouter")) {
        lastCatalog = undefined
        if (e.affectsConfiguration("exetrouter.executable")) {
          clearProfile()
          for (const update of usageViews) update(true)
        }
        void refresh()
      }
    }),
    {
      dispose() {
        epoch++
        clearInterval(timer)
        lifetime.abort()
        historyPanel?.dispose()
        usagePanel?.dispose()
        native?.dispose()
      },
    },
  )
  void refreshAccounts()
}
