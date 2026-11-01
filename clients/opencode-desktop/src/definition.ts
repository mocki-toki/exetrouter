import { Plugin, Store } from "@opencode/plugin/gui"
import { Schema } from "effect"

export const definition = Plugin.define({
  os: ["macos", "linux"],
  stores: {
    preferences: Store.global(Schema.Struct({
      tab: Schema.Literals(["Overview", "Usage", "Tokens", "Models", "Settings"]),
      period: Schema.Literals(["day", "24h", "week", "month"]),
      by: Schema.Literals(["total", "user", "model", "token"]),
      metric: Schema.Literals(["tokens", "requests"]),
    }), { tab: "Overview", period: "day", by: "total", metric: "tokens" }),
  },
})
