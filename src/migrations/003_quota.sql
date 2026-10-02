ALTER TABLE oauth_accounts ADD COLUMN cooldown_until INTEGER;
ALTER TABLE oauth_accounts ADD COLUMN cooldown_source TEXT;
CREATE TABLE oauth_quota_windows (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
    kind TEXT NOT NULL CHECK(kind IN ('primary','secondary')),
    used_percent REAL NOT NULL CHECK(used_percent >= 0),
    window_minutes INTEGER,
    reset_at INTEGER,
    observed_at INTEGER NOT NULL,
    request_order INTEGER NOT NULL,
    PRIMARY KEY(account_id,kind)
);
