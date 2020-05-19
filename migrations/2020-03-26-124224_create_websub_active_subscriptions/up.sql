CREATE TABLE websub_active_subscriptions (
    id INTEGER NOT NULL PRIMARY KEY REFERENCES websub_subscriptions(id) ON DELETE CASCADE,
    expires_at BIGINT NOT NULL
);
