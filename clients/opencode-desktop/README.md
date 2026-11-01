# ExetRouter for OpenCode Desktop (experimental)

A local OpenCode V2 plugin with a dynamically discovered **ExetRouter** provider,
a single native-web dashboard panel. Targets the
OpenCode **2.2.0** plugin contract; GUI APIs are experimental. This package is not
published to npm. See [the integration guide](../../docs/opencode-desktop.md).

## Build

Requires Node.js 22.12+ and npm. From this directory:

```sh
npm ci --ignore-scripts
npm run typecheck
npm test
npm run build
```

Native actions require an **updated exr built from the same checkout**, including
the private `desktop` bridge. Build it with `cargo build --locked --bin exr` at
the repository root and point `exrBinary` at that binary during development.
An older installed release can provide passive reports but cannot run the new
desktop actions. Do not replace an installed binary or profile just to test UI.

Configure the local package directory in the **global** `opencode.json(c)`.
Merge the entry into existing plugins; do not replace unrelated settings:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": [
    {
      "package": "/absolute/path/to/exetrouter/clients/opencode-desktop",
      "options": {
        "baseURL": "https://api.example.com/v1",
        "exrBinary": "/absolute/path/to/exr"
      }
    }
  ]
}
```

Use OpenCode's account connection UI to save an existing router bearer in
OpenCode's native credential store.
`EXETROUTER_TOKEN` in the **OpenCode server's** environment is an alternative.
Do not put a token in plugin options, configuration, command arguments or logs.
This plugin never issues/rotates a token automatically.

Select an `exetrouter/<model>` through the normal model picker. No default model
is forcibly selected. **Open ExetRouter** opens the panel in the current session.
The side panel's **+** menu also offers ExetRouter. A native quota indicator above
the composer shows the short/weekly windows of the account actually used by this
chat; click it for live account details. It follows primary HTTP/WS response
observations, including account transfers, not the global Best recommendation or
token activity. Title/compaction/auxiliary requests and other chats do not change it.
This requires router/native `exr` version 0.8.3 or later and a matching
router user in the native profile and OpenCode connection. Until an attributable
primary response is observed (or on older servers), no account pill is shown.
Observations are bounded in memory only; loading the panel never submits inference.
There is no separate ExetRouter page in OpenCode Settings.

The panel follows the TUI's five views with a compact, theme-aware web layout.
It uses OpenCode's buttons, icon buttons, menus, search fields, loaders,
tooltips and dialogs. Shared layout components live in `src/ui.tsx`; the shell,
overview, usage and resource views are separate modules. Tabs and quota bars
are accessible custom components because the host SDK does not provide them.
No separate UI framework or terminal renderer is bundled.

- **Overview:** router status, OAuth/catalog/concurrency, account availability,
  Best/Priority markers, reset credits/expiry, automatic reset and weekly
  activation status, reported quota windows/reset times, cooldown and bounded
  session-local forecasts. Open account details for routing/target share/deadline
  and server-wide client activity (own names and other users' aliases) in a
  live-updating details dialog.
- **Usage:** the same four periods, total/user/model/token grouping, requests or
  reported-token activity, series selection and cached/nullable breakdown cards.
  The chart has its own inset container; hover/keyboard values sit away from its
  cursor and use one hourly timestamp or one daily date.
  Large grouped reports select one series rather than rendering unbounded lines.
- **Tokens:** searchable decrypted token labels and metadata dialogs: ID, status,
  creation, expiry and last use; create, rename, rotate and revoke actions.
  Issuance copies secrets natively. A failed copy retains one secret for retry,
  without issuing another token or returning it to the window.
- **Models:** searchable catalog in reverse order, model ID, display name,
  context and Copy ID with clipboard feedback.
- **Settings:** configure remote/standalone connection fields; add, reauthorize
  and disable local accounts; check native client releases and explicitly install updates.
  This is a tab inside the panel, not a host settings contribution. Profile
  refresh does not check releases or start OAuth.
  Like the TUI, one passive release check runs at startup unless
  `EXR_NO_UPDATE_CHECK` is set; the Settings button checks again on demand.

Views load lazily and retain filters/search/scroll when switching tabs. The
selected tab and usage period/group/metric persist through the host's native
plugin store; no account/report data or secrets are stored there. The
header and navigation stay fixed while the active view scrolls. Secondary
explanations live in tooltips and dialogs, not a permanent controls footer.

Background refresh keeps the complete last snapshot visible, including its Best
marker and quota indicator. Temporary IPC gaps and local-clock expiry do not
clear them; stale values are identified as cached in their tooltips/details.
A newly received snapshot replaces the recommendation atomically, including
actual deactivation or loss of eligibility. It applies replies in place and
does not remount the panel or close dialogs. Dialogs belong to the stable panel,
not replaceable result rows; drafts, filters and scroll survive a refresh.
Native actions use the same Rust implementation as the TUI, through private
stdio IPC rather than a terminal automation workaround.
One shared refresh button updates the snapshot and active view, including when
the server returns the same second-resolution timestamp. Update availability is
highlighted in the first Settings section.
Passive snapshots use doctor rather than the TUI's live limits request, so
freshness and unavailable metadata are labeled instead of activating a week.

When moving from a static export, remove only the redundant
`providers.exetrouter` definition after reviewing it; its explicit model overrides
can otherwise reintroduce models absent from discovery. Preserve other providers
and settings. The plugin does not remove or rewrite configuration for you.

## Options

| Option | Default | Purpose |
| --- | --- | --- |
| `baseURL` | `http://127.0.0.1:8787/v1` | Router HTTP API; remote endpoints require HTTPS. |
| `transport` | `websocket` | Native `websocket` or `http` transport. |
| `refreshSeconds` | `60` | Catalog refresh interval, 30–3600 seconds. |
| `exrBinary` | `exr` | Executable or path; an absolute path is recommended in Desktop. |
| `exrConfig` | Native exr default | Optional absolute or `~/` path to an existing exr profile. |

