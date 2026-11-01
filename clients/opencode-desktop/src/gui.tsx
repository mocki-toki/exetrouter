import { Plugin } from "@opencode/plugin/gui"
import { main } from "./main"
import { setup } from "./window"
import { definition } from "./definition"

export default Plugin.define({ ...definition, setup, main })
