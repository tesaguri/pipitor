CREATE TABLE websub_renewing_subscriptions (
    old BIGINT NOT NULL REFERENCES websub_active_subscriptions(id) ON DELETE CASCADE,
    new BIGINT NOT NULL REFERENCES websub_pending_subscriptions(id) ON DELETE CASCADE,
    PRIMARY KEY (old, new)
);
