-- Operator-owned billing metadata, independent of OAuth expiry and quota resets.
CREATE TABLE IF NOT EXISTS account_subscriptions (
    account_id INTEGER PRIMARY KEY REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    billing_day INTEGER CHECK (billing_day BETWEEN 1 AND 31),
    plan_usd INTEGER CHECK (plan_usd BETWEEN 1 AND 100000),
    api_budget_cents INTEGER CHECK (api_budget_cents BETWEEN 0 AND 1000000000)
);

-- Admin credentials are independent of OAuth. This table does not authorize
-- Platform inference, create keys, or treat an allowance as verified credit.
CREATE TABLE IF NOT EXISTS account_platform_credentials (
    account_id INTEGER PRIMARY KEY REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    encrypted_admin_key BLOB NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    configured_at INTEGER NOT NULL
);
