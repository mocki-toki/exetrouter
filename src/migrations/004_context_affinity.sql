CREATE TABLE context_bindings (
    user_id INTEGER NOT NULL REFERENCES users(id),
    digest BLOB NOT NULL CHECK(length(digest)=32),
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(user_id,digest)
);
CREATE INDEX context_binding_expiry ON context_bindings(expires_at);
