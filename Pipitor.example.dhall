let Pipitor =
      https://github.com/tesaguri/pipitor/raw/dhall-package-v0.3.0-alpha.14/package.dhall sha256:7cda9e784009d372f4b6272be2487647cfa02587faceb2c746b94927c4dec3fc
let botUserId = 12345

let manifest = Pipitor.Manifest::{
    websub = Some Pipitor.WebSub::{
        callback = "https://your-domain.example/websub/",
        bind = Some "tcp://0.0.0.0:8080",
    },
    twitter = Some Pipitor.Twitter::{
        client = {
            identifier = "Your application's API key",
            secret = "Your application's API secret",
        },
        -- `user_id` of the account used to retrieve Tweets
        user = botUserId,
        list = Some Pipitor.TwitterList::{
            -- `list_id` of the list used to backfill timeline
            -- Running `pipitor twitter-list-sync` will overwrite members in the list.
            id = 123456,
        },
    },
    rule = [
        Pipitor.Rule::{
            topics = [
                -- `user_id`s of accounts to retrieve Tweets from
                Pipitor.Topic.Twitter 783214, -- @Twitter
                Pipitor.Topic.Twitter 12, -- @jack
                -- ... or Atom/RSS feed URLs.
                Pipitor.Topic.Feed "https://wordpress.com/blog/feed/atom/",
            ],
            -- If `filter` is unspecified, the rule will match any Tweet.
            filter = Some Pipitor.Filter::{ title="regex|to|filter the|Tweets" },
            -- `user_id`s of the accounts to Retweet filtered Tweets.
            outbox = [Pipitor.Outbox.Twitter botUserId],
        },
        {
            topics = [
                Pipitor.Topic.Twitter 17874544, -- @TwitterSupport
            ],
            filter = Some Pipitor.Filter::{ title = "(?i)Another filter|This regex is case insensitive" },
            exclude = Some Pipitor.Filter::{ title = "excluding Tweets that match this regex" },
            -- `outbox` can be empty, in which case the matched Tweets won't be Retweeted.
            outbox = [] : List Pipitor.Outbox,
        },
    ],
}

let _ = assert : Pipitor.Manifest/validate manifest === True

in manifest
