# Pipitor

A Twitter bot that gathers Tweets from a specified set of accounts, filters and Retweets them.

## Features

- Pipitor uses the `POST statuses/filter` API. It can get the Tweets in realtime without worrying being rate limited.
- Pipitor optionally supports getting past Tweets from a list, so it can even retrieve Tweets posted while it was suspended.

## Usage

Run the following to install the required program:

```shell
cargo install diesel-cli
```

(If this fails, run `cargo install diesel_cli --no-default-features --features sqlite`.)

And run the following:

```shell
git clone https://github.com/tesaguri/pipitor
cd pipitor
diesel setup
```

Create a manifest file named `Pipitor.toml`. Manifest format is shown in [`Pipitor.example.toml`](Pipitor.example.toml).

Then, run the following:

```shell
cargo run --release -- twitter-login
# The following is only required only if you have set `twitter.list` in the manifest.
# You have to run this again everytime you modify `rule.topics` in the manifest.
cargo run --release -- twitter-list-sync
```

Now, you're all set! Run the following to start the bot:

```shell
cargo run --release -- run
```
