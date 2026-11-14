# Subscription billing metadata

Schema 17 adds operator-owned billing metadata, independent of OAuth token
expiry, short/weekly quota resets and reset-credit expiry. The expected billing
day does not prove that a payment succeeded or that an API credit grant exists.

The server operator configures account IDs from `exrd admin oauth list`:

```sh
exrd admin oauth subscription 1 --billing-day 8 --plan-usd 100
exrd admin oauth subscription 1 --api-budget-usd 125.50
exrd admin oauth subscription 1 --clear-api-budget
```

The standalone owner uses the same implementation and arguments:

```sh
exr account subscription 1 --billing-day 8 --plan-usd 100
```

Billing days are 1–31. An omitted field preserves its previous value. An explicit
API budget override is a nonnegative USD amount with at most two decimals; zero
is distinct from unknown. Recognized Pro price defaults are $100 and $200. A $20
subscription price does not imply an API allowance. An allowance is configuration,
not observed remaining credit, and never enables Platform inference by itself.

Doctor/Limits expose `quota_accounts[].subscription` for both server and
standalone. GUI, TUI and ordinary CLI display the same billing metadata. Missing
metadata remains absent/unknown. Only the operator or standalone owner can edit
it; restricted SSH management has no billing mutation or credential input action.

## Platform credentials

The operator can store a **replacement** Admin Key from a mode-600 file owned by
the current user. Never pass the secret as a command argument or paste it into
source/configuration. Keys disclosed in chat must be revoked, not provisioned.

```sh
exrd admin oauth platform set-admin-key 1 --key-file /private/path/admin-input
exrd admin oauth platform status 1
exrd admin oauth platform remove-admin-key 1
# The standalone owner uses the same implementation:
exr account platform set-admin-key 1 --key-file /private/path/admin-input
```

Provisioning only encrypts the key locally; it does not contact OpenAI or verify
the key, organization ownership or available balance. Doctor/Limits return an optional
`quota_accounts[].platform` status without credentials. Disconnected accounts have
no Platform UI. An Admin Key alone does not enable API inference or establish a
balance. Working-key readiness and optional manual-balance estimates are reported
separately under the [Platform contract](platform-api.md). Removing local
credentials does not revoke upstream keys.
Backup verification authenticates these encrypted secrets with the matching
credential key. Restricted SSH clients cannot provision or remove them.

## Migration and rollback

Stop workers and take a verified private snapshot before the schema 16 → 17
upgrade. The additive migration creates `account_subscriptions` and
`account_platform_credentials`, keyed by the existing account ID, without changing OAuth ciphertext/generation, token signing
keys, quota history, affinity, usage, account policy or reset ledgers. Existing
accounts initially have no billing metadata; there is no inferred backfill.

For reviewed rollback, stop all workers, preserve a current private snapshot and
set `PRAGMA user_version=16` in a maintenance transaction. **Retain both new tables**
and all current account/token/usage state; schema-16 binaries ignore the additive
tables. Restore matching schema-16 binaries/native gateway, then check health
without inference. Re-upgrade preserves the retained billing metadata. Never
overwrite current credentials/accounting with an old snapshot just to downgrade.

The approved HTML mockups are design references. Their fabricated balances,
Platform history and simulated key/action flows are not production observations.
Schema 18 adds [real daily costs and managed thirty-day working keys](platform-api.md).
Schema 19 adds reviewed Platform catalog exposure and inference; schema 20 adds
manual balance reconciliation and independent remaining-funds estimates.
Operator-only `platform models ID` inspects availability with the working key
without inference.

The public [Costs API](https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/usage/methods/costs)
reports daily monetary costs, not the remaining free-credit grant. The public
[service-account creation API](https://developers.openai.com/api/reference/resources/organization/subresources/projects/subresources/service_accounts/methods/create)
supports expiring initial keys, but existing key lists contain only redacted
values: an existing key cannot be recovered from its name. Platform funds may be
free or purchased; there is no free-only admission requirement or local budget cap.
Lifecycle mutations use a durable uncertain-outcome fence and explicit project configuration.

The [subscription-credit benefit](https://developers.openai.com/api/docs/guides/chatgpt-api-credits)
explicitly continues onto purchased credits after free credits run out, and its
first grant can be prorated. [Hard spend limits](https://developers.openai.com/api/docs/guides/spend-limits)
have delayed enforcement. Display allowances and daily costs are not a remaining
balance and never authorize inference by themselves.
