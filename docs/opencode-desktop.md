# OpenCode Desktop integration

The optional [experimental plugin](../clients/opencode-desktop/README.md) adds a
native dashboard panel and dynamic ExetRouter
model discovery. A plugin is **not** required for inference: the existing
[configuration export](compatibility.md#opencode-setup) remains supported.

## Architecture

```text
OpenCode server plugin → authenticated /v1/models/opencode-v2
                      → provider transform → native Responses provider
                                            → ExetRouter HTTP/WS inference

Desktop window → typed IPC → Electron main → exr doctor/usage --json
                                           → exr desktop (private stdio actions)
                                           → standalone / restricted SSH
```

The server entry registers only the ExetRouter integration, provider and a
content-free catalog-status/refresh RPC. It neither edits global configuration
nor intercepts model requests. Account-specific inventory is bound to its
OpenCode connection, refreshed after credential changes and atomically replaced.
A temporary same-connection discovery failure keeps the last inventory marked
stale. A connection change cannot retain the previous inventory.

The desktop main process owns the bounded native-client adapter. It reuses
exr's existing profile, SSH identity/host-key verification and private metadata
handling. It never reads SQLite or OAuth credentials directly. Dashboard reports
are in memory, not persisted to GUI stores. A host-native store saves only the
selected tab, usage period/group and metric, independently of the TUI's encrypted
preferences; it contains no accounts, labels, credentials or report data. The native bridge is IPC-only: it
does not expose account management through the OpenCode HTTP RPC surface.

## Current scope

- Configure the plugin in global `opencode.json(c)`; install/build instructions
  and options are in the package README. The package is source-only, not on npm.
- Save an existing router token through OpenCode's native connection flow, or
  supply `EXETROUTER_TOKEN` to its server. Tokens can be issued explicitly in
  the panel, but are never automatically installed into OpenCode Accounts.
- Select discovered models in the normal picker. Router metadata supplies limits,
  modalities and reasoning variants without inheriting a bundled OpenAI catalog.
- The single panel contains Overview, Usage, Tokens, Models and Settings, with
  native web controls and no separate OpenCode Settings page. The native composer
   quota indicator shows the account actually serving this chat and opens account
   details; it never substitutes the global Best recommendation or token activity.
   The server plugin observes only primary HTTP response headers / native WS
   metadata and publishes a bounded, in-memory per-session projection via RPC.
   A user-scoped HMAC alias matches the authenticated native doctor's account,
   without exposing upstream identities, credentials, content or turn state.
   Account transfers update immediately; auxiliary calls and parallel chats stay
   isolated. Without an observation or a matching updated router/profile, the
   pill is absent. No inference is submitted to discover the account.
  Overview includes router/account status, credits, forecasts and detailed
  routing/client activity. Usage includes the TUI periods/groups and activity
  chart. Tokens exposes metadata and native management actions; Models exposes
  the same catalog and clipboard action as the TUI.
- Views use the host's menus, dialogs, search inputs, buttons, tooltips and
  loading indicators. Shared primitives, shell and views are separate modules.
  Account dialogs follow refreshed snapshots by ID; loaded views retain their
  filters and search when hidden. Dialogs are owned by the stable panel rather
  than refreshed rows, so refresh cannot close an account window or discard an
  action form's draft. Previous results remain visible during background reads,
  including the last reported Best marker and indicator values. Temporary IPC
  gaps do not clear the last snapshot. Cached recommendations are labelled in
  tooltips/details, not by a blinking subtitle; new complete snapshots replace
  them atomically. This is presentation caching, not a routing/eligibility override.
   The active view refreshes when a new snapshot arrives. There are no wide metadata tables or raw
  browser selects in the narrow panel.
- Overview reads cached diagnostics and quota observations. Other views load on
  demand. No diagnostic request submits inference or activates a weekly window.
- The panel's Settings tab shows native-client connection fields/local accounts
  and checks client releases once at startup and on demand, respecting
  `EXR_NO_UPDATE_CHECK`. Connect an API token through OpenCode Accounts. Edit
  plugin connection options in configuration; the native profile can be edited
  in the panel. Local OAuth, deactivation and client update installation use
  the same native implementations as the TUI, with explicit user actions.
- Standalone starts or attaches to the normal embedded API through the native
  desktop bridge. Remote OpenCode
  execution requires its own server-side plugin configuration and credentials.

The bridge requires an updated exr from the same checkout, not an older release
without the `desktop` command. It accepts a closed JSON-line protocol over private
stdin/stdout only: one request at a time, 64 KiB input frames and 1 MiB replies.
Secrets are copied natively after clipboard detection and never cross window IPC;
one failed copy can be retried without reissuing. Uncertain mutations are not
retried automatically. Ordinary CLI terminal and secret-output guards are unchanged.
There is no OS service supervisor, operator administration, personal account
ranking, manual credit redemption or public HTTP management endpoint.

### TUI parity checklist

| TUI surface | Desktop surface |
| --- | --- |
| Overview: server, routing, OAuth, catalog, concurrency | Overview status/statistics |
| Account windows, actual durations, remaining/used, reset/observation/freshness, forecasts | Account groups and subscription details |
| Best/Priority, availability/lock, routing/share/deadline | Account badges and details dialog |
| Credits/count/expiry, automatic reset, weekly activation, cooldown/source, refresh failures | Account groups and subscription details |
| Clients/scope/count, ID/name, running count, last request age/time, truncation | Live account details dialog |
| Usage: four periods, four groups, metric, selected series, chart/counters | Usage filters, chart and breakdown |
| Token ID/name/state/created/expiry/last used | Token rows and details dialog |
| Create with expiry, rename, rotate, revoke, clipboard retry | Token forms, confirmation dialogs and copy-recovery notice |
| Model ID/name/context, reverse catalog order, copy | Searchable Models rows and copy control |
| Remote/standalone setup, API URL, SSH fields, state/listen | Configure connection dialog |
| Local account add, same-account reauth, reversible deactivation | Settings browser sign-in and account actions |
| Native client update check/confirmation/install | Settings update row and confirmation dialog |

Data/action parity does not mean emulating terminal keys or terminal layout.
Native integration tests use synthetic state; real OAuth, update installation
and live inference require separately authorized validation.

## Desktop design references

The panel layout was reviewed against the installed OpenCode Desktop **2.2.5**
renderer sources (including its shipped source maps), not the unrelated CLI
version on PATH. Source references at the matching release tag:

- [Context panel](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/gui-plugins/src/context/tab.tsx):
  scroll ownership, compact label/value statistics, spacing and container layout.
- [SettingsList](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/app/src/settings/list.tsx)
  and [SettingsRow](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/app/src/settings/row.tsx):
  grouped surfaces, row copy/control hierarchy and separators.
- [Settings search](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/app/src/settings/search-field.tsx):
  leading search icon, clear action, focus retention and Escape handling.
- [Tabs](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/ui/src/navigation/tabs/tabs-current.css)
  and [Badge](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/ui/src/data-display/badge/badge.css):
  natural-width navigation, compact segmented filters and tags.
- [Plugin UI adapter](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/app/src/runtime/plugin/ui.tsx):
  SDK controls delegate to the host's actual UI components.
- [Goal renderer](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/gui-plugins/src/goal/renderer.tsx):
  per-server/session RPC snapshot and live-event subscription without remounting.
- [Model request hooks](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/session/model-request.ts)
  and [session transport](https://github.com/anomalyco/opencode/blob/v2.2.5/packages/core/src/session/model-transport.ts):
  primary/auxiliary attribution, HTTP headers and per-exchange WS receive hooks.

`ctx.ui` supplies buttons, icons, inputs, menus, dialogs, tooltips and loaders.
Tabs and settings rows are not exposed by the SDK, so `src/ui.tsx` and
`src/window.css` provide small scoped adaptations of those patterns. They use
the host's `--v2-*` semantic theme tokens, without a private colour palette,
private `@opencode/ui` imports or reliance on lazily loaded Settings styles.
The panel does not duplicate the host's branded tab heading. Dialog content
leaves horizontal padding and scrolling to the native dialog body.
All panel text uses the proportional host font, including IDs and one-time
login codes. Overview status/statistics use compact inline rows. Cooldown appears
over dimmed quota indicators, not as a separate banner.
Dates use local time without a timezone suffix. Activity charts use an inset
container, a vertical hover/keyboard cursor and a value tooltip offset by 24px.
Hourly tooltips show one endpoint timestamp; daily tooltips show the bucket date.
Incomplete/partial-count annotations are omitted; nullable values remain Unknown,
never fabricated zeroes.

This design-source review does not change the pinned experimental SDK or claim
full native Electron compatibility testing on 2.2.5.

## Reviewed contract and verification

Targets OpenCode **2.2.0**, source revision
`1097e2668b1e08ad63bdf0b1c4860b9bc01e420c`. Review covered GUI registries/main
process APIs, provider/integration schema and provider transforms. The SDK is
pinned to that version because the GUI API is experimental. Relevant references:
[GUI plugins](https://opencode.ai/v2/docs/build/plugins/gui/),
[server plugins](https://opencode.ai/v2/docs/build/plugins/server),
[RPC](https://opencode.ai/v2/docs/build/plugins/rpc).

Package checks cover TypeScript, the single CommonJS GUI bundle, synthetic
catalog projection, redirect/body bounds, credential-switch races, cancellation,
passive native command selection, nullable usage and stale quota formatting.
The built bundle is also evaluated with the host's two-process placeholder
model, checking panel-only contributions and a synthetic main-process native read.
DOM tests render all five views against synthetic metadata and exercise tab
navigation, menus, dialogs, search, clipboard feedback and filter retention.
Chart tests cover group identities, duplicate buckets and future observations.
Session-account tests cover primary/auxiliary separation, concurrent chats,
failover, bounded metadata retention and no Best fallback. Offline router tests
verify opt-in HTTP and per-turn WS observations, socket reuse, deactivation
transfer and user-scoped alias matching; no real upstream inference was used.
Forecast tests cover the native algorithm's freshness/reset fences.
The optional native test loads a packed/installed package in an isolated 2.2.0
server against a loopback catalog without generation. It does not prove the
rendered Electron UI, live upstream inference, long sessions or compaction;
the [existing compatibility matrix](compatibility.md) retains its recorded scope.
