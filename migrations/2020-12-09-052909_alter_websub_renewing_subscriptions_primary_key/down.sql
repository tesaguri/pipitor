CREATE TABLE websub_renewing_subscriptions_new (
    old BIGINT NOT NULL REFERENCES websub_active_subscriptions(id) ON DELETE CASCADE,
    new BIGINT NOT NULL REFERENCES websub_pending_subscriptions(id) ON DELETE CASCADE,
    PRIMARY KEY (old, new)
);

INSERT INTO websub_renewing_subscriptions_new (old, new)
    SELECT old, new FROM websub_renewing_subscriptions;
DROP TABLE websub_renewing_subscriptions;
ALTER TABLE websub_renewing_subscriptions_new RENAME TO websub_renewing_subscriptions;
