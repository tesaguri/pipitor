# Configuration format

Before using Pipitor, you need to make a configuration file to describe its
behavior.

You can write the configuration in TOML, JSON or Dhall (optional
behind `dhall` feature) formats. The configuration file should be named
`Pipitor.toml`, `Pipitor.json` or `Pipitor.dhall` respectively.

Here is a cheatsheet of the configuration items along with their default values
(except for mandatory ones) in TOML format:

```toml
database_url = "pipitor.sqlite3"
skip_duplicate = false

[websub] # optional
callback = "https://your-domain.example/websub/" # required if `[websub]` exists
bind = "tcp://0.0.0.0:8080" # default: uses `$LISTEN_FDS`
renewal_margin = 3600

[twitter]
user = 12345 # required
stream = true

[twitter.client]
identifier = "Your application's API key"
secret = "Your application's API secret"

[twitter.list] # optional
id = 12345 # required if `[twitter.list]` exists
interval = 1
delay = 1

[[rule]]
topics = [
    "https://example.com/feed",
    12345,
] # required
filter = "foo" # default: matches all entries
exclude = "bar" # default: does not match any entry
```

Following sections are detailed explanation of each item.

## `database_url`

_Optional_. Path to place the database file. If unspecified, Pipitor will
look for `$DATABASE_URL` value in the `.env` file or the environment variable,
and then falls back on `./pipitor.sqlite3`.

## `skip_duplicate`

_Optional_. Whether to skip broadcasting _duplicate_ entries.

A _duplicate_ entry is an entry with exactly same content as an entry
broadcasted in the past. For example, if an account Tweets "hello" and Pipitor
Retweets that Tweet and then the account Tweets "hello" again, the second
"hello" Tweet is considered a duplicate.

Defaults to `false` (broadcast all entries).

## `websub`

_Optional_. Configuration of the WebSub subscriber server.

### `websub.callback`

URI to use as the prefix of callback URIs of the subscriber server.

If you set this to `https://example.com/websub/` for example, the callback URI
will look like `https://example.com/websub/UWfFAW_8wUQ`.

### `websub.bind`

_Optional_. Bind address of the subscriber server.

This takes an internet socket address like `tcp://127.0.0.1:8080` or a path to Unix domain socket like `unix:///path/to/socket`.

If unspecified, Pipitor will attempt to use `$LISTEN_FDS`.

### `websub.renewal_margin`

_Optional_. `Duration` between expiration time of subscriptions and timing that
the subscriber server attempts to renew the subscriptions.

For example, if a subscription is going to expire at `2020-01-01T12:00:00Z` and
`renewal_margin` is set to one hour, the subscriber server will send a renewal
request of the subscription to the hub at `2020-01-01T11:00:00Z`.

A `Duration` value is a record like `{ secs = 1, nanos = 500000000 }` or a single number which is a shorthand for `{ secs = .., nanos = 0 }`. `secs` is a
number of seconds of the duration and `nanos` is additional nanoseconds to the
duration.

Defaults to 1 hour.

## `twitter`

### `twitter.client`

OAuth API key and secret of the Twitter App.

This takes a record like `{ identifier = "API key", secret = "API secret" }`.

### `twitter.user`

User ID of a Twitter account to retrieve Tweets as.

Thi account is used to make List and streaming API requests.

### `twitter.stream`

_Optional_. Whether to use the streaming API.

Defaults to `true`.

### `twitter.list`

_Optional_.

#### `twitter.list.id`

ID of a List to retrieve Tweets from.

Running the `pipitor twitter-list-sync` command fills the List with
the accounts in `rule[].topics` list of the configuration.

#### `twitter.list.interval`

_Optional_. `Duration` (`websub.renewal_margin`) of intervals between API
requests to retrieve Tweets from the List.

Defaults to 1 second.

#### `twitter.list.delay`

_Optional_. The maximum `Duration` of time to subtract from `since_id`
parameter of API requests.

When retrieving Tweets from a List, the bot sets the `since_id` parameter to the
largest Tweet ID the bot has received until then, in order to reduce bandwidth
usage (without this, the bot would end up getting 200 Tweets every second, most
of which are duplicate).

However, the Tweet IDs are not completely sorted in chronological order.
Instead, they are _roughly sorted_, and new Tweets with IDs smaller than the
said `since_id` may appear after the last request. To catch such Tweets,
the bot may subtract a small amount of time from `since_id` like the following:
`since_id := clamp(time2sf(last_retrieved - delay), time2sf(sf2time(since_id) - delay), since_id)`,
where `last_retrieved` is the time of the last request, `time2sf()` is a
Snowflake ID corresponding to the given time and `sf2time()` is its reverse.

For further details, see the article
_[Your Timelines Are Leaky: A Slipperiness of Twitter's Snowflake IDs and `since_id` ❄️][leaky-snowflake]_.
The configuration option corresponds to $k$ value in the article.

[leaky-snowflake]: <https://github.com/tesaguri/leaky-snowflake/blob/main/README.md>

Defaults to 1 second.

## `rule`

A list of rules to describe the topics to retrieve entries from, the entries to
be broadcasted and the bot accounts to broadcast them.

### `rule[].topics`

A list of user IDs of accounts to retrieve Tweets of and URIs of
WebSub topics to retrieve entries from.

### `rule[].outbox`

A list of user IDs of accounts to broadcast the entries as.

While you can specify multiple Twitter accounts as `outbox` of a single `rule`,
it is advised to review Twitter's [automation rules] before doing so.

[automation rules]: https://help.twitter.com/en/rules-and-policies/twitter-automation

### `rule[].filter`

_Optional_. Regular expression filter to match entries to be broadcasted.

This takes a record like `{ title = "..", text = ".." }` (where `text` is
optional) or a single text which is a shorthand for `{ title = ".." }`. `title`
will match against the title of feed entries and body of Tweets. `text` will
match against the content and summary of feed entries.

If unspecified, all entries from the `topic`s will be broadcasted.

### `rule[].exclude`

_Optional_. Regular expression to filter out entries that have matched `filter`.

The format of values is the same as `filter`.
