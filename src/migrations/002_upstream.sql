CREATE TABLE oauth_accounts (
    id INTEGER PRIMARY KEY,
    account_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('active','disabled','reauth_required')),
    encrypted_credentials BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    refresh_owner TEXT,
    refresh_until INTEGER,
    catalog_updated_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE TABLE oauth_models (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
    model TEXT NOT NULL,
    display_name TEXT NOT NULL,
    PRIMARY KEY(account_id, model)
);
ALTER TABLE usage_events ADD COLUMN request_id TEXT;
ALTER TABLE usage_events ADD COLUMN account_id INTEGER REFERENCES oauth_accounts(id);
ALTER TABLE usage_events ADD COLUMN client_transport TEXT;
ALTER TABLE usage_events ADD COLUMN upstream_transport TEXT;
ALTER TABLE usage_events ADD COLUMN upstream_request_id TEXT;
ALTER TABLE usage_events ADD COLUMN upstream_response_id TEXT;
ALTER TABLE usage_events ADD COLUMN duration_ms INTEGER;
CREATE UNIQUE INDEX usage_request_id ON usage_events(request_id) WHERE request_id IS NOT NULL;
