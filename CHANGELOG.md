# Changelog

## Unreleased

- Encrypt native-client token names and personal account labels, notes, project names and dashboard preferences with an automatically created client-held key. Add bounded user-scoped ciphertext storage, automatic migration of existing token names, and the schema-9-to-10 migration/rollback plan.

## 0.4.0

- Configure soft per-account switching thresholds for all, short and weekly quota windows through exr, exrd and the dashboard. Prefer alternatives at or below the remaining-percentage threshold, with fallback to healthy capacity and safe context transfer before submission.
- Replace the dashboard priority toggle with numeric priority -255…255 (default 1), inheritance and operator locks. Preserve stored priorities and credentials through the explicit schema-8-to-9 migration.
- Keep switching thresholds in the account settings form, edit priority there, and hide completed weekly-activation notices from ordinary account views.
- Service upstream and client WebSocket ping/pong, detect silent pre-response peers after 90 seconds, and retire recoverable upstream sockets between requests after 30 idle seconds or five minutes of age. Preserve live slow generations and same-account context recovery.
- Correlate client and upstream WebSocket lifecycle, request timing and interruptions using content-free IDs, fixed reasons and numeric liveness counters.
- Leave interrupted inference retry and transport fallback policy to clients instead of overriding native client defaults or wrapping WebSocket interruptions as HTTP 400 errors.
- Publish schema-9 native gateway and container artifacts together. Existing schema-8 installations require the reviewed account-routing migration rather than an automatic packaging update.

## 0.3.0

- Export dedicated OpenCode V1/V2 providers with pool models, native metadata and connection settings; reuse the standalone or saved remote API URL. Matching authenticated model routes expose provider fragments.
- Forward backend request options without a static capability denylist and translate Chat output caps. Preserve backend HTTP statuses with redacted HTTP/Chat errors, resource bounds and adapter checks; enforce user ownership and account pinning for saved conversation references.
- Activate apparently inactive weekly windows with one durably guarded minimal gpt-5.6-sol request; retain nullable counters in user/model/total usage and expose the attempt status. Requires the explicit schema-7-to-8 migration plan.
- Support Ctrl-C cancellation throughout connection setup without saving edits.
- Add a visible cursor and editing within connection fields, including Unicode and long paths.
- Check remote SSH settings before saving and allow verified first-time host-key confirmation in the same terminal.
- Add source-pinned Nix flake installation and development support.
- Simplify documentation, separate client configuration from launch and provide a dedicated Docker/native server installation guide.

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
