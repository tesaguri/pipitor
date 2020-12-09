CREATE TABLE websub_renewing_subscriptions_new (
    old INTEGER NOT NULL UNIQUE REFERENCES websub_active_subscriptions(id) ON DELETE CASCADE,
    new INTEGER NOT NULL PRIMARY KEY REFERENCES websub_pending_subscriptions(id) ON DELETE CASCADE
);

INSERT INTO websub_renewing_subscriptions_new (old, new)
    SELECT old, new FROM websub_renewing_subscriptions;
DROP TABLE websub_renewing_subscriptions;
ALTER TABLE websub_renewing_subscriptions_new RENAME TO websub_renewing_subscriptions;
