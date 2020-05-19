CREATE TABLE entries (
    topic TEXT NOT NULL,
    id TEXT,
    link TEXT NOT NULL,
    title TEXT,
    summary TEXT,
    content TEXT,
    updated BIGINT,
    PRIMARY KEY (topic, id)
);
