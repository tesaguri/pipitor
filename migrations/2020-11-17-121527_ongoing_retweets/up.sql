CREATE TABLE ongoing_retweets (
    id INTEGER NOT NULL REFERENCES tweets(id) ON DELETE CASCADE,
    user INTEGER NOT NULL REFERENCES twitter_tokens(id) ON DELETE CASCADE,
    PRIMARY KEY (id, user)
);
