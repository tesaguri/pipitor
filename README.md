# Pipitor

A Twitter bot that gathers Tweets from a specified set of accounts, filters and Retweets them.

## Features

- Pipitor uses the `POST statuses/filter` API. It can get the Tweets in realtime without worrying being rate limited.
- Pipitor optionally supports getting past Tweets from a list, so it can even retrieve Tweets posted while it was suspended.

## Usage

Download the latest binary package for your platform from the [releases](https://github.com/tesaguri/pipitor/releases) page
and install it to a directory of your choice.

Or alternatively, you can manually build the project from the source (requires Nightly Rust):

```shell
cargo install pipitor
```

After the installation, create a manifest file named `Pipitor.toml` in the working directory.
Manifest format is shown in [`Pipitor.example.toml`](Pipitor.example.toml).

Then, run the following, and follow the instructions on the command line:

```shell
pipitor setup
```

Now, you're all set! Run the following to start the bot:

```shell
pipitor run
```

## License

This project is licensed under the GNU Affero General Public License, Version 3 ([LICENSE](LICENSE) or https://www.gnu.org/licenses/agpl-3.0.html).

The CI configuration (`.travis.yml` and `scripts/*`) was generated with [crossgen](https://github.com/yoshuawuyts/crossgen),
based on [trust](https://github.com/japaric/trust) template written by Jorge Aparicio ([@japaric](https://github.com/japaric)).
The trust template is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/japaric/trust/blob/v0.1.2/LICENSE-APACHE) or
  https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](https://github.com/japaric/trust/blob/v0.1.2/LICENSE-MIT) or
  https://opensource.org/licenses/MIT)

at your option.
