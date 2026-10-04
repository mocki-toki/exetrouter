# Changelog

## 0.2.2

- Select individual models, users or API tokens with recorded usage in the dashboard; user totals include all of their tokens, while token IDs remain scoped to the authenticated user.
- Default Usage charts to reported tokens and draw clearer lines with readable upper axis limits rounded upward.
- Keep the selected item in the chart legend, including full token IDs on small terminals; reserve the combined view for total grouping.
- Show keyboard help for the current dashboard tab.
- Simplify OpenCode V1/V2 configuration to the built-in OpenAI provider, removing the optional custom plugins.

## 0.2.1

- Simplify OpenCode V1/V2 setup to the built-in OpenAI provider with a router endpoint and token; remove the custom client plugins and retain standard OpenCode retries.

- Put Linux binary installation first in the README, followed by macOS Homebrew and source builds; give shared-server deployment its own section.

## 0.2.0

- Add a binary-only macOS Homebrew tap for exr, update its checksums from release artifacts, and preserve Homebrew ownership during client updates.

- Publish versioned Linux AMD64/ARM64 container images alongside native releases.
- Make Docker Compose the primary installation, administration and update interface; bootstrap directly with the container and document the separate native SSH gateway.
- Keep the host launcher optional and change its updater to pull images, pin digests, compare the live backup schema and validate binary versions before switching.
- Preserve legacy host-network overrides and health rollback without building Rust on the operator host.

## 0.1.3

- Fix Docker updates incorrectly refusing the existing seven-migration schema; future schema changes still require a planned operator upgrade.

## 0.1.2

- Exclude generated Python bytecode from Cargo packages.

## 0.1.1

- Add usage grouping by API token in the CLI and dashboard, scoped to the authenticated user and preserving revoked-token history and unknown usage.

## 0.1.0

- Initialize the consolidated ExetRouter source history.
- Include standalone and shared routing, OAuth account pools, Responses and limited text Chat Completions.
- Include account preferences, operator policy, contextual dashboard actions and bounded 16 MiB inference requests.
- Add bounded automatic quota account failover with full-context recovery, preserved tool results and user-scoped portability for concurrent forks.
