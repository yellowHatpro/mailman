# Development Guide

## Purpose

`mailman` is a Rust terminal Gmail client. The project currently supports:

- config bootstrapping
- browser-based OAuth sign-in
- local token persistence
- real inbox listing from the Gmail API

The `read` and `send` commands are still scaffolded and need real Gmail API implementations.

## Stack

- Rust edition `2024`
- `tokio` for async runtime
- `clap` for CLI parsing
- `oauth2` for Google OAuth
- `reqwest` for Gmail API HTTP requests
- `serde` and `toml` for config and token serialization

## Repository Layout

```text
src/
  app.rs                top-level command dispatch
  cli.rs                CLI definitions
  config.rs             config loading and app paths
  gmail/
    client.rs           OAuth flow, token handling, Gmail API logic
    models.rs           app-facing models and stored token shape
    mod.rs
  main.rs               async entrypoint
```

## Local Setup

### 1. Install Rust

Use a current stable Rust toolchain.

### 2. Create the app config

```bash
cargo run -- init
```

This creates `config.toml` in the platform config directory.

Linux:

```text
~/.config/mailman/config.toml
```

### 3. Configure Google OAuth

In Google Cloud:

1. Create or select a project.
2. Enable `Gmail API`.
3. Configure the OAuth consent screen.
4. Create an OAuth client with type `Desktop app`.
5. If the app is in testing mode, add your Gmail account as a test user.

Then set these values in `config.toml`:

```toml
[gmail]
account_email = "your@gmail.com"
client_id = "YOUR_CLIENT_ID.apps.googleusercontent.com"
client_secret = "YOUR_CLIENT_SECRET"
redirect_url = "http://127.0.0.1:8080"
token_store = "tokens.json"
```

### 4. Authenticate locally

```bash
cargo run -- auth
```

This opens the browser, completes the OAuth flow, and stores tokens in the platform data directory.

## Development Commands

Format:

```bash
cargo fmt
```

Type-check and compile:

```bash
cargo check
```

Run the inbox command:

```bash
cargo run -- inbox
```

## Current Behavior

### Implemented

- `init`
- `auth`
- `inbox`

### Placeholder commands

- `read`
- `send`

These commands currently validate setup but do not yet perform real Gmail API operations.

## Implementation Notes

### OAuth model

- The app uses a desktop OAuth flow with PKCE.
- The redirect listener binds to the host and port from `redirect_url`.
- The current recommended redirect value is `http://127.0.0.1:8080`.

### Token storage

- Tokens are stored locally as JSON.
- The current implementation refreshes the access token using the stored refresh token before API requests.

### Gmail inbox implementation

- `inbox` calls `users/me/messages` with the `INBOX` label.
- It then fetches message metadata to display `From`, `Subject`, and `Date`.

## Next Development Tasks

1. Implement `read` with `users/me/messages/{id}` and payload decoding.
2. Implement `send` with RFC 2822 message construction and `users.messages.send`.
3. Add better token expiry tracking instead of refreshing on every call.
4. Add integration tests around config loading and Gmail response parsing.
5. Decide whether OAuth credentials should be loaded from config, environment, or a bundled credentials file for distribution.

## Distribution Considerations

- End users should not need to create their own Google Cloud project if you ship your OAuth client.
- End users still need to sign in with their Google account on first use.
- If the OAuth app remains in testing mode, every user must be added as a test user.
- Public distribution of Gmail-scoped apps may require Google verification.
