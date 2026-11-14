-- Operator-entered reconciliation points, not provider-confirmed cash balances.
-- Replacement is a new epoch; delayed observations must match its random ID.
CREATE TABLE IF NOT EXISTS platform_balance_anchors (
    account_id INTEGER PRIMARY KEY REFERENCES account_platform_credentials(account_id) ON DELETE CASCADE,
    epoch TEXT NOT NULL,
    generation INTEGER NOT NULL,
    entered_at INTEGER NOT NULL,
    initial_usd_nanos INTEGER NOT NULL CHECK(initial_usd_nanos>=0),
    usage_after_id INTEGER NOT NULL,
    organization_spent_nanos INTEGER,
    organization_to_utc INTEGER,
    organization_observed_at INTEGER
);
CREATE TABLE IF NOT EXISTS platform_request_costs (
    usage_id INTEGER PRIMARY KEY REFERENCES usage_events(id) ON DELETE CASCADE,
    usd_nanos INTEGER NOT NULL CHECK(usd_nanos>=0)
);
CREATE INDEX IF NOT EXISTS usage_platform_balance ON usage_events(account_id,id) WHERE upstream_transport IN ('platform_http_sse','platform_http_json');
