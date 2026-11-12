import { Ipc } from "@opencode/plugin/gui"
import { DashboardState, Usage, UsageInput, Tokens, Models, Profile, Update, DesktopAction, DesktopResult, QuotaHistory, HistoryInput } from "./schemas"

export const Dashboard = Ipc.define({
  id: "exetrouter.dashboard",
  state: DashboardState,
  methods: {
    refresh: {},
    usage: { input: UsageInput, output: Usage },
    quotaHistory: { input: HistoryInput, output: QuotaHistory },
    tokens: { output: Tokens },
    models: { output: Models },
    profile: { output: Profile },
    update: { output: Update },
    desktop: { input: DesktopAction, output: DesktopResult },
  },
})
