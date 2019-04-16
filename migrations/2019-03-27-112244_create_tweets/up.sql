CREATE TABLE tweets (
  id INTEGER NOT NULL PRIMARY KEY,
  text TEXT NOT NULL,
  user_id INTEGER NOT NULL,
  in_reply_to_status_id INTEGER,
  quoted_status_id INTEGER
)
