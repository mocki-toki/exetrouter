# VS Code integration

The [desktop extension](../clients/vscode/README.md) contributes the `exetrouter` Language Model Chat Provider and an Activity Bar dashboard. Build/install and connection instructions are in its README. It does not require an OpenAI-compatible client extension or a change to the router transport.

## Data paths

| Feature | Source |
| --- | --- |
| Chat picker and Models view | Authenticated `GET /v1/models`; limits, routes, reasoning efforts, tool and image capabilities come from the selected API endpoint. Non-chat inventory stays out of the picker. |
| Chat | `POST /v1/responses` with SSE, full ordered history and `store: false`. Each request has independent cancellation. The extension never replays a failed POST. |
| Overview and status bar | Local `exr doctor --json`, including every reported profile account. Hover or click to see accounts individually. Refreshed every minute. |
| Quota history | `exr quota-history ACCOUNT --period … --json`. The editor chart plots quota windows on a shared 0–100% axis and shows the Platform balance estimate when available. Missing observations remain gaps. |
| Usage | `exr usage --period … --json` for totals, or `--by model`, `--by user`, `--by token`. Plot tokens, requests, or estimated API-equivalent USD in the sidebar or editor. Missing counters remain unknown. |
| Tokens | `exr token list --json` loads metadata. Confirmed create/rename/pause/resume/rotate/revoke/Fast and clipboard retry/discard run through the private bridge. |
| Native profile | Private `exr desktop` profile action. The child remains attached for standalone mode, where it may host the listener. Changes to the native profile file invalidate cached reports and bridge identity. Extension preferences are in standard VS Code Settings. |

The profile bridge and passive reports reuse the same [native interface as the OpenCode dashboard](opencode-desktop.md). If the bridge is unavailable, set the user-scoped API base explicitly; older native clients may still provide individual reports. A failed doctor, token list or usage read retains the previous same-profile snapshot with its timestamp and a stale label. An empty successful report replaces it. Token actions re-read the current list and bind confirmation to the current native bridge; uncertain mutations are never retried automatically. Only `exr` handles newly issued secrets and clipboard recovery. Local reports and the HTTP catalog remain in memory. An explicit chat endpoint may differ from the native profile behind the dashboard.

Chart webviews use bundled SVG rendering and receive only formatted labels and numeric report data. They do not make network requests, receive API tokens, or expose a management message channel. Legends toggle individual series, and keyboard and pointer inspection report actual interval values.

## Compatibility and limits

The public declarations in installed VS Code **1.147.0**, revision `8a14f0cc1b08d531eb83255ea3764ae22aa932b5`, and the installed host provider dispatch were inspected. The build targets the public `@types/vscode` **1.146.0** API. It does not use proposed chat APIs. The stable interface does not expose active-chat account attribution, a response metadata channel or an encrypted reasoning round trip. Accordingly the status bar reports all profile accounts, reasoning summaries are not rendered and the extension does not request encrypted thinking. It does not substitute the routing recommendation for the current chat account.

Tool calls are reported to VS Code with their Responses `call_id`; VS Code owns execution. Function results, multiple calls and embedded image parts retain their order on the next full-history request. Unsupported content is rejected rather than silently dropped. No `max_output_tokens` is submitted. When the router does not report an output limit, the picker adapter supplies `0` and displays **Unknown**. Input headroom follows the router’s percentage/compaction projection. Local `o200k_base` counts estimate prompt budgeting; framing, image accounting and a model’s actual upstream tokenizer may differ. No router prices or per-request usage are invented.

The VS Code 1.147.0 Extension Development Host resolved a synthetic ExetRouter model through `vscode.lm`, returned a tool call, and completed a second request with its result against a loopback fixture. Offline tests also cover transport, credential isolation, passive reporting and account presentation. Live inference and individual built-in Chat harnesses were not verified.
