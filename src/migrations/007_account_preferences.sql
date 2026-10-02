CREATE TABLE account_policy (
 account_id INTEGER PRIMARY KEY REFERENCES oauth_accounts(id) ON DELETE CASCADE,
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN -100 AND 100),
 locked INTEGER NOT NULL DEFAULT 0 CHECK(locked IN (0,1))
);
CREATE TABLE account_preferences (
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 account_id INTEGER NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
 enabled INTEGER CHECK(enabled IN (0,1)),
 priority INTEGER CHECK(priority BETWEEN -100 AND 100),
 PRIMARY KEY(user_id,account_id)
);
