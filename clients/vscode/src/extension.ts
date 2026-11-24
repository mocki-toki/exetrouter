import * as v from "vscode"
import { baseUrl, discover, secretKey, type Model } from "./catalog"
import {
  accountRows,
  historyRows,
  routerRow,
  statusSummary,
  tokenRows,
  usageRows,
  type Row,
} from "./dashboard"
import { Native, periods, groups, profileStamp, type Group, type TokenAction } from "./native"
import { provider } from "./provider"

const config = () => v.workspace.getConfiguration("exetrouter")
const setting = (key: string) => config().get<string>(key, "").trim()
const areas = ["overview", "usage", "tokens", "models", "settings"] as const
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
    period = "7d",
    group: Group = "model",
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
  const snapshots = new Map<string, { at: number; rows: Row[] }>()
  let lastHistory: { key: string; at: number; rows: Row[] } | undefined
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
    snapshots.clear()
    lastHistory = undefined
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
      modelsChanged.fire()
    }
    return (native ??= new Native(setting("executable") || "exr"))
  }
  const base = async () => {
    const configured = setting("apiBaseUrl")
    if (configured) return baseUrl(configured)
    const p = await client().profile()
    if (!p.base) throw new Error("Set ExetRouter: API Base URL in user settings.")
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
    const models = await discover(
      url,
      bearer,
      AbortSignal.any([signal, lifetime.signal, AbortSignal.timeout(15000)]),
    )
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
    modelsChanged.fire()
    return refreshAccounts()
  }
  const register = (id: string, fn: (arg?: unknown) => unknown) =>
    ctx.subscriptions.push(v.commands.registerCommand(`exetrouter.${id}`, fn))
  register("connect", async () => {
    const url = await base()
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
  register("period", async () => {
    const p = await v.window.showQuickPick(["24h", "7d", "30d", "90d"], { title: "Usage period" })
    if (p) {
      period = p
      treeChanged.get("usage")?.fire()
    }
  })
  register("group", async () => {
    const choice = await v.window.showQuickPick([...groups], { title: "Group usage by" })
    if (choice) {
      group = choice as Group
      treeChanged.get("usage")?.fire()
    }
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
    const p = await v.window.showQuickPick([...periods], { title: `${a.label} · History period` })
    if (!p || n !== client()) return
    const key = `${a.accountId}:${p}`
    let list: Row[],
      suffix = ""
    try {
      list = historyRows(await n.history(a.accountId, p))
      if (n !== client()) return
      lastHistory = { key, rows: list, at: Date.now() }
    } catch {
      if (n !== client()) return
      if (lastHistory?.key !== key) throw new Error("Quota history unavailable.")
      list = lastHistory.rows
      suffix = ` · Stale (${new Date(lastHistory.at).toLocaleString()})`
    }
    if (!list.length) {
      await v.window.showInformationMessage("No quota observations in this period.")
      return
    }
    await v.window.showQuickPick(list, {
      title: `${a.label} · Quota history · ${p} · Newest first${suffix}`,
      matchOnDescription: true,
      matchOnDetail: true,
    })
  })
  let managing = false
  register("manageToken", async (id) => {
    if (managing) return
    managing = true
    try {
      const n = client(),
        profile = await n.profile(),
        target = profile.destination ?? profile.base ?? profile.mode
      const choices = n.canRetryCopy
        ? ["Retry clipboard copy", "Discard uncopied secret"]
        : ["Create token", "Manage existing token"]
      const first =
        typeof id === "string" && !n.canRetryCopy
          ? "Manage existing token"
          : await v.window.showQuickPick(choices, { title: `Profile tokens · ${target}` })
      if (!first) return
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
      if (first === "Retry clipboard copy") {
        action = { action: "copy_retry" }
        confirm = "Retry copy"
        subject = "Copy the previously issued secret again"
      } else if (first === "Discard uncopied secret") {
        action = { action: "copy_discard" }
        confirm = "Discard"
        subject = "Discard the uncopied secret permanently"
      } else if (first === "Create token") {
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
        const entries = tokenRows(await n.report("tokens")).filter((r) => r.tokenId)
        const t =
          typeof id === "string"
            ? entries.find((t) => t.tokenId === id)
            : await v.window.showQuickPick(entries, {
                title: `Choose a token · ${target}`,
                matchOnDetail: true,
              })
        if (!t?.tokenId) return
        const op = await v.window.showQuickPick(
          t.description === "Revoked"
            ? ["Rename"]
            : [
                "Rename",
                ...(t.description === "Disabled" ? ["Resume"] : t.description === "Active" ? ["Pause"] : []),
                "Rotate",
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
          "The secret was not copied. Use Manage Profile Tokens → Retry clipboard copy. Do not issue another token.",
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
  })
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
      if (area === "settings") {
        const p = await client()
            .profile()
            .catch(() => undefined),
          url = await base().catch(() => undefined)
        return [
          {
            label: "Native profile",
            description: p ? `${p.mode} · exr ${p.version ?? "Unknown"}` : "Unavailable",
          },
          { label: "Profile API", description: p?.base ?? "Not reported" },
          { label: "Chat API", description: url ?? "Not configured" },
          {
            label: "Saved API token",
            description: url && (await token(url)) ? "Connected" : "Not saved",
            command: "exetrouter.connect",
          },
          { label: "Open extension settings", command: "workbench.action.openSettings" },
        ]
      }
      if (area === "models") {
        let models: Model[]
        try {
          models = (await fetchModels(lifetime.signal)).models
        } catch {
          const url = await base(),
            saved = await token(url)
          if (!saved || !lastCatalog || lastCatalog.base !== url) throw new Error("Catalog unavailable.")
          return [
            { label: "Stale catalog", description: new Date(lastCatalog.at).toLocaleString() },
            ...lastCatalog.models.map(modelRow),
          ]
        }
        return models.length
          ? models.map(modelRow)
          : [{ label: "No models · Connect API token", command: "exetrouter.connect" }]
      }
      const n = client()
      await n.profile().catch(() => undefined)
      const selectedPeriod = period,
        selectedGroup = group,
        key = area === "tokens" ? area : `${area}:${selectedPeriod}:${selectedGroup}`
      let result: Row[]
      try {
        const data = await n.report(area, selectedPeriod, selectedGroup)
        if (n !== client()) return []
        result = area === "tokens" ? tokenRows(data) : usageRows(data)
        snapshots.set(key, { at: Date.now(), rows: result })
      } catch {
        if (n !== client()) return []
        const old = snapshots.get(key)
        result = old
          ? [{ label: "Stale report", description: new Date(old.at).toLocaleString() }, ...old.rows]
          : unavailable("Native report unavailable")
      }
      return area === "usage"
        ? [
            { label: "Usage period", description: selectedPeriod, command: "exetrouter.period" },
            { label: "Group by", description: selectedGroup, command: "exetrouter.group" },
            ...result,
          ]
        : [
            {
              label: n.canRetryCopy ? "Resolve pending clipboard copy" : "Manage profile tokens",
              command: "exetrouter.manageToken",
            },
            ...(result.length ? result : unavailable("No profile tokens reported")),
          ]
    } catch {
      return unavailable(
        area === "models"
          ? "Model catalog unavailable · Check connection and refresh"
          : "Native report unavailable",
      )
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
              arguments:
                row.command === "workbench.action.openSettings" ? ["@ext:exetrouter.exetrouter"] : row.args,
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
        }
        void refresh()
      }
    }),
    {
      dispose() {
        epoch++
        clearInterval(timer)
        lifetime.abort()
        native?.dispose()
      },
    },
  )
  void refreshAccounts()
}
