# Usage Guide

## What `mailman` Does

`mailman` is a terminal app for Gmail.

Right now, these commands work:

- `mailman` opens the inbox terminal UI
- `init` creates your config file
- `auth` signs you into Google in the browser
- `inbox` lists recent inbox messages

These commands exist but are not fully implemented yet:

- `read`
- `send`

## Before You Start

The app needs Google OAuth credentials from the developer of the app.

If you are the developer, set those values in the config file first. If you are just using the tool, you should normally receive a preconfigured build or config from the developer.

## Config File

Create the default config:

```bash
cargo run -- init
```

On Linux, this creates:

```text
~/.config/mailman/config.toml
```

Expected config format:

```toml
[gmail]
account_email = "your@gmail.com"
client_id = "YOUR_CLIENT_ID.apps.googleusercontent.com"
client_secret = "YOUR_CLIENT_SECRET"
redirect_url = "http://127.0.0.1:8080"
token_store = "tokens.json"
```

## Sign In

Run:

```bash
cargo run -- auth
```

What happens:

1. Your browser opens to Google sign-in.
2. You approve Gmail access.
3. The browser redirects back to `127.0.0.1:8080`.
4. `mailman` stores your tokens locally.

If the browser does not open automatically, the command prints a URL you can open manually.

## View Your Inbox

List recent messages:

```bash
cargo run -- inbox
```

List a different number of messages:

```bash
cargo run -- inbox --limit 20
```

The inbox output includes:

- Gmail message id
- message date
- subject
- sender

## Launch the TUI

Open the inbox viewer:

```bash
cargo run
```

TUI controls:

- `j` or Down Arrow moves down
- `k` or Up Arrow moves up
- `g` jumps to the top
- `G` jumps to the bottom
- `q` or `Esc` quits

## Current Limitations

- `read` is not connected to the real Gmail API yet.
- `send` is not connected to the real Gmail API yet.
- The app currently focuses on a developer-run local workflow rather than packaged distribution.

## Troubleshooting

### Google says access is blocked

If you see `403: access_denied` and a message saying the app is still being tested:

- the Google account must be added as a test user in Google Cloud
- or the app must be published and verified for broader access

### Redirect issues

Use:

```toml
redirect_url = "http://127.0.0.1:8080"
```

`127.0.0.1` is the expected local callback host for the current implementation.

### No token found

If `inbox` says no OAuth token was found, run:

```bash
cargo run -- auth
```

If `mailman` exits immediately with an auth-related error, authenticate first and then launch it again.

## Security Notes

- Do not commit your `client_secret` or token files to git.
- Tokens are stored locally on the machine running `mailman`.
- Anyone with access to those files may be able to use the connected Gmail account until access is revoked.
