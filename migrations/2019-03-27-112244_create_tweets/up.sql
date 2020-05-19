CREATE TABLE tweets (
  id INTEGER NOT NULL PRIMARY KEY,
  text TEXT NOT NULL,
  user_id BIGINT NOT NULL,
  in_reply_to_status_id BIGINT,
  quoted_status_id BIGINT
)
