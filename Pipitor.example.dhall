-- let Pipitor = https://raw.githubusercontent.com/tesaguri/pipitor/dhall-schema-v0.3.0-alpha.8/schema.dhall sha256:00a2c768b7e5a739ed17ef82c947405965f3b3010c9f408ed4e80b8744166e9b
let Pipitor = ./schema.dhall

let botUserId = 12345

in
Pipitor.Manifest::{
    twitter = Pipitor.Twitter::{
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