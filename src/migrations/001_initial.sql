CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS access_tokens (
  id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id),
  name TEXT NOT NULL, digest BLOB NOT NULL UNIQUE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  last_used_at INTEGER, revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS tokens_user ON access_tokens(user_id);
CREATE TABLE IF NOT EXISTS ssh_identities (
  id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id),
  fingerprint TEXT NOT NULL UNIQUE, public_key TEXT NOT NULL,
  created_at INTEGER NOT NULL, revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS ssh_identities_user ON ssh_identities(user_id);
CREATE TABLE IF NOT EXISTS usage_events (
  id INTEGER PRIMARY KEY, at_utc INTEGER NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id),
  token_id TEXT NOT NULL REFERENCES access_tokens(id),
  api_surface TEXT NOT NULL, model TEXT NOT NULL,
  status TEXT NOT NULL,
  input_tokens INTEGER, output_tokens INTEGER,
  cached_input_tokens INTEGER, reasoning_output_tokens INTEGER,
  CHECK (input_tokens IS NULL OR input_tokens >= 0),
  CHECK (output_tokens IS NULL OR output_tokens >= 0)
);
CREATE INDEX IF NOT EXISTS usage_time ON usage_events(at_utc);
CREATE INDEX IF NOT EXISTS usage_user_time ON usage_events(user_id, at_utc);
