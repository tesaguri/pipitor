# Pipitor

[![Build Status](https://github.com/tesaguri/pipitor/workflows/CI/CD/badge.svg)](https://github.com/tesaguri/pipitor/actions)
[![crates.io](https://img.shields.io/crates/v/pipitor.svg)](https://crates.io/crates/pipitor)

Pipitor is a Twitter bot that listens on WebSub/Twitter streams and (re)Tweet
the updates in real time.

## Overview

Pipitor gathers Atom/RSS feed entries from [WebSub] (PubSubHubbub) hubs or
Tweets from a set of Twitter accounts and (re)Tweets the updates in real time.

[WebSub]: https://en.wikipedia.org/wiki/WebSub

You can optionally specify regular expression patterns to filter the updates to
(re)Tweet. The regular expression syntax used by Pipitor is documented by
the [regex] crate.

[regex]: https://docs.rs/regex/1/regex/#syntax

Pipitor uses the streaming API ([`POST statuses/filter`]) to retrieve Tweets,
which has quite small impact on the rate limit. The rate limit bucket of
`POST statuses/filter` is consumed only once on startup of the bot.

[`POST statuses/filter`]: https://developer.twitter.com/en/docs/twitter-api/v1/tweets/filter-realtime/overview

The streaming API, however, is not complete (it misses Tweets on rare occasions)
nor extremely fast (there can be a latency of around 5 seconds), so Pipitor also
provides an ability to retrieve Tweets from a List. When the List is enabled,
Pipitor can deliver a Retweet within a little more than a second of the original
Tweet being posted, at the expense of [`GET lists/statuses`]'s rate limit being
exhausted. In addition, when the bot is suspended for some reason, it can
retrieve Tweets posted while its suspension from the List on later restart.

[`GET lists/statuses`]: https://developer.twitter.com/en/docs/twitter-api/v1/accounts-and-users/create-manage-lists/api-reference/get-lists-statuses

## Usage

Download the latest binary package for your platform from the [releases] page
and install it to a directory of your choice.

[releases]: https://github.com/tesaguri/pipitor/releases

Or alternatively, you can manually build the project from the source:

```shell
cargo install pipitor
```

After the installation, create a configuration file named `Pipitor.toml` in the
working directory. You can use JSON and Dhall (via disabled-by-default `dhall`
feature) formats as well (`Pipitor.json` and `Pipitor.dhall` respectively).
The config format is documented in [`CONFIG.md`](CONFIG.md) and example configs
are shown in [`Pipitor.example.toml`](Pipitor.example.toml) and
[`Pipitor.example.dhall`](Pipitor.example.dhall).

Then, run the following, and follow the instructions on the command line.
This is needed to retrieve the API credentials for the bot.

```shell
pipitor setup
```

Now, you're all set! Run the following to start the bot:

```shell
pipitor run
```

## License

This project is licensed under the GNU Affero General Public License, Version 3
([LICENSE](LICENSE) or https://www.gnu.org/licenses/agpl-3.0.html) unless explicitly stated otherwise.
