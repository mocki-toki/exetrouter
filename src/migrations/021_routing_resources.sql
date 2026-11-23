CREATE TABLE IF NOT EXISTS routing_resources (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    resources TEXT NOT NULL CHECK(resources IN ('codex','platform','both'))
);
