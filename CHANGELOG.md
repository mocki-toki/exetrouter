# Changelog

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