The dashboard uses the saved native exr profile, including SSH host-key checks
and private-label decryption. Configure that profile in the panel or exr. The HTTP
endpoint and SSH profile are independent; the plugin cannot prove they refer to
the same deployment. Settings displays them separately. Changing plugin options
reloads the plugin; no OpenCode configuration is rewritten by the plugin.

For standalone, the desktop bridge starts or attaches to the normal local API.
It owns an API it starts until the bridge closes; an existing `exr serve` stays
running. This is not an OS service installer or supervisor.

## Safety and limits

- Generation goes directly through OpenCode's native OpenAI Responses provider,
  with `store=false` and native compaction. There are no inference hooks, retries,
  prompt/response logging, or JavaScript routing proxy.
- Catalog GETs use the native connection's bearer, refuse redirects, have a
  15-second deadline and a 2 MiB metadata bound. Only model limits, modalities,
  reasoning options and names are imported; remote executable/endpoint overlays
  are ignored. New connection inventories replace rather than merge old ones.
- Passive dashboard polling runs once per desktop app every 30 seconds. It uses
  `exr doctor --json`, **not** live `limits`, so it cannot request weekly activation
  or redeem credits. Cached quota may be stale/unknown. The router's independent
  automatic metadata/reset worker remains unchanged.
- Usage loads on demand. Model/user totals are shared; token grouping is scoped
  to the native authenticated user. Unknown counters stay unknown. Known token
  totals can be incomplete when unknown/partial requests exist.
- Native reads have a 50-second deadline, a 1 MiB output bound and at most two
  concurrent subprocesses. Cancellation kills
  the read's process group. Child stderr and arbitrary errors are never shown.
- Native actions have a separate closed, typed stdio protocol with 64 KiB input
  frames, 1 MiB replies, one request in flight and a 55-second deadline (ten
  minutes for an explicit update install). There is no shell and no automatic
  mutation retry. Secrets stay in Rust; failed clipboard delivery can be retried
  against the same in-memory secret. Closing a dialog does not cancel issuance.
- Rotation, revocation, account deactivation, connection saving and installation
  require a user submission/confirmation. OAuth begins only from its explicit
  action and is cancelled when its dialog closes. Server-side identity and
  operator restrictions remain enforced by the shared native implementation.
- The panel describes the configured native profile, **not** the upstream account
  of the current session. No quota percentages are added across subscriptions.
- The GUI and native bridge run only on the local desktop. For remote OpenCode
  sessions, install/configure the server plugin and credentials there separately;
  the local panel is explicitly marked as a local-profile view.

Operator administration, manual account ranking/redemption and automatic
installation of a token into another client are not added. No public ExetRouter
management HTTP API is exposed; ordinary CLI terminal/secret guards are intact.

## Native package smoke test

After building, an optional isolated test packs and installs the package, then
loads it in an exact OpenCode 2.2.0 binary against a synthetic loopback catalog:

```sh
EXETROUTER_OPENCODE_PLUGIN_BIN=/absolute/path/to/opencode npm test
```

It does not use real accounts, the normal OpenCode profile, inference or reset
credits. npm installation in the test is offline and needs dependencies already
cached by `npm ci`. The regular tests use only synthetic data and loopback HTTP.
The native test does not verify the rendered Electron UI or renew ExetRouter's
full inference compatibility matrix.
