ALTER TABLE account_policy RENAME TO old_account_policy;
CREATE TABLE account_policy (
 account_id INTEGER PRIMARY KEY REFERENCES oauth_accounts(id) ON DELETE CASCADE,
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
 priority INTEGER NOT NULL DEFAULT 1 CHECK(priority BETWEEN -255 AND 255),
 locked INTEGER NOT NULL DEFAULT 0 CHECK(locked IN (0,1)),
 switch_at INTEGER NOT NULL DEFAULT -1 CHECK(switch_at BETWEEN -1 AND 100),
 switch_at_short INTEGER CHECK(switch_at_short BETWEEN -1 AND 100),
 switch_at_weekly INTEGER CHECK(switch_at_weekly BETWEEN -1 AND 100)
);
INSERT INTO account_policy(account_id,enabled,priority,locked)
 SELECT account_id,enabled,priority,locked FROM old_account_policy;
DROP TABLE old_account_policy;
ALTER TABLE account_preferences RENAME TO old_account_preferences;
CREATE TABLE account_preferences (
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 account_id INTEGER NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
 enabled INTEGER CHECK(enabled IN (0,1)),
 priority INTEGER CHECK(priority BETWEEN -255 AND 255),
 switch_at INTEGER CHECK(switch_at BETWEEN -1 AND 100),
 switch_at_short INTEGER CHECK(switch_at_short BETWEEN -1 AND 100),
 switch_at_weekly INTEGER CHECK(switch_at_weekly BETWEEN -1 AND 100),
 PRIMARY KEY(user_id,account_id)
);
INSERT INTO account_preferences(user_id,account_id,enabled,priority)
 SELECT user_id,account_id,enabled,priority FROM old_account_preferences;
DROP TABLE old_account_preferences;
