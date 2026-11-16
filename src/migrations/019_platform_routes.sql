-- Source-qualified context identities. Never reuse a deleted route identifier:
-- its negative value is authenticated in client-carried ownership envelopes.
CREATE TABLE IF NOT EXISTS platform_routes (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id>0),
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1,
    credential_generation INTEGER NOT NULL,
    key_id TEXT NOT NULL,
    catalog TEXT NOT NULL DEFAULT '[]',
    observed_at INTEGER NOT NULL,
    UNIQUE(account_id,project_id)
);
CREATE TABLE IF NOT EXISTS platform_route_health (
    route_id INTEGER PRIMARY KEY REFERENCES platform_routes(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0,
    retry_at INTEGER NOT NULL DEFAULT 0,
    last_order INTEGER NOT NULL DEFAULT 0,
    quota_until INTEGER NOT NULL DEFAULT 0,
    quota_order INTEGER NOT NULL DEFAULT 0
);
