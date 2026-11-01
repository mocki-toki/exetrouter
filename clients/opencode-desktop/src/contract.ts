import { Ipc } from "@opencode/plugin/gui"
import { DashboardState, Usage, UsageInput, Tokens, Models, Profile, Update, DesktopAction, DesktopResult } from "./schemas"

export const Dashboard = Ipc.define({
  id: "exetrouter.dashboard",
  state: DashboardState,
  methods: {
    refresh: {},
    usage: { input: UsageInput, output: Usage },
    tokens: { output: Tokens },
    models: { output: Models },
    profile: { output: Profile },
    update: { output: Update },
    desktop: { input: DesktopAction, output: DesktopResult },
  },
})
