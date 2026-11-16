-- Costs are monetary observations, not free-credit balances or admission permits.
CREATE TABLE IF NOT EXISTS platform_policy (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    codex_fallback_allowed INTEGER NOT NULL DEFAULT 0 CHECK(codex_fallback_allowed IN (0,1))
);
INSERT OR IGNORE INTO platform_policy(singleton) VALUES(1);
CREATE TABLE IF NOT EXISTS platform_settings (
    account_id INTEGER PRIMARY KEY REFERENCES account_platform_credentials(account_id) ON DELETE CASCADE,
    project_id TEXT,
    manage_keys INTEGER NOT NULL DEFAULT 0 CHECK(manage_keys IN (0,1))
);
CREATE TABLE IF NOT EXISTS platform_refresh (
    account_id INTEGER PRIMARY KEY REFERENCES account_platform_credentials(account_id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    owner TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    observed_at INTEGER,
    costs_to_utc INTEGER,
    error TEXT
);
CREATE TABLE IF NOT EXISTS platform_cost_history (
    account_id INTEGER NOT NULL REFERENCES account_platform_credentials(account_id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    day INTEGER NOT NULL,
    end_at INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    usd_nanos INTEGER NOT NULL CHECK(usd_nanos >= 0),
    PRIMARY KEY(account_id,day)
);
CREATE TABLE IF NOT EXISTS platform_working_keys (
    account_id INTEGER PRIMARY KEY REFERENCES account_platform_credentials(account_id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    service_account_id TEXT NOT NULL,
    key_id TEXT NOT NULL,
    encrypted_key BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_used_at INTEGER,
    checked_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active','expired','inactive','missing'))
);
-- An uncertain mutation survives Admin Key removal/replacement and restart.
-- Its secret may have been lost. Never POST again just because a list is empty.
CREATE TABLE IF NOT EXISTS platform_key_operations (
    account_id INTEGER PRIMARY KEY REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    owner TEXT NOT NULL,
    attempted_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('submitted','unknown','confirmed'))
);
