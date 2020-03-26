CREATE TABLE websub_pending_subscriptions (
    id INTEGER NOT NULL PRIMARY KEY REFERENCES websub_subscriptions(id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL DEFAULT (strftime('%s', 'now'))
);
