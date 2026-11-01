-- Numeric subscription observations only; one latest sample per account/window/hour.
CREATE TABLE IF NOT EXISTS oauth_quota_history (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('primary','secondary')),
    hour INTEGER NOT NULL CHECK(hour >= 0 AND hour % 3600 = 0),
    observed_at INTEGER NOT NULL CHECK(observed_at >= hour AND observed_at < hour + 3600),
    used_percent REAL NOT NULL CHECK(used_percent >= 0),
    window_minutes INTEGER CHECK(window_minutes IS NULL OR window_minutes > 0),
    reset_at INTEGER CHECK(reset_at IS NULL OR reset_at >= 0),
    request_order INTEGER NOT NULL,
    PRIMARY KEY(account_id, kind, hour)
);
CREATE INDEX IF NOT EXISTS oauth_quota_history_hour ON oauth_quota_history(hour);
