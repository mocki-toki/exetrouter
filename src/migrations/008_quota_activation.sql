CREATE TABLE quota_activation_attempts (
 id INTEGER PRIMARY KEY,
 account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
 user_id INTEGER NOT NULL REFERENCES users(id),
 attempted_at INTEGER NOT NULL,
 reset_at INTEGER NOT NULL,
 model TEXT NOT NULL CHECK(model='gpt-5.6-sol'),
 status TEXT NOT NULL CHECK(status IN ('pending','completed','incomplete','rejected','unknown')),
 input_tokens INTEGER CHECK(input_tokens>=0),
 output_tokens INTEGER CHECK(output_tokens>=0),
 cached_input_tokens INTEGER CHECK(cached_input_tokens>=0),
 reasoning_output_tokens INTEGER CHECK(reasoning_output_tokens>=0)
);
CREATE INDEX quota_activation_account_time ON quota_activation_attempts(account_id,attempted_at);
CREATE INDEX quota_activation_time ON quota_activation_attempts(attempted_at);
