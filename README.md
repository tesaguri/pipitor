# Pipitor

A Twitter bot that gathers Tweets from a specified set of accounts, filters and Retweets them.

## Features

- Pipitor uses the `POST statuses/filter` API. It can get the Tweets in realtime without worrying being rate limited.
- Pipitor optionally supports getting past Tweets from a list, so it can even retrieve Tweets posted while it was suspended.

## Usage

Run the following to install (requires Nightly Rust):

```shell
cargo install pipitor
```

Create a manifest file named `Pipitor.toml` in the working directory.
Manifest format is shown in [`Pipitor.example.toml`](Pipitor.example.toml).

Then, run the following, and follow the instructions on the command line:

```shell
pipitor setup
```

Now, you're all set! Run the following to start the bot:

```shell
pipitor run
```
