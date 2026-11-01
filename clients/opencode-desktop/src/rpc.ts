import { Rpc } from "@opencode/plugin/rpc"
import { Schema } from "effect"
import { CatalogStatus, SessionAccount } from "./schemas"

export const ExetRouter = Rpc.define({
  id: "exetrouter.catalog",
  methods: {
    status: { input: Schema.toStandardSchemaV1(Schema.Struct({})), output: Schema.toStandardSchemaV1(CatalogStatus) },
    refresh: { input: Schema.toStandardSchemaV1(Schema.Struct({})), output: Schema.toStandardSchemaV1(CatalogStatus) },
  },
  events: {},
})

export const Accounts = Rpc.define({
  id: "exetrouter.accounts",
  methods: {
    get: { input: Schema.toStandardSchemaV1(Schema.Struct({ sessionID: Schema.String })), output: Schema.toStandardSchemaV1(Schema.NullOr(SessionAccount)) },
  },
  events: {
    changed: { schema: Schema.toStandardSchemaV1(Schema.Struct({ sessionID: Schema.String, account: Schema.NullOr(SessionAccount) })) },
  },
})
