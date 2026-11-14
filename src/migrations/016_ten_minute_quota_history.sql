-- Preserve actual observations, without inventing samples for the other buckets.
CREATE TABLE oauth_quota_history_new (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('primary','secondary')),
    hour INTEGER NOT NULL CHECK(hour >= 0 AND hour % 600 = 0),
    observed_at INTEGER NOT NULL CHECK(observed_at >= hour AND observed_at < hour + 600),
    used_percent REAL NOT NULL CHECK(used_percent >= 0),
    window_minutes INTEGER CHECK(window_minutes IS NULL OR window_minutes > 0),
    reset_at INTEGER CHECK(reset_at IS NULL OR reset_at >= 0),
    request_order INTEGER NOT NULL,
    PRIMARY KEY(account_id, kind, hour)
);
INSERT INTO oauth_quota_history_new
    SELECT account_id,kind,observed_at/600*600,observed_at,used_percent,window_minutes,reset_at,request_order
    FROM oauth_quota_history;
DROP TABLE oauth_quota_history;
ALTER TABLE oauth_quota_history_new RENAME TO oauth_quota_history;
CREATE INDEX oauth_quota_history_hour ON oauth_quota_history(hour);
