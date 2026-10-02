CREATE TABLE upstream_operation_clock (
    id INTEGER PRIMARY KEY CHECK(id=1),
    sequence INTEGER NOT NULL
);
INSERT INTO upstream_operation_clock VALUES(1,0);
CREATE TABLE oauth_health (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
    scope TEXT NOT NULL CHECK(scope IN ('catalog','refresh','responses')),
    generation INTEGER NOT NULL,
    failures INTEGER NOT NULL,
    retry_at INTEGER NOT NULL,
    reason TEXT NOT NULL,
    last_order INTEGER NOT NULL,
    PRIMARY KEY(account_id,scope)
);
CREATE TABLE oauth_auth_rejections (
    account_id INTEGER NOT NULL REFERENCES oauth_accounts(id),
    scope TEXT NOT NULL CHECK(scope IN ('catalog','responses')),
    generation INTEGER NOT NULL,
    last_order INTEGER NOT NULL,
    PRIMARY KEY(account_id,scope)
);
